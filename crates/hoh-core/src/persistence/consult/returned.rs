//! Consultation completion and replay share the caller's transaction and the existing input outbox.
use super::super::inputs::{self, NewInput};
use super::*;

pub(in super::super) fn finish(
    tx: &Transaction<'_>,
    attempt: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let task: Option<String> = tx
        .query_row(
            "SELECT task_id FROM attempts WHERE attempt_id=?1",
            params![attempt],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(task) = task {
        settle(tx, &task, ctx)?;
    }
    Ok(())
}
pub(in super::super) fn sweep_open(
    tx: &Transaction<'_>,
    project: &str,
    run: Option<&str>,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let tasks: Vec<String> = {
        let mut q=tx.prepare(&format!("SELECT json_extract(e.payload_json,'$.task_id') FROM events e JOIN outbox o ON o.message_id=json_extract(e.payload_json,'$.waiting_id') WHERE e.project_id=?1 AND (?2 IS NULL OR e.run_id=?2) AND e.kind='task.consult' AND o.state IN ('PENDING','DELIVERING') AND {} ORDER BY e.seq", super::super::hold::not_held("e.run_id"))).map_err(sqlite_error)?;
        q.query_map(params![project, run], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for task in tasks {
        settle(tx, &task, ctx)?;
    }
    Ok(())
}

/// Close open consultations before the terminal Run's result publishes its stop, in that same transaction.
pub(in super::super) fn finish_run(
    tx: &Transaction<'_>,
    run: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let tasks: Vec<String> = {
        let mut q = tx.prepare("SELECT t.task_id FROM tasks t JOIN events e ON e.run_id=t.run_id AND e.kind='task.consult' AND json_extract(e.payload_json,'$.task_id')=t.task_id WHERE t.run_id=?1 AND NOT EXISTS(SELECT 1 FROM events f WHERE f.run_id=t.run_id AND f.kind='task.consult_finished' AND json_extract(f.payload_json,'$.task_id')=t.task_id) ORDER BY e.seq").map_err(sqlite_error)?;
        q.query_map(params![run], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for task in tasks {
        end(tx, &task, None, Some("run_ended"), None, ctx)?;
    }
    Ok(())
}
pub(in super::super) fn sweep_run(
    tx: &Transaction<'_>,
    project: &str,
    run: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    sweep_open(tx, project, Some(run), ctx)
}
pub(in super::super) fn sweep(
    tx: &Transaction<'_>,
    project: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    sweep_open(tx, project, None, ctx)?;
    let pending: Vec<(String, String, String)> = {
        let mut q=tx.prepare("SELECT input_id,i.run_id,json_extract(i.payload_json,'$.consultation_task_id') FROM input_queue i JOIN runs r USING(run_id) WHERE r.project_id=?1 AND i.state IN ('QUEUED','DELIVERED') AND json_extract(i.payload_json,'$.consultation_task_id') IS NOT NULL ORDER BY i.rowid").map_err(sqlite_error)?;
        q.query_map(params![project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for (input, run, child) in pending {
        if !parent_valid(tx, &run, &child)? {
            // Same input-queue reason as routing::reject_stale. Not a Message reason_code (M3K-07).
            inputs::exhausted(tx, &input, "CONSULTATION_OBSOLETE", ctx)?;
        }
    }
    super::recovery::sweep(tx, project, ctx)
}
fn metadata(tx: &Connection, task: &str) -> Result<Option<(String, Value, Message)>, PersistError> {
    let raw: Option<(String, String)> = tx.query_row("SELECT project_id,payload_json FROM events WHERE kind='task.consult' AND json_extract(payload_json,'$.task_id')=?1 ORDER BY seq LIMIT 1", params![task], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(sqlite_error)?;
    raw.map(|(project, raw)| {
        let meta: Value = serde_json::from_str(&raw)
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
        let request: Message = serde_json::from_value(meta["source_message"].clone())
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
        Ok((project, meta, request))
    })
    .transpose()
}
fn current(tx: &Connection, request: &Message, meta: &Value) -> Result<bool, PersistError> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks t JOIN runs r USING(run_id) JOIN role_bindings b ON b.run_id=r.run_id AND b.role=t.role WHERE t.task_id=?1 AND t.state<>'CANCELLED' AND r.state NOT IN ('CANCEL_REQUESTED','CANCELLED','COMPLETED','FAILED') AND r.spec_id=?2 AND t.spec_id=?2 AND b.role_epoch=?3)",params![request.task_id,meta["spec_id"].as_str(),meta["parent_epoch"].as_i64()],|r|r.get(0)).map_err(sqlite_error)
}
fn settle(tx: &Transaction<'_>, task: &str, ctx: &Ctx<'_>) -> Result<(), PersistError> {
    if super::state::finished(tx, task)? {
        return Ok(());
    }
    let Some((_, meta, request)) = metadata(tx, task)? else {
        return Ok(());
    };
    let (state, failures): (String, i64) = tx
        .query_row(
            "SELECT state,attempt_count FROM tasks WHERE task_id=?1",
            params![task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let latest: Option<(String, String, i64, bool)> = tx.query_row("SELECT attempt_id,state,role_epoch,cancel_requested FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1", params![task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional().map_err(sqlite_error)?;
    let mut reason = None;
    let mut detail = None;
    let mut actual = None;
    // A cancelled allocation without a task cancellation request (pause/block/restart) is still open work.
    if !current(tx, &request, &meta)? {
        reason = Some("obsolete_parent");
    } else if state == "CANCELLED" {
        reason = Some("cancelled");
    } else if latest.as_ref().is_some_and(|a| a.1 == "UNKNOWN_EFFECT") {
        reason = Some("unknown_effect");
    } else if latest.as_ref().is_some_and(|a| a.1 == "CANCELLED" && a.3) {
        reason = Some("cancelled");
    } else if let Some((attempt, attempt_state, epoch, _)) = &latest
        && matches!(attempt_state.as_str(), "SETTLED" | "FAILED")
    {
        let source_current: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM role_bindings WHERE run_id=?1 AND role=?2 AND role_epoch=?3)", params![request.run_id,request.recipient_role,epoch], |r| r.get(0)).map_err(sqlite_error)?;
        if !source_current {
            reason = Some("stale_child_epoch");
        } else {
            actual = read_result(tx, &request, task, attempt, *epoch)?;
        }
    }
    let active = latest
        .as_ref()
        .is_some_and(|a| matches!(a.1.as_str(), "PREPARED" | "STARTING" | "RUNNING"));
    if reason.is_none() && actual.is_none() && !active {
        if let Some(stop) =
            super::super::budget_view::load(tx, &request.run_id, ctx.now(), ctx.wall())?
                .task_stop(task)
        {
            reason = Some("budget_exhausted");
            detail = Some(stop.to_string());
        } else if failures > i64::from(crate::retry::MAX_RETRIES) {
            reason = Some("retries_exhausted");
        } else if let Some((attempt, attempt_state, _, _)) = &latest
            && attempt_state == "SETTLED"
            && !closing::exists(tx, &child_id(attempt, "protocol_correction"))?
        {
            let reasons =
                vec!["Consultation output needs one current, untruncated TaskResult".into()];
            let verdict = super::super::completion::correct_readonly_output(
                tx, attempt, task, &reasons, ctx,
            )?;
            if matches!(verdict, super::super::retry::RepairVerdict::Refused { .. }) {
                reason = Some("protocol_corrections_exhausted");
            }
        }
    }
    if reason.is_some() || actual.is_some() {
        end(tx, task, actual, reason, detail, ctx)?;
    }
    Ok(())
}

/// The only terminal-state writer for consult children. All effects roll back together.
pub(super) fn end(
    tx: &Transaction<'_>,
    task: &str,
    actual: Option<crate::protocol::TaskResult>,
    reason: Option<&str>,
    detail: Option<String>,
    ctx: &Ctx<'_>,
) -> Result<i64, PersistError> {
    let revision: i64 = tx
        .query_row(
            "SELECT revision FROM tasks WHERE task_id=?1",
            params![task],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let done = child_id(task, "consult/finished");
    if super::state::finished(tx, task)? {
        return Ok(revision);
    }
    let (project, meta, request) = metadata(tx, task)?
        .ok_or_else(|| PersistError::IntegrityViolation(format!("missing consult {task}")))?;
    let parent_current = current(tx, &request, &meta)?;
    let actual = actual.filter(|_| parent_current);
    let reason = if parent_current {
        reason
    } else {
        Some("obsolete_parent")
    };
    let latest: Option<(String,)> = tx
        .query_row(
            "SELECT attempt_id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
            params![task],
            |r| Ok((r.get(0)?,)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let terminal = if actual.is_some() {
        "IMPLEMENTED"
    } else {
        "CANCELLED"
    };
    let changed = tx.execute("UPDATE tasks SET state=?1,revision=revision+1,blocked_reason=NULL,next_retry_at=NULL WHERE task_id=?2 AND revision=?3", params![terminal,task,revision]).map_err(sqlite_error)?;
    if changed != 1 {
        return Err(PersistError::IntegrityViolation(format!(
            "consult {task} lost its terminal CAS"
        )));
    }
    super::bounds::release(tx, &request.run_id, task, &meta)?;
    let input = child_id(task, "consult/result");
    if parent_current {
        let mut payload = if let Some(result) = &actual {
            let message = Message {
                schema_version: "1.0".into(),
                message_id: input.clone(),
                run_id: request.run_id.clone(),
                task_id: task.into(),
                attempt_id: result.attempt_id.clone(),
                sender_role: result.role.clone(),
                recipient_role: request.sender_role.clone(),
                role_epoch: result.role_epoch,
                correlation_id: request.message_id.clone(),
                causation_id: Some(request.message_id.clone()),
                idempotency_key: input.clone(),
                expected_revision: revision as u64,
                spec_sha256: request.spec_sha256.clone(),
                candidate_id: result.candidate_id.clone(),
                kind: MessageKind::Result,
                reason_code: None,
                payload: serde_json::to_value(result)
                    .map_err(|e| PersistError::Storage(e.to_string()))?
                    .as_object()
                    .cloned()
                    .ok_or_else(|| {
                        PersistError::IntegrityViolation("result is not an object".into())
                    })?,
            };
            message
                .check_payload()
                .map_err(PersistError::IntegrityViolation)?;
            let source =
                serde_json::to_value(&message).map_err(|e| PersistError::Storage(e.to_string()))?;
            json!({"text":source.to_string(),"source_message":source})
        } else {
            let status = Some("CANCELLED");
            json!({"text":format!("Consultation {task} ended{}: {}{}",status.map(|s|format!(" {s}")).unwrap_or_default(),reason.unwrap_or("unavailable"),detail.as_ref().map(|s|format!(" ({s})")).unwrap_or_default()),"consultation_diagnostic":{"task_id":task,"correlation_id":request.message_id,"reason":reason,"detail":detail,"origin":"runtime","status":status}})
        };
        payload["consultation_task_id"] = json!(task);
        inputs::enqueue(
            tx,
            &NewInput {
                input_id: input.clone(),
                run_id: request.run_id.clone(),
                kind: "STEER".into(),
                target_role: Some(request.sender_role.clone()),
                payload,
            },
            ctx,
        )?;
    }
    tx.execute("UPDATE outbox SET state=?2,last_error=?3,next_retry_at=NULL WHERE message_id=?1 AND state IN ('PENDING','DELIVERING')",params![meta["waiting_id"].as_str(),if parent_current {"ACKED"} else {"DEAD_LETTER"},reason]).map_err(sqlite_error)?;
    if parent_current {
        tx.execute(
            "INSERT INTO receipts(consumer,message_id,applied_at,result_json) VALUES (?1,?2,?3,?4)",
            params![
                request.recipient_role,
                meta["waiting_id"].as_str(),
                crate::time::rfc3339_utc(ctx.now()),
                json!({"kind":"ack","task_id":task,"result_input_id":input}).to_string()
            ],
        )
        .map_err(sqlite_error)?;
    }
    event(
        tx,
        &project,
        &request,
        done,
        "task.consult_finished",
        json!({"task_id":task,"parent_task_id":request.task_id,"attempt_id":latest.as_ref().map(|a|&a.0),"input_id":parent_current.then_some(input),"reason":reason,"detail":detail,"status":actual.as_ref().map_or(json!("CANCELLED"), |r|json!(r.status))}),
    )?;
    Ok(revision + 1)
}

/// Recheck the original task and specification at each delivery boundary, not just when the answer was enqueued.
pub(in super::super) fn input_current(
    conn: &Connection,
    row: &super::super::InputRow,
) -> Result<bool, PersistError> {
    match row.payload["consultation_task_id"].as_str() {
        Some(child) => parent_valid(conn, &row.run_id, child),
        None => Ok(true),
    }
}
pub(super) fn parent_valid(
    conn: &Connection,
    run: &str,
    child: &str,
) -> Result<bool, PersistError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM events e JOIN tasks t ON t.task_id=json_extract(e.payload_json,'$.parent_task_id') JOIN runs r ON r.run_id=t.run_id WHERE e.run_id=?1 AND e.kind='task.consult' AND json_extract(e.payload_json,'$.task_id')=?2 AND t.state<>'CANCELLED' AND t.spec_id=r.spec_id AND r.spec_id=json_extract(e.payload_json,'$.spec_id'))",params![run,child],|r|r.get(0)).map_err(sqlite_error)
}

fn read_result(
    conn: &Connection,
    request: &Message,
    task: &str,
    attempt: &str,
    epoch: i64,
) -> Result<Option<crate::protocol::TaskResult>, PersistError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE run_id=?1 AND event_id=?2",
            params![request.run_id, child_id(attempt, "outputs")],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let output: Value = raw
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?
        .unwrap_or(Value::Null);
    let lines: Vec<String> = output["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let identity = crate::bridge::Identity {
        run_id: request.run_id.clone(),
        task_id: task.into(),
        attempt_id: attempt.into(),
        role: request.recipient_role.clone(),
        role_epoch: u64::try_from(epoch)
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?,
    };
    Ok(crate::bridge::parse(&lines, &identity)
        .task_result
        .filter(|r| {
            output["truncated"] != true
                && r.candidate_id
                    .as_ref()
                    .is_none_or(|c| Some(c) == request.candidate_id.as_ref())
        }))
}

fn require_terminal(conn: &Connection, task: &str, has_result: bool) -> Result<(), PersistError> {
    let row: Option<(String, bool)> = conn
        .query_row(
            "SELECT state,writer FROM tasks WHERE task_id=?1",
            params![task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    // Read-only work with any valid TaskResult remains IMPLEMENTED; it is not acceptance of a candidate or an AC.
    if row.as_ref().is_some_and(|(state, writer)| {
        matches!(state.as_str(), "CANCELLED" | "ACCEPTED")
            || (state == "IMPLEMENTED" && !writer && has_result)
    }) {
        return Ok(());
    }
    Err(PersistError::IntegrityViolation(format!(
        "consult {task} finished without a terminal child outcome: {row:?}"
    )))
}

/// Run alongside the open-question audit in the same read snapshot, including other runs and closed waits.
pub(in super::super) fn check_terminals(conn: &Connection) -> Result<(), PersistError> {
    let open_terminal: Option<String> = conn.query_row("SELECT t.task_id FROM tasks t JOIN events c ON c.kind='task.consult' AND json_extract(c.payload_json,'$.task_id')=t.task_id WHERE t.state IN ('IMPLEMENTED','ACCEPTED','CANCELLED') AND NOT EXISTS(SELECT 1 FROM events f WHERE f.kind='task.consult_finished' AND json_extract(f.payload_json,'$.task_id')=t.task_id) LIMIT 1", [], |r| r.get(0)).optional().map_err(sqlite_error)?;
    if let Some(task) = open_terminal {
        return Err(PersistError::IntegrityViolation(format!(
            "consult {task} is open with a terminal child"
        )));
    }

    let mut q = conn
        .prepare(
            "SELECT e.run_id,e.payload_json,c.payload_json FROM events e
        LEFT JOIN events c ON c.run_id=e.run_id AND c.kind='task.consult'
            AND json_extract(c.payload_json,'$.task_id')=json_extract(e.payload_json,'$.task_id')
        WHERE e.kind='task.consult_finished' ORDER BY e.seq",
        )
        .map_err(sqlite_error)?;
    for row in q
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(sqlite_error)?
    {
        let (run, raw, meta) = row.map_err(sqlite_error)?;
        let finished: Value = serde_json::from_str(&raw)
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
        let task = finished["task_id"].as_str().ok_or_else(|| {
            PersistError::IntegrityViolation("consult finished without task id".into())
        })?;
        let mut has_result = false;
        if finished["status"] != "CANCELLED"
            && let Some(meta) = meta
        {
            let meta: Value = serde_json::from_str(&meta)
                .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
            let request: Message = serde_json::from_value(meta["source_message"].clone())
                .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
            let latest: Option<(String, String, i64)> = conn.query_row(
                "SELECT attempt_id,state,role_epoch FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
                params![task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ).optional().map_err(sqlite_error)?;
            if let Some((attempt, state, epoch)) = latest {
                has_result = run.as_deref() == Some(&request.run_id)
                    && matches!(state.as_str(), "SETTLED" | "FAILED")
                    && finished["attempt_id"] == attempt
                    && read_result(conn, &request, task, &attempt, epoch)?
                        .is_some_and(|r| json!(r.status) == finished["status"]);
            }
        }
        require_terminal(conn, task, has_result)?;
    }
    Ok(())
}
