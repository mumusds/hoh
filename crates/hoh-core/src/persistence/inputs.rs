//! The input queue in the store (docs/06-orchestration.md: QUEUED -> DELIVERED -> APPLIED/REJECTED). An input is
//! created QUEUED by whoever accepted it and moved by whoever delivers it (an adapter, for a steer); like an attempt
//! it has no revision column, so a move compares on the state it expects (another one is `StaleState`) and follows
//! `lifecycle::InputState` (`ILLEGAL_TRANSITION` otherwise). Each write appends its event in the same transaction.

use super::decide::child_id;
use super::writer::{self, Commit, Entity, Intent, NewEvent, NewOutbox};
use super::{PersistError, sqlite_error};
use crate::lifecycle::{self, InputState, Lifecycle};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// A new input of run `run_id`, created QUEUED.
#[derive(Debug, Clone, PartialEq)]
pub struct NewInput {
    pub input_id: String,
    pub run_id: String,
    /// `ASK`, `STEER`, `CHANGE`, `CONTROL` or `REPLY` (the table's CHECK).
    pub kind: String,
    pub target_role: Option<String>,
    pub payload: Value,
}

/// Move input `input_id` from `from` to `to`; `reason` goes into the event (e.g. why the agent refused it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputMove {
    pub input_id: String,
    pub from: InputState,
    pub to: InputState,
    pub reason: Option<String>,
}

/// An `input_queue` row.
#[derive(Debug, Clone, PartialEq)]
pub struct InputRow {
    pub input_id: String,
    pub run_id: String,
    pub kind: String,
    pub target_role: Option<String>,
    pub state: InputState,
    pub payload: Value,
}

fn event(
    tx: &Transaction<'_>,
    ctx: &super::writer::Ctx<'_>,
    run_id: &str,
    event_id: String,
    kind: &str,
    payload: Value,
) -> Result<(), PersistError> {
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| PersistError::IntegrityViolation(format!("no run {run_id}")))?;
    let commit = Commit {
        events: vec![NewEvent {
            intent: if kind == "input.rejected" {
                Intent::Record
            } else {
                Intent::Progress
            },
            project_id,
            event_id,
            run_id: Some(run_id.to_owned()),
            kind: kind.to_owned(),
            payload,
        }],
        ..Commit::default()
    };
    writer::write(tx, &commit, ctx).map(drop)
}

pub(super) fn enqueue(
    tx: &Transaction<'_>,
    i: &NewInput,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let role = i
        .target_role
        .as_deref()
        .unwrap_or(if matches!(i.kind.as_str(), "ASK" | "STEER") {
            "commander"
        } else {
            "runtime"
        });
    let epoch: i64 = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id=?1 AND role=?2",
            params![i.run_id, role],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .unwrap_or(0);
    let mut payload = i.payload.clone();
    payload
        .as_object_mut()
        .ok_or_else(|| {
            PersistError::IntegrityViolation("input payload must be an object".to_owned())
        })?
        .insert("role_epoch".to_owned(), json!(epoch));
    let after: i64 = tx.query_row("SELECT coalesce(max(a.rowid),0) FROM attempts a JOIN tasks t USING(task_id) WHERE t.run_id=?1 AND t.role=?2 AND a.state IN ('STARTING','RUNNING')",params![i.run_id,role],|r|r.get(0)).map_err(sqlite_error)?;
    payload["after_attempt"] = json!(after);
    tx.execute(
        "INSERT INTO input_queue(input_id, run_id, kind, target_role, state, payload_json) \
         VALUES (?1, ?2, ?3, ?4, 'QUEUED', ?5)",
        params![
            i.input_id,
            i.run_id,
            i.kind,
            i.target_role,
            payload.to_string()
        ],
    )
    .map_err(sqlite_error)?;
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id=?1",
            params![i.run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    writer::write(tx,&Commit {
        events:vec![NewEvent { intent:Intent::Progress, project_id, run_id:Some(i.run_id.clone()),
            event_id:child_id(&i.input_id,InputState::Queued.as_str()),kind:"input.queued".to_owned(),
            payload:json!({"input_id":i.input_id,"kind":i.kind,"to":"QUEUED","target_role":role,"role_epoch":epoch}) }],
        outbox:vec![NewOutbox { message_id:i.input_id.clone(),event:0,run_id:Some(i.run_id.clone()),
            recipient_role:role.to_owned(),role_epoch:epoch,
            payload:json!({"notice":"input","input_id":i.input_id,"kind":i.kind}) }],
        ..Default::default()
    },ctx).map(drop)
}

pub(super) fn move_input(
    tx: &Transaction<'_>,
    m: &InputMove,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT state, run_id FROM input_queue WHERE input_id = ?1",
            params![m.input_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let stale = |actual: Option<String>| PersistError::StaleState {
        entity: Entity::Input,
        id: m.input_id.clone(),
        expected: m.from.as_str().to_owned(),
        actual,
    };
    let Some((state, run_id)) = row else {
        return Err(stale(None));
    };
    if state != m.from.as_str() {
        return Err(stale(Some(state)));
    }
    lifecycle::check(m.from, m.to).map_err(|_| PersistError::IllegalTransition {
        entity: Entity::Input,
        id: m.input_id.clone(),
        from: m.from.as_str().to_owned(),
        to: m.to.as_str().to_owned(),
        why: "not in the transition table".to_owned(),
        details: None,
    })?;
    tx.execute(
        "UPDATE input_queue SET state = ?1 WHERE input_id = ?2 AND state = ?3",
        params![m.to.as_str(), m.input_id, m.from.as_str()],
    )
    .map_err(sqlite_error)?;
    event(
        tx,
        ctx,
        &run_id,
        child_id(&m.input_id, m.to.as_str()),
        "input.moved",
        json!({
            "input_id": m.input_id,
            "from": m.from.as_str(),
            "to": m.to.as_str(),
            "reason": m.reason,
        }),
    )
}

pub(super) fn read_input(
    conn: &Connection,
    input_id: &str,
) -> Result<Option<InputRow>, PersistError> {
    let raw: Option<(String, String, String, Option<String>, String, String)> = conn
        .query_row(
            "SELECT input_id, run_id, kind, target_role, state, payload_json FROM input_queue WHERE input_id = ?1",
            params![input_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((input_id, run_id, kind, target_role, state, payload)) = raw else {
        return Ok(None);
    };
    Ok(Some(InputRow {
        state: InputState::parse(&state).ok_or_else(|| {
            PersistError::Storage(format!("input {input_id}: unknown state {state}"))
        })?,
        payload: serde_json::from_str(&payload)
            .map_err(|e| PersistError::Storage(format!("input {input_id}: payload: {e}")))?,
        input_id,
        run_id,
        kind,
        target_role,
    }))
}

pub(super) mod routing;

#[cfg(test)]
mod tests;

/// Ordinary deliveries wait until the current holder has consumed its first input (-153).
/// A stale epoch does not wait for the new holder: it must be rejected at consumption.
pub(super) fn first_input_wait(
    conn: &Connection,
    run: &str,
    role: &str,
    epoch: Option<i64>,
) -> Result<bool, PersistError> {
    first_input_gate(conn, run, role, epoch, false)
}

/// Commander ASK/STEER is carried by a new supervision turn, which also carries
/// the owed first-input package. It must wait for session.start and any first turn
/// already in flight, but must not wait for its own turn to complete before routing.
/// Other inputs and HCP messages retain the ordinary first-input gate.
pub(super) fn delivery_input_wait(
    conn: &Connection,
    run: &str,
    role: &str,
    epoch: Option<i64>,
    payload: &Value,
) -> Result<bool, PersistError> {
    let carries_package = role == "commander"
        && payload["notice"] == "input"
        && matches!(payload["kind"].as_str(), Some("ASK" | "STEER"));
    first_input_gate(conn, run, role, epoch, carries_package)
}

fn first_input_gate(
    conn: &Connection,
    run: &str,
    role: &str,
    epoch: Option<i64>,
    carries_package: bool,
) -> Result<bool, PersistError> {
    let binding: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT role_epoch,session_id FROM role_bindings WHERE run_id=?1 AND role=?2",
            params![run, role],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((current, session)) = binding else {
        return Ok(epoch == Some(0));
    };
    if epoch.is_some_and(|epoch| epoch != current) {
        return Ok(false);
    }
    if super::handover::first_input_pending(conn, run, role, current)? {
        return Ok(true);
    }
    match session {
        Some(session) if carries_package => {
            super::handover::first_input_in_flight(conn, run, role, current, &session)
        }
        Some(session) => super::handover::first_input_owed(conn, run, role, current, &session),
        None => Ok(true),
    }
}

fn change(
    tx: &Transaction<'_>,
    row: &InputRow,
    to: InputState,
    answer: Option<&str>,
    reason: Option<&str>,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let kind = match to {
        InputState::Delivered => "input.delivered",
        InputState::Applied => "input.applied",
        _ => "input.rejected",
    };
    let mut payload = row.payload.clone();
    if let Some(answer) = answer {
        payload["answer"] = json!(answer);
    }
    if let Some(reason) = reason {
        payload["reason"] = json!(reason);
    }
    lifecycle::check(row.state, to)
        .map_err(|e| PersistError::IntegrityViolation(format!("input transition: {e:?}")))?;
    tx.execute(
        "UPDATE input_queue SET state=?2,payload_json=?3 WHERE input_id=?1",
        params![row.input_id, to.as_str(), payload.to_string()],
    )
    .map_err(sqlite_error)?;
    event(
        tx,
        ctx,
        &row.run_id,
        child_id(&row.input_id, to.as_str().to_ascii_lowercase().as_str()),
        kind,
        json!({"input_id":row.input_id,"from":row.state.as_str(),"to":to.as_str(),"answer":answer,"reason":reason,"boundary":payload.get("boundary"),"attempt_id":payload.get("attempt_id")}),
    )
}

/// Consume an input notice once, using its stored envelope and a durable receipt.
/// None means the first-input gate closed since the claim; the caller returns the generation.
pub(super) fn consume(
    conn: &mut Connection,
    claimed: &super::OutboxRow,
    now: std::time::SystemTime,
    ctx: &super::writer::Ctx<'_>,
) -> Result<Option<String>, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let stored: Option<String> = tx
        .query_row(
            "SELECT result_json FROM receipts WHERE consumer=?1 AND message_id=?2",
            params![claimed.recipient_role, claimed.message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let (run, role, epoch, raw): (String, String, i64, String) = tx
        .query_row(
            "SELECT run_id,recipient_role,role_epoch,payload_json FROM outbox WHERE message_id=?1",
            params![claimed.message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(sqlite_error)?;
    if claimed.run_id.as_deref() != Some(&run)
        || claimed.recipient_role != role
        || claimed.role_epoch != epoch
        || serde_json::from_str::<Value>(&raw).map_err(|e| PersistError::Storage(e.to_string()))?
            != claimed.payload
    {
        return Err(PersistError::KeyReuse(claimed.message_id.clone()));
    }
    if let Some(stored) = stored {
        check_receipt(&tx, &claimed.message_id, &stored)?;
        return Ok(Some(stored));
    }
    super::hold::writable(&tx, &run)?;
    if delivery_input_wait(&tx, &run, &role, Some(epoch), &claimed.payload)? {
        return Ok(None);
    }
    let row = read_input(&tx, &claimed.message_id)?
        .ok_or_else(|| PersistError::IntegrityViolation("missing input".to_owned()))?;
    let current: Option<i64> = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id=?1 AND role=?2",
            params![run, role],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let reason = if current != Some(epoch) {
        Some("STALE_ROLE_EPOCH")
    } else if row.state != InputState::Queued {
        Some("input_not_queued")
    } else {
        None
    };
    if row.state == InputState::Queued {
        change(
            &tx,
            &row,
            if reason.is_some() {
                InputState::Rejected
            } else {
                InputState::Delivered
            },
            None,
            reason,
            ctx,
        )?;
    }
    let result = json!({"message_id":claimed.message_id,"kind":if reason.is_some(){"rejected"}else{"ack"},"reason":reason,"input_sha256":fingerprint(&row)}).to_string();
    tx.execute(
        "INSERT INTO receipts(consumer,message_id,applied_at,result_json) VALUES (?1,?2,?3,?4)",
        params![
            role,
            claimed.message_id,
            crate::time::rfc3339_utc(now),
            result
        ],
    )
    .map_err(sqlite_error)?;
    writer::commit(tx, ctx)?;
    Ok(Some(result))
}

pub(super) fn for_wake(conn: &Connection, seqs: &[i64]) -> Result<Option<InputRow>, PersistError> {
    let id: Option<String> = conn.query_row("SELECT json_extract(payload_json,'$.input_id') FROM events WHERE kind='input.queued' AND seq IN (SELECT value FROM json_each(?1)) ORDER BY seq LIMIT 1",params![json!(seqs).to_string()],|r|r.get(0)).optional().map_err(sqlite_error)?;
    id.map(|id| read_input(conn, &id))
        .transpose()
        .map(Option::flatten)
}

pub(super) fn for_attempt(
    conn: &Connection,
    attempt: &str,
) -> Result<Option<InputRow>, PersistError> {
    let task: String = conn
        .query_row(
            "SELECT task_id FROM attempts WHERE attempt_id=?1",
            params![attempt],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let id: Option<String> = conn.query_row("SELECT json_extract(payload_json,'$.input_id') FROM events WHERE event_id=?1 AND kind='supervisor.allocated'",params![child_id(&task,"allocated")],|r|r.get(0)).optional().map_err(sqlite_error)?.flatten();
    id.map(|id| read_input(conn, &id))
        .transpose()
        .map(Option::flatten)
}

pub(super) fn finish(
    tx: &Transaction<'_>,
    attempt: &str,
    succeeded: bool,
    answer: Option<&str>,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    routing::finish_attempt(tx, attempt, ctx)?;
    let Some(row) = for_attempt(tx, attempt)?.filter(|r| r.state == InputState::Delivered) else {
        return Ok(());
    };
    if !succeeded {
        return Ok(());
    }
    let epoch: Option<i64> = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id=?1 AND role='commander'",
            params![row.run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if epoch != row.payload["role_epoch"].as_i64() {
        return change(
            tx,
            &row,
            InputState::Rejected,
            None,
            Some("STALE_ROLE_EPOCH"),
            ctx,
        );
    }
    match answer.filter(|s| !s.trim().is_empty()) {
        Some(answer) => change(tx, &row, InputState::Applied, Some(answer), None, ctx),
        None => change(
            tx,
            &row,
            InputState::Rejected,
            None,
            Some("empty_answer"),
            ctx,
        ),
    }
}

pub(super) fn exhausted(
    tx: &Transaction<'_>,
    id: &str,
    reason: &str,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    if let Some(row) = read_input(tx, id)?
        .filter(|r| matches!(r.state, InputState::Queued | InputState::Delivered))
    {
        // Exhausting transport retries does not prove that a reserved external steer was never delivered.
        let reason = if reason == "delivery_exhausted" && row.payload["delivery_pending"] == true {
            "UNKNOWN_DELIVERY"
        } else {
            reason
        };
        change(tx, &row, InputState::Rejected, None, Some(reason), ctx)?;
    }
    Ok(())
}

pub(super) fn sweep(
    conn: &mut Connection,
    project: &str,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let before = tx.total_changes();
    routing::sweep(&tx, project, ctx)?;
    let ids: Vec<String> = {
        let mut q = tx.prepare("SELECT i.input_id FROM input_queue i JOIN runs r USING(run_id) WHERE r.project_id=?1 AND (r.state IN ('COMPLETED','CANCELLED','FAILED') OR (i.state='DELIVERED' AND coalesce(i.target_role,'commander')='commander' AND coalesce((SELECT b.role_epoch FROM role_bindings b WHERE b.run_id=i.run_id AND b.role='commander'),-1) <> json_extract(i.payload_json,'$.role_epoch'))) AND i.state IN ('QUEUED','DELIVERED') ORDER BY i.rowid LIMIT 32").map_err(sqlite_error)?;
        q.query_map(params![project], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    if ids.is_empty() {
        if tx.total_changes() == before {
            // An idle poll has no write to commit and must not consume a real write's fault hook.
            return Ok(());
        }
        return writer::commit(tx, ctx);
    }
    for id in ids {
        let terminal: bool = tx.query_row("SELECT r.state IN ('COMPLETED','CANCELLED','FAILED') FROM runs r JOIN input_queue i USING(run_id) WHERE i.input_id=?1",params![id],|r|r.get(0)).map_err(sqlite_error)?;
        exhausted(
            &tx,
            &id,
            if terminal {
                "run_ended"
            } else {
                "STALE_ROLE_EPOCH"
            },
            ctx,
        )?;
    }
    writer::commit(tx, ctx)
}

/// Immutable input identity; delivery state, answer and routing metadata are deliberately excluded.
pub(super) fn fingerprint(row: &InputRow) -> String {
    use sha2::{Digest as _, Sha256};
    let identity = json!({"id":row.input_id,"run":row.run_id,"kind":row.kind,"role":row.target_role,
        "epoch":row.payload["role_epoch"],"text":row.payload["text"],"source":row.payload.get("source_message")});
    Sha256::digest(identity.to_string().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn check_receipt(conn: &Connection, id: &str, receipt: &str) -> Result<(), PersistError> {
    let value: Value =
        serde_json::from_str(receipt).map_err(|e| PersistError::Storage(e.to_string()))?;
    let row = read_input(conn, id)?
        .ok_or_else(|| PersistError::IntegrityViolation("receipt without input".into()))?;
    if value.get("message_id").is_some_and(|v| v != id)
        || value
            .get("input_sha256")
            .is_some_and(|v| v != &json!(fingerprint(&row)))
    {
        return Err(PersistError::KeyReuse(id.to_owned()));
    }
    Ok(())
}
