//! Durable reasons for drafting an already-approved revision; dispatch still belongs to admission::admit.
use super::*;

struct Request {
    project: String,
    spec: String,
    cause: String,
    note: Value,
}

fn requested(conn: &Connection, run: &str) -> Result<Option<Request>, PersistError> {
    let row: Option<(String, String, String)> = conn.query_row(
        "SELECT project_id,spec_id,mode FROM runs r WHERE run_id=?1 AND spec_id IS NOT NULL \
         AND state IN ('SPECIFYING','READY','RUNNING','VERIFYING') AND mode IN ('PLAN','AUTO') \
         AND NOT EXISTS(SELECT 1 FROM tasks t WHERE t.run_id=r.run_id AND kind='spec_draft' AND state NOT IN ('ACCEPTED','CANCELLED'))",
        [run], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sqlite_error)?;
    let Some((project, spec, mode)) = row else {
        return Ok(None);
    };
    let last: i64 = conn.query_row("SELECT coalesce(max(seq),0) FROM events WHERE run_id=?1 AND kind='spec.draft_requested'",[run],|r|r.get(0)).map_err(sqlite_error)?;
    // Only untargeted PLAN steer requests another generation (DC-T04). SendInput normalizes a questionless REPLY to STEER.
    // CHANGE already proposes its CR (REQ-017); creating another draft would turn a CR into a write hold (M3L-103).
    let input: Option<(String,String)> = conn.query_row(
        "SELECT i.input_id,i.payload_json FROM input_queue i JOIN events e ON json_extract(e.payload_json,'$.input_id')=i.input_id \
         WHERE i.run_id=?1 AND ?3='PLAN' AND i.kind='STEER' AND i.state<>'REJECTED' AND i.target_role IS NULL \
         AND e.kind='input.queued' AND e.seq>?2 AND e.seq>(SELECT coalesce(max(seq),0) FROM events WHERE run_id=?1 AND kind='question.answered') \
         AND e.seq>(SELECT coalesce(max(seq),0) FROM events WHERE run_id=?1 AND kind='run.mode_changed' AND json_extract(payload_json,'$.to')='PLAN' AND json_extract(payload_json,'$.from')<>'PLAN') \
         ORDER BY e.seq DESC LIMIT 1",params![run,last,mode],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sqlite_error)?;
    let mode_entry: Option<String> = conn.query_row(
        "SELECT event_id FROM events WHERE run_id=?1 AND kind='run.mode_changed' AND json_extract(payload_json,'$.to')='PLAN' AND json_extract(payload_json,'$.from')<>'PLAN' AND seq>?2 ORDER BY seq DESC LIMIT 1",
        params![run,last],|r|r.get(0)).optional().map_err(sqlite_error)?;
    let (cause, note) = if let Some((id, payload)) = input {
        (
            format!("input/{id}"),
            serde_json::from_str(&payload)
                .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?,
        )
    } else if mode == "PLAN" {
        let cause = match mode_entry {
            Some(id) => format!("plan/{id}"),
            None if last == 0 => "initial_plan".into(),
            None => return Ok(None),
        };
        (
            cause,
            json!({"text":"根据当前已批准的规格与用户目标起草变更；只提出 CR 并请用户批准。"}),
        )
    } else {
        if super::super::admission::product_work_progressing(conn, run)? {
            return Ok(None);
        }
        let reasons = match super::super::readiness::run_readiness(conn, run)? {
            Some(crate::readiness::Readiness::NotReady(reasons)) => reasons
                .into_iter()
                .filter(|r| {
                    matches!(
                        r.code,
                        crate::readiness::Code::MissingAc
                            | crate::readiness::Code::Unobservable
                            | crate::readiness::Code::Undecided
                    )
                })
                .collect::<Vec<_>>(),
            _ => vec![],
        };
        if reasons.is_empty() {
            return Ok(None);
        }
        (
            "auto_content".into(),
            json!({"reasons":reasons.iter().map(crate::readiness::Reason::json).collect::<Vec<_>>()}),
        )
    };
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND kind='spec.draft_requested' AND json_extract(payload_json,'$.base_spec_id')=?2 AND json_extract(payload_json,'$.cause')=?3)",
        params![run,spec,cause],|r|r.get(0)).map_err(sqlite_error)?;
    Ok((!exists).then_some(Request {
        project,
        spec,
        cause,
        note,
    }))
}

pub(super) fn step(conn: &mut Connection, run: &str, ctx: &Ctx<'_>) -> Result<bool, PersistError> {
    if requested(conn, run)?.is_none() {
        return Ok(false);
    }
    let tx = writer::begin(conn, ctx)?;
    let Some(request) = requested(&tx, run)? else {
        return Ok(false);
    };
    let generation: i64 = tx
        .query_row(
            "SELECT count(*)+1 FROM tasks WHERE run_id=?1 AND kind='spec_draft'",
            [run],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let task = child_id(run, &format!("spec-draft/{generation}"));
    tx.execute("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,layer,kind) VALUES(?1,?2,?3,'READY','cto',1,(SELECT coalesce(max(created_seq),0)+1 FROM tasks WHERE run_id=?2),0,'spec_draft')",
        params![task,run,request.spec]).map_err(sqlite_error)?;
    writer::write(
        &tx,
        &Commit {
            events: vec![
                event(
                    &request.project,
                    run,
                    child_id(&task, "requested"),
                    "spec.draft_requested",
                    json!({"task_id":task,"base_spec_id":request.spec,"generation":generation,"cause":request.cause,"note":request.note}),
                ),
                event(
                    &request.project,
                    run,
                    child_id(&task, "created"),
                    "spec.drafting",
                    json!({"task_id":task,"base_spec_id":request.spec,"generation":generation}),
                ),
            ],
            ..Commit::default()
        },
        ctx,
    )?;
    writer::commit(tx, ctx)?;
    Ok(true)
}
