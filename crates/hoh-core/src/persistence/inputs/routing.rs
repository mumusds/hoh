//! Durable routing reservations for host input delivery (ADR-036).
use super::*;

#[derive(Debug, Clone)]
pub enum InputRouting {
    Stage {
        row: super::super::OutboxRow,
        attempt: Option<String>,
        now: std::time::SystemTime,
    },
    Finish {
        input_id: String,
        attempt: String,
        outcome: InputOutcome,
    },
    Take {
        attempt: String,
    },
}
#[derive(Debug, Clone)]
pub enum InputOutcome {
    Delivered,
    NextAttempt,
    Rejected(String),
}
#[derive(Debug, Default, Clone)]
pub struct InputRouted {
    pub reply: Option<String>,
    pub steer: Option<InputRow>,
    pub inputs: Vec<Value>,
}

fn save(tx: &Transaction<'_>, row: &InputRow) -> Result<(), PersistError> {
    tx.execute(
        "UPDATE input_queue SET payload_json=?2 WHERE input_id=?1",
        params![row.input_id, row.payload.to_string()],
    )
    .map_err(sqlite_error)?;
    Ok(())
}
fn receipt(
    tx: &Transaction<'_>,
    row: &InputRow,
    now: std::time::SystemTime,
) -> Result<String, PersistError> {
    let reply=json!({"message_id":row.input_id,"kind":if row.state==InputState::Rejected {"rejected"} else {"ack"},"reason":row.payload.get("reason"),"input_sha256":fingerprint(row)}).to_string();
    tx.execute(
        "INSERT INTO receipts(consumer,message_id,applied_at,result_json) VALUES (?1,?2,?3,?4)",
        params![
            row.target_role.as_deref().unwrap_or("commander"),
            row.input_id,
            crate::time::rfc3339_utc(now),
            reply
        ],
    )
    .map_err(sqlite_error)?;
    Ok(reply)
}
fn previous(tx: &Transaction<'_>, id: &str, role: &str) -> Result<Option<String>, PersistError> {
    let stored: Option<String> = tx
        .query_row(
            "SELECT result_json FROM receipts WHERE consumer=?1 AND message_id=?2",
            params![role, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(ref reply) = stored {
        check_receipt(tx, id, reply)?;
    }
    Ok(stored)
}

fn current(tx: &Transaction<'_>, row: &InputRow) -> Result<bool, PersistError> {
    let bound:bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_bindings WHERE run_id=?1 AND role=?2 AND role_epoch=?3)",
        params![
            row.run_id,
            row.target_role.as_deref().unwrap_or("commander"),
            row.payload["role_epoch"].as_i64()
        ],
        |r| r.get(0),
    )
    .map_err(sqlite_error)?;
    Ok(bound && super::super::consult::input_current(tx, row)?)
}
fn reject(
    tx: &Transaction<'_>,
    row: &mut InputRow,
    reason: &str,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    if matches!(row.state, InputState::Queued | InputState::Delivered) {
        change(tx, row, InputState::Rejected, None, Some(reason), ctx)?;
        row.state = InputState::Rejected;
        row.payload["reason"] = json!(reason);
    }
    Ok(())
}
fn next(row: &mut InputRow) -> Result<(), PersistError> {
    let payload = row
        .payload
        .as_object_mut()
        .ok_or_else(|| PersistError::IntegrityViolation("input payload is not an object".into()))?;
    payload.insert("boundary".into(), json!("next_attempt"));
    payload.remove("attempt_id");
    payload.remove("delivery_pending");
    Ok(())
}

pub(in super::super) fn route(
    conn: &mut Connection,
    request: &InputRouting,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<InputRouted, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let mut result = InputRouted::default();
    match request {
        InputRouting::Stage {
            row: claimed,
            attempt,
            now,
        } => {
            let raw:(Option<String>,String,i64,String)=tx.query_row("SELECT run_id,recipient_role,role_epoch,payload_json FROM outbox WHERE message_id=?1",params![claimed.message_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(sqlite_error)?;
            if raw.0 != claimed.run_id
                || raw.1 != claimed.recipient_role
                || raw.2 != claimed.role_epoch
                || serde_json::from_str::<Value>(&raw.3)
                    .map_err(|e| PersistError::Storage(e.to_string()))?
                    != claimed.payload
            {
                return Err(PersistError::KeyReuse(claimed.message_id.clone()));
            }
            if let Some(reply) = previous(&tx, &claimed.message_id, &claimed.recipient_role)? {
                result.reply = Some(reply);
                return Ok(result);
            }
            let mut row = read_input(&tx, &claimed.message_id)?
                .ok_or_else(|| PersistError::IntegrityViolation("missing input".into()))?;
            super::super::hold::writable(&tx, &row.run_id)?;
            if first_input_wait(
                &tx,
                &row.run_id,
                &claimed.recipient_role,
                Some(claimed.role_epoch),
            )? {
                return Ok(result);
            }
            if !current(&tx, &row)? {
                reject_stale(&tx, &mut row, ctx)?;
            } else if row.payload["delivery_pending"] == true {
                // A prior sender crossed the external-effect boundary. Never send it a second time blindly.
                reject(&tx, &mut row, "UNKNOWN_DELIVERY", ctx)?;
            } else if row.state == InputState::Queued {
                let active = if let Some(id) = attempt.as_ref().filter(|_| row.kind == "STEER") {
                    tx.query_row("SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t USING(task_id) JOIN runs r USING(run_id) WHERE a.attempt_id=?1 AND a.state='RUNNING' AND t.run_id=?2 AND t.role=?3 AND a.role_epoch=?4 AND r.state IN ('RUNNING','VERIFYING'))",params![id,row.run_id,claimed.recipient_role,claimed.role_epoch],|r|r.get::<_,bool>(0)).map_err(sqlite_error)?
                } else {
                    false
                };
                if active {
                    row.payload["boundary"] = json!("turn");
                    row.payload["attempt_id"] = json!(attempt);
                    row.payload["delivery_pending"] = json!(true);
                    save(&tx, &row)?;
                    result.steer = Some(row);
                    writer::commit(tx, ctx)?;
                    return Ok(result);
                }
                next(&mut row)?;
                save(&tx, &row)?;
            }
            result.reply = Some(receipt(&tx, &row, *now)?);
        }
        InputRouting::Finish {
            input_id,
            attempt,
            outcome,
        } => {
            let Some(mut row) = read_input(&tx, input_id)? else {
                return Ok(result);
            };
            if let Some(reply) = previous(
                &tx,
                input_id,
                row.target_role.as_deref().unwrap_or("commander"),
            )? {
                result.reply = Some(reply);
                return Ok(result);
            }
            if row.payload["attempt_id"] != *attempt {
                return Ok(result);
            }
            if !current(&tx, &row)? {
                reject_stale(&tx, &mut row, ctx)?;
            } else if row.state == InputState::Queued {
                match outcome {
                    InputOutcome::Delivered => {
                        let cancelled:bool=tx.query_row("SELECT state IN ('CANCEL_REQUESTED','CANCELLED','FAILED','COMPLETED','INCOMPLETE') FROM runs WHERE run_id=?1",params![row.run_id],|r|r.get(0)).map_err(sqlite_error)?;
                        if cancelled {
                            reject(&tx, &mut row, "run_stopped", ctx)?;
                        } else {
                            row.payload["delivery_pending"] = json!(false);
                            save(&tx, &row)?;
                            change(&tx, &row, InputState::Delivered, None, None, ctx)?;
                            finish_attempt(&tx, attempt, ctx)?;
                        }
                    }
                    InputOutcome::NextAttempt => {
                        next(&mut row)?;
                        save(&tx, &row)?;
                    }
                    InputOutcome::Rejected(why) => reject(&tx, &mut row, why, ctx)?,
                }
            }
            result.reply = Some(receipt(&tx, &row, std::time::SystemTime::now())?);
        }
        InputRouting::Take { attempt } => {
            let identity:Option<(String,String,i64,i64,String)>=tx.query_row("SELECT t.run_id,t.role,a.role_epoch,a.rowid,a.state FROM attempts a JOIN tasks t USING(task_id) WHERE a.attempt_id=?1",params![attempt],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(sqlite_error)?;
            let Some((run, role, epoch, ordinal, state)) = identity else {
                return Ok(result);
            };
            if role == "commander" || state != "STARTING" {
                return Ok(result);
            }
            super::super::hold::writable(&tx, &run)?;
            if first_input_wait(&tx, &run, &role, Some(epoch))? {
                return Ok(result);
            }
            let ids: Vec<String> = {
                let mut q=tx.prepare("SELECT input_id FROM input_queue WHERE run_id=?1 AND target_role=?2 AND kind IN ('STEER','ASK') AND state='QUEUED' AND json_extract(payload_json,'$.boundary') IS NOT 'turn' AND coalesce(json_extract(payload_json,'$.after_attempt'),0) < ?3 AND (json_extract(payload_json,'$.attempt_id') IS NULL OR json_extract(payload_json,'$.attempt_id')=?4) ORDER BY rowid").map_err(sqlite_error)?;
                q.query_map(params![run, role, ordinal, attempt], |r| r.get(0))
                    .map_err(sqlite_error)?
                    .collect::<Result<_, _>>()
                    .map_err(sqlite_error)?
            };
            for id in ids {
                let mut row = read_input(&tx, &id)?.ok_or_else(|| {
                    PersistError::IntegrityViolation("selected input disappeared".into())
                })?;
                if row.payload["role_epoch"] != epoch || !current(&tx, &row)? {
                    reject_stale(&tx, &mut row, ctx)?;
                    continue;
                }
                if row.payload["attempt_id"] != *attempt {
                    row.payload["boundary"] = json!("next_attempt");
                    row.payload["attempt_id"] = json!(attempt);
                    save(&tx, &row)?;
                    // Prompt preparation is only a reservation. The RUNNING commit confirms delivery before
                    // opening the process gate; a refusal or crash before that can safely return the input.
                    event(
                        &tx,
                        ctx,
                        &row.run_id,
                        child_id(&id, &format!("reserved/{attempt}")),
                        "input.reserved",
                        json!({"input_id":id,"attempt_id":attempt,"boundary":"next_attempt"}),
                    )?;
                    if previous(&tx, &id, &role)?.is_none() {
                        receipt(&tx, &row, std::time::SystemTime::now())?;
                    }
                }
                result
                    .inputs
                    .push(json!({"input_id":id,"kind":row.kind,"text":row.payload["text"]}));
            }
        }
    }
    writer::commit(tx, ctx)?;
    Ok(result)
}

/// Confirm reserved prompt inputs with the Attempt's RUNNING transition, before its external-effect gate opens.
pub(in super::super) fn start_attempt(
    tx: &Transaction<'_>,
    attempt: &str,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let ids: Vec<String> = {
        let mut q=tx.prepare("SELECT input_id FROM input_queue WHERE state='QUEUED' AND json_extract(payload_json,'$.boundary')='next_attempt' AND json_extract(payload_json,'$.attempt_id')=?1 ORDER BY rowid").map_err(sqlite_error)?;
        q.query_map(params![attempt], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for id in ids {
        let mut row = read_input(tx, &id)?
            .ok_or_else(|| PersistError::IntegrityViolation("reserved input disappeared".into()))?;
        if !current(tx, &row)? {
            reject_stale(tx, &mut row, ctx)?;
        } else {
            change(tx, &row, InputState::Delivered, None, None, ctx)?;
        }
    }
    Ok(())
}

pub(in super::super) fn finish_attempt(
    tx: &Transaction<'_>,
    attempt: &str,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let state: Option<String> = tx
        .query_row(
            "SELECT state FROM attempts WHERE attempt_id=?1",
            params![attempt],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(state) = state.filter(|s| !matches!(s.as_str(), "PREPARED" | "STARTING" | "RUNNING"))
    else {
        return Ok(());
    };
    let ids: Vec<String> = {
        let mut q=tx.prepare("SELECT input_id FROM input_queue WHERE (state='DELIVERED' OR (state='QUEUED' AND json_extract(payload_json,'$.boundary')='next_attempt')) AND json_extract(payload_json,'$.attempt_id')=?1").map_err(sqlite_error)?;
        q.query_map(params![attempt], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for id in ids {
        let mut row = read_input(tx, &id)?
            .ok_or_else(|| PersistError::IntegrityViolation("selected input disappeared".into()))?;
        if !current(tx, &row)? {
            reject_stale(tx, &mut row, ctx)?;
        } else if row.state == InputState::Queued {
            // Still reserved: this Attempt never crossed RUNNING, so none of its prompt inputs were sent.
            next(&mut row)?;
            save(tx, &row)?;
            event(
                tx,
                ctx,
                &row.run_id,
                child_id(&id, &format!("released/{attempt}")),
                "input.released",
                json!({"input_id":id,"attempt_id":attempt,"reason":"attempt_not_started"}),
            )?;
        } else if state == "SETTLED" {
            change(tx, &row, InputState::Applied, None, None, ctx)?;
        } else {
            reject(tx, &mut row, "attempt_ended", ctx)?;
        }
    }
    Ok(())
}
pub(in super::super) fn sweep(
    tx: &Transaction<'_>,
    project: &str,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let attempts: Vec<String> = {
        let mut q=tx.prepare("SELECT DISTINCT json_extract(i.payload_json,'$.attempt_id') FROM input_queue i JOIN runs r USING(run_id) WHERE r.project_id=?1 AND (i.state='DELIVERED' OR (i.state='QUEUED' AND json_extract(i.payload_json,'$.boundary')='next_attempt')) AND json_extract(i.payload_json,'$.attempt_id') IS NOT NULL").map_err(sqlite_error)?;
        q.query_map(params![project], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for attempt in attempts {
        finish_attempt(tx, &attempt, ctx)?;
    }
    Ok(())
}

pub(in super::super) fn request(
    conn: &mut Connection,
    message: &crate::protocol::Message,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<Option<String>, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let key = child_id(
        &message.attempt_id,
        &format!("request/{}", message.message_id),
    );
    let historical: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id=?1 AND kind='request.deferred')",
            params![child_id(&key, "deferred")],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if historical {
        return Ok(None);
    }
    let id = child_id(&key, "input");
    let source = serde_json::to_value(message).map_err(|e| PersistError::Storage(e.to_string()))?;
    if let Some(row) = read_input(&tx, &id)? {
        return if row.payload["source_message"] == source {
            Ok(Some(id))
        } else {
            Err(PersistError::KeyReuse(message.message_id.clone()))
        };
    }
    super::super::hold::writable(&tx, &message.run_id)?;
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t USING(task_id) JOIN role_bindings b ON b.run_id=t.run_id AND b.role=t.role WHERE a.attempt_id=?1 AND t.run_id=?2 AND t.role=?3 AND a.role_epoch=?4 AND b.role_epoch=a.role_epoch AND a.state='RUNNING')",params![message.attempt_id,message.run_id,message.sender_role,i64::try_from(message.role_epoch).unwrap_or(-1)],|r|r.get(0)).map_err(sqlite_error)?;
    if !valid {
        return Err(PersistError::IntegrityViolation(
            "request sender is no longer current and running".into(),
        ));
    }
    let kind = match message.kind {
        crate::protocol::MessageKind::Query => "ASK",
        crate::protocol::MessageKind::Steer => "STEER",
        _ => {
            return Err(PersistError::IntegrityViolation(
                "only query/steer can be routed".into(),
            ));
        }
    };
    let text = ["text", "question", "summary"]
        .iter()
        .find_map(|k| message.payload.get(*k).and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| json!(message.payload).to_string());
    enqueue(
        &tx,
        &NewInput {
            input_id: id.clone(),
            run_id: message.run_id.clone(),
            kind: kind.into(),
            target_role: Some(message.recipient_role.clone()),
            payload: json!({"text":text,"message_id":message.message_id,"source_message":source}),
        },
        ctx,
    )?;
    event(
        &tx,
        ctx,
        &message.run_id,
        child_id(&key, "routed"),
        "request.routed",
        json!({"input_id":id,"message_id":message.message_id,"attempt_id":message.attempt_id,"recipient_role":message.recipient_role}),
    )?;
    writer::commit(tx, ctx)?;
    Ok(Some(id))
}

fn reject_stale(
    tx: &Transaction<'_>,
    row: &mut InputRow,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let reason = if row.payload["consultation_task_id"].is_string() {
        "CONSULTATION_OBSOLETE"
    } else {
        "STALE_ROLE_EPOCH"
    };
    reject(tx, row, reason, ctx)
}
