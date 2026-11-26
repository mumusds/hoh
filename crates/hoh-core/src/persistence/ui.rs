//! Rebuildable event identities. The engineering iteration counter and Task ownership are never changed here.
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

pub(super) fn inserted(
    conn: &Connection,
    seq: i64,
    run: Option<&str>,
    kind: &str,
    payload: &Value,
) -> Result<(), PersistError> {
    // A migration test can deliberately open an older schema. Production always opens migration 003.
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='ui_index_generations')",
            [],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !exists {
        return Ok(());
    }
    conn.execute("INSERT INTO ui_index_generations(generation,target_watermark,index_ready,active) SELECT 1,?1,?2,?2 WHERE NOT EXISTS(SELECT 1 FROM ui_index_generations)",params![seq,i64::from(seq==1)]).map_err(sqlite_error)?;
    let active: Option<i64> = conn
        .query_row(
            "SELECT generation FROM ui_index_generations WHERE active=1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(generation) = active {
        if !try_index(conn, generation, seq, run, kind, payload)? {
            return Ok(());
        }
        conn.execute("UPDATE ui_index_generations SET indexed_through=?1,target_watermark=?1 WHERE generation=?2",params![seq,generation]).map_err(sqlite_error)?;
    }
    Ok(())
}

pub(super) fn index_event(
    conn: &Connection,
    generation: i64,
    seq: i64,
    run: Option<&str>,
    kind: &str,
    payload: &Value,
) -> Result<(), PersistError> {
    validate(kind, payload)?;
    let iteration = if let Some(run) = run {
        let (number,start):(i64,i64)=conn.query_row("SELECT coalesce(max(number),0)+1,coalesce(max(close_seq),0) FROM ui_iteration_ranges WHERE generation=?1 AND run_id=?2 AND close_seq IS NOT NULL",params![generation,run],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sqlite_error)?;
        let key = child_id(run, &format!("iteration/{number}"));
        conn.execute("INSERT OR IGNORE INTO ui_iteration_ranges(generation,run_id,number,iteration_key,start_seq) VALUES(?1,?2,?3,?4,?5)",params![generation,run,number,key,start]).map_err(sqlite_error)?;
        if kind == "iteration.completed" {
            if payload["iteration"].as_i64() != Some(number) {
                return Err(PersistError::IntegrityViolation(format!(
                    "UI iteration boundary at {seq}: expected {number}"
                )));
            }
            conn.execute("UPDATE ui_iteration_ranges SET close_seq=?1 WHERE generation=?2 AND run_id=?3 AND number=?4",params![seq,generation,run,number]).map_err(sqlite_error)?;
        }
        Some(key)
    } else {
        None
    };
    let attempt = payload["attempt_id"].as_str();
    let facts: Option<(String, Option<i64>, Option<String>)> = if let Some(id) = attempt {
        conn.query_row("SELECT t.role,a.role_epoch,a.session_id FROM attempts a JOIN tasks t USING(task_id) WHERE a.attempt_id=?1 AND (?2 IS NULL OR t.run_id=?2)",params![id,run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sqlite_error)?
    } else {
        None
    };
    let role = payload["role"]
        .as_str()
        .or(payload["target_role"].as_str())
        .or_else(|| facts.as_ref().map(|f| f.0.as_str()));
    let epoch = payload["role_epoch"]
        .as_i64()
        .or_else(|| facts.as_ref().and_then(|f| f.1))
        .filter(|n| *n > 0);
    let named = payload["session_id"]
        .as_str()
        .or_else(|| facts.as_ref().and_then(|f| f.2.as_deref()));
    let session: Option<String> = if let Some(id) = named {
        conn.query_row(
            "SELECT session_id FROM sessions WHERE session_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
    } else {
        None
    };
    conn.execute("INSERT OR IGNORE INTO ui_event_index(generation,seq,run_id,iteration_key,attempt_id,role,role_epoch,session_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![generation,seq,run,iteration,attempt.filter(|_|facts.is_some()),role,epoch,session]).map_err(sqlite_error)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UiIdentity {
    pub seq: i64,
    pub run_id: Option<String>,
    pub iteration_key: Option<String>,
    pub attempt_id: Option<String>,
    pub role: Option<String>,
    pub role_epoch: Option<i64>,
    pub session_id: Option<String>,
}

pub(super) fn at(
    conn: &Connection,
    run: &str,
    watermark: i64,
) -> Result<Vec<UiIdentity>, PersistError> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    let state = status(&tx)?;
    if !state.index_ready || state.indexed_through < watermark {
        return Err(PersistError::Busy("index_rebuilding".into()));
    }
    let hidden = tail_hidden(&tx, run, watermark)?;
    let rows=tx.prepare("SELECT i.seq,i.run_id,CASE WHEN ?3 AND (r.close_seq IS NULL OR r.close_seq>?2) THEN NULL ELSE i.iteration_key END,i.attempt_id,i.role,i.role_epoch,i.session_id FROM ui_event_index i LEFT JOIN ui_iteration_ranges r ON r.generation=i.generation AND r.iteration_key=i.iteration_key WHERE i.generation=?4 AND i.run_id=?1 AND i.seq<=?2 ORDER BY i.seq").map_err(sqlite_error)?
        .query_map(params![run,watermark,hidden,state.generation],|r|Ok(UiIdentity{seq:r.get(0)?,run_id:r.get(1)?,iteration_key:r.get(2)?,attempt_id:r.get(3)?,role:r.get(4)?,role_epoch:r.get(5)?,session_id:r.get(6)?})).map_err(sqlite_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sqlite_error)?;
    // A read snapshot needs no commit and invokes none of the writer lifecycle hooks.
    drop(tx);
    Ok(rows)
}

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UiIndexStatus {
    pub generation: i64,
    pub indexed_through: i64,
    pub target_watermark: i64,
    pub index_ready: bool,
}
pub(super) fn status(conn: &Connection) -> Result<UiIndexStatus, PersistError> {
    conn.query_row("SELECT generation,indexed_through,target_watermark,index_ready FROM ui_index_generations ORDER BY generation DESC LIMIT 1",[],|r|Ok(UiIndexStatus{generation:r.get(0)?,indexed_through:r.get(1)?,target_watermark:r.get(2)?,index_ready:r.get(3)?})).optional().map_err(sqlite_error).map(|s|s.unwrap_or(UiIndexStatus{generation:0,indexed_through:0,target_watermark:0,index_ready:false}))
}
pub(super) fn backfill(
    conn: &mut Connection,
    ctx: &super::writer::Ctx<'_>,
) -> Result<UiIndexStatus, PersistError> {
    // A healthy index needs an integrity read, not an empty write transaction. In particular,
    // starting the background observer must not acquire a write lock or consume a command's write hooks.
    let current = status(conn)?;
    if current.index_ready {
        let max: i64 = conn
            .query_row("SELECT coalesce(max(seq),0) FROM events", [], |r| r.get(0))
            .map_err(sqlite_error)?;
        let holes: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM events e LEFT JOIN ui_event_index i ON i.seq=e.seq AND i.generation=?1 WHERE e.seq<=?2 AND i.seq IS NULL)", params![current.generation,current.indexed_through], |r| r.get(0)).map_err(sqlite_error)?;
        if current.indexed_through == max
            && !holes
            && missing_boundary(conn, current.generation)?.is_none()
        {
            return Ok(current);
        }
    }
    let tx = super::writer::begin(conn, ctx)?;
    let max: i64 = tx
        .query_row("SELECT coalesce(max(seq),0) FROM events", [], |r| r.get(0))
        .map_err(sqlite_error)?;
    tx.execute("INSERT INTO ui_index_generations(generation,target_watermark) SELECT 1,?1 WHERE NOT EXISTS(SELECT 1 FROM ui_index_generations)",[max]).map_err(sqlite_error)?;
    let mut state = status(&tx)?;
    // A startup integrity check catches holes without changing a healthy generation on an ordinary reopen.
    if state.index_ready {
        let missing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM events e LEFT JOIN ui_event_index i ON i.seq=e.seq AND i.generation=?1 WHERE e.seq<=?2 AND i.seq IS NULL)",params![state.generation,state.indexed_through],|r|r.get(0)).map_err(sqlite_error)?;
        if missing {
            tx.execute(
                "UPDATE ui_index_generations SET active=0 WHERE active=1",
                [],
            )
            .map_err(sqlite_error)?;
            tx.execute(
                "INSERT INTO ui_index_generations(generation,target_watermark) VALUES(?1,?2)",
                params![state.generation + 1, max],
            )
            .map_err(sqlite_error)?;
            state = status(&tx)?;
        } else if state.indexed_through == max {
            complete_boundaries(&tx, state.generation)?;
            super::writer::commit(tx, ctx)?;
            return status(conn);
        }
    }
    tx.execute(
        "UPDATE ui_index_generations SET target_watermark=?1 WHERE generation=?2",
        params![max, state.generation],
    )
    .map_err(sqlite_error)?;
    // Only identity/lifecycle fields cross the backfill boundary; legacy megabyte transcripts stay untouched.
    let sql = "SELECT seq,run_id,kind,CASE WHEN json_valid(payload_json) THEN json_object('iteration',json_extract(payload_json,'$.iteration'),'attempt_id',json_extract(payload_json,'$.attempt_id'),'role',json_extract(payload_json,'$.role'),'target_role',json_extract(payload_json,'$.target_role'),'role_epoch',json_extract(payload_json,'$.role_epoch'),'session_id',json_extract(payload_json,'$.session_id'),'state',json_extract(payload_json,'$.state'),'from',json_extract(payload_json,'$.from'),'to',json_extract(payload_json,'$.to'),'command_id',json_extract(payload_json,'$.command_id'),'reason',json_extract(payload_json,'$.reason'),'due',json_extract(payload_json,'$.due'),'stop',json_extract(payload_json,'$.stop'),'remaining',json_extract(payload_json,'$.remaining')) ELSE '{}' END FROM events WHERE seq>?1 AND seq<=?2 ORDER BY seq LIMIT 500";
    let mut stmt = tx.prepare(sql).map_err(sqlite_error)?;
    let mut rows = stmt
        .query(params![state.indexed_through, max])
        .map_err(sqlite_error)?;
    let mut bytes = 0usize;
    let mut through = state.indexed_through;
    let mut valid = true;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let seq: i64 = row.get(0).map_err(sqlite_error)?;
        let run: Option<String> = row.get(1).map_err(sqlite_error)?;
        let kind: String = row.get(2).map_err(sqlite_error)?;
        let raw: String = row.get(3).map_err(sqlite_error)?;
        let size = raw.len() + kind.len() + run.as_ref().map_or(0, String::len) + 8;
        if bytes > 0 && bytes + size > 1 << 20 {
            break;
        }
        let payload: Value =
            serde_json::from_str(&raw).map_err(|e| PersistError::Storage(e.to_string()))?;
        if !try_index(&tx, state.generation, seq, run.as_deref(), &kind, &payload)? {
            valid = false;
            break;
        }
        bytes += size;
        through = seq;
    }
    drop(rows);
    drop(stmt);
    let ready = valid && through == max && complete_boundaries(&tx, state.generation)?;
    tx.execute("UPDATE ui_index_generations SET indexed_through=?1,index_ready=?2,active=?2 WHERE generation=?3",params![through,ready,state.generation]).map_err(sqlite_error)?;
    super::writer::commit(tx, ctx)?;
    status(conn)
}

// Engineering rows are only a corruption check, never the source of an event's membership.
fn missing_boundary(
    conn: &Connection,
    generation: i64,
) -> Result<Option<(String, String, i64)>, PersistError> {
    conn.query_row(
        "SELECT r.project_id,i.run_id,i.number FROM iterations i JOIN runs r USING(run_id) WHERE NOT EXISTS(SELECT 1 FROM ui_iteration_ranges u WHERE u.generation=?1 AND u.run_id=i.run_id AND u.number=i.number AND u.close_seq IS NOT NULL) ORDER BY i.run_id,i.number LIMIT 1",
        [generation], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional().map_err(sqlite_error)
}
fn complete_boundaries(conn: &Connection, generation: i64) -> Result<bool, PersistError> {
    let Some((project, run, number)) = missing_boundary(conn, generation)? else {
        return Ok(true);
    };
    conn.execute(
        "UPDATE ui_index_generations SET active=0,index_ready=0 WHERE generation=?1",
        [generation],
    )
    .map_err(sqlite_error)?;
    let event_id = child_id(
        &run,
        &format!("ui-index/{generation}/missing-close/{number}"),
    );
    let recorded: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id=?1)",
            [&event_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !recorded {
        super::writer::insert_event(
            conn,
            &super::writer::NewEvent {
                intent: super::writer::Intent::Record,
                project_id: project,
                run_id: None,
                event_id,
                kind: "runner.diagnostic".into(),
                payload: serde_json::json!({"reason":"ui_missing_close","run_id":run,"iteration":number,"generation":generation}),
            },
        )?;
    }
    Ok(false)
}

fn validate(kind: &str, p: &Value) -> Result<(), PersistError> {
    if kind == "run.result"
        && !matches!(
            p["state"].as_str(),
            Some(
                "CREATED"
                    | "DISCOVERING"
                    | "SPECIFYING"
                    | "READY"
                    | "VERIFYING"
                    | "PAUSE_REQUESTED"
                    | "CANCEL_REQUESTED"
                    | "RUNNING"
                    | "WAITING_EXTERNAL"
                    | "BLOCKED"
                    | "PAUSED"
                    | "INCOMPLETE"
                    | "COMPLETED"
                    | "FAILED"
                    | "CANCELLED"
            )
        )
    {
        return Err(PersistError::IntegrityViolation(
            "UI lifecycle state is missing or unknown".into(),
        ));
    }
    if kind == "run.resumed"
        && !((p["command_id"].is_string() && p["reason"].is_string())
            || (p["from"].is_string() && p["to"].is_string() && !p["due"].is_null()))
    {
        return Err(PersistError::IntegrityViolation(
            "UI resume state fields are missing".into(),
        ));
    }
    if kind == "run.cancelled" && p["state"] != "CANCELLED" {
        return Err(PersistError::IntegrityViolation(
            "UI cancellation state is missing".into(),
        ));
    }
    if kind == "run.incomplete"
        && !(p["stop"].is_string() && p["from"].is_string() && p["remaining"].is_array())
    {
        return Err(PersistError::IntegrityViolation(
            "UI incomplete state fields are missing".into(),
        ));
    }
    Ok(())
}
fn try_index(
    conn: &Connection,
    generation: i64,
    seq: i64,
    run: Option<&str>,
    kind: &str,
    p: &Value,
) -> Result<bool, PersistError> {
    conn.execute_batch("SAVEPOINT public_ui_identity")
        .map_err(sqlite_error)?;
    match index_event(conn, generation, seq, run, kind, p) {
        Ok(()) => {
            conn.execute_batch("RELEASE public_ui_identity")
                .map_err(sqlite_error)?;
            Ok(true)
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK TO public_ui_identity; RELEASE public_ui_identity")
                .map_err(sqlite_error)?;
            if !matches!(e, PersistError::IntegrityViolation(_)) {
                return Err(e);
            }
            conn.execute(
                "UPDATE ui_index_generations SET active=0,index_ready=0 WHERE generation=?1",
                [generation],
            )
            .map_err(sqlite_error)?;
            // The diagnostic is independent of the source payload and cannot recursively rebuild this generation.
            let event_id = format!("ui-index/{generation}/invalid/{seq}");
            let recorded: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM events WHERE event_id=?1)",
                    [&event_id],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            if !recorded {
                let project: String = conn
                    .query_row("SELECT project_id FROM events WHERE seq=?1", [seq], |r| {
                        r.get(0)
                    })
                    .map_err(sqlite_error)?;
                super::writer::insert_event(
                    conn,
                    &super::writer::NewEvent {
                        intent: super::writer::Intent::Record,
                        project_id: project,
                        run_id: None,
                        event_id,
                        kind: "runner.diagnostic".into(),
                        payload: serde_json::json!({"reason":"ui_index_invalid","source_seq":seq,"generation":generation,"detail":e.to_string()}),
                    },
                )?;
            }
            Ok(false)
        }
    }
}
fn tail_hidden(conn: &Connection, run: &str, h: i64) -> Result<bool, PersistError> {
    let mut hidden = false;
    let mut terminal = false;
    let mut resume = None;
    let mut stmt=conn.prepare("SELECT kind,payload_json FROM events WHERE run_id=?1 AND seq<=?2 AND kind IN ('run.result','run.cancelled','run.incomplete','reconcile.run','run.resumed') ORDER BY seq").map_err(sqlite_error)?;
    let mut rows = stmt.query(params![run, h]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let kind: String = row.get(0).map_err(sqlite_error)?;
        let raw: String = row.get(1).map_err(sqlite_error)?;
        let p: Value = serde_json::from_str(&raw)
            .map_err(|_| PersistError::Busy("index_rebuilding: invalid lifecycle".into()))?;
        validate(&kind, &p)
            .map_err(|_| PersistError::Busy("index_rebuilding: invalid lifecycle".into()))?;
        match kind.as_str() {
            "run.result" => {
                let state = p["state"].as_str().unwrap_or("");
                terminal |= matches!(state, "COMPLETED" | "FAILED" | "CANCELLED");
                hidden = terminal || state == "INCOMPLETE";
            }
            "run.cancelled" => {
                terminal = true;
                hidden = true;
            }
            "run.incomplete" => hidden = true,
            "reconcile.run" if p["from"] == "INCOMPLETE" => {
                resume = p["command_id"].as_str().map(str::to_owned)
            }
            "run.resumed"
                if !terminal
                    && ((resume.is_some() && p["command_id"].as_str() == resume.as_deref())
                        || (p["from"] == "INCOMPLETE"
                            && matches!(
                                p["to"].as_str(),
                                Some(
                                    "RUNNING"
                                        | "DISCOVERING"
                                        | "SPECIFYING"
                                        | "READY"
                                        | "WAITING_EXTERNAL"
                                        | "BLOCKED"
                                )
                            ))) =>
            {
                hidden = false;
                resume = None;
            }
            _ => {}
        }
    }
    Ok(hidden)
}
