//! The input queue in the store (docs/06-orchestration.md: QUEUED -> DELIVERED -> APPLIED/REJECTED). An input is
//! created QUEUED by whoever accepted it and moved by whoever delivers it (an adapter, for a steer); like an attempt
//! it has no revision column, so a move compares on the state it expects (another one is `StaleState`) and follows
//! `lifecycle::InputState` (`ILLEGAL_TRANSITION` otherwise). Each write appends its event in the same transaction.

use super::decide::child_id;
use super::writer::{self, Commit, Entity, NewEvent};
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
    tx.execute(
        "INSERT INTO input_queue(input_id, run_id, kind, target_role, state, payload_json) \
         VALUES (?1, ?2, ?3, ?4, 'QUEUED', ?5)",
        params![
            i.input_id,
            i.run_id,
            i.kind,
            i.target_role,
            i.payload.to_string()
        ],
    )
    .map_err(sqlite_error)?;
    event(
        tx,
        ctx,
        &i.run_id,
        child_id(&i.input_id, InputState::Queued.as_str()),
        "input.queued",
        json!({"input_id": i.input_id, "kind": i.kind, "to": InputState::Queued.as_str()}),
    )
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

#[cfg(test)]
mod tests;
