//! A run's project baseline (REQ-028; docs/adr/ADR-039.md decisions 4 and 5): the one `run.baseline` event the host's
//! read-only discovery writes while the run is in DISCOVERING, and the runs still waiting for it. Never a task, never a
//! revision of the run.

use super::writer::{self, Commit, Ctx, Intent, NewEvent};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::protocol::RunState;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// The kind of the baseline event.
pub(super) const KIND: &str = "run.baseline";

/// The id of run `run_id`'s baseline event: one per run.
fn event_id(run_id: &str) -> String {
    child_id(run_id, "baseline")
}

/// Whether run `run_id` has its baseline.
pub(super) fn recorded(conn: &Connection, run_id: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE event_id = ?1)",
        params![event_id(run_id)],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The project's runs in DISCOVERING without their baseline, oldest first.
pub(super) fn waiting(conn: &Connection, project_id: &str) -> Result<Vec<String>, PersistError> {
    let runs: Vec<String> = conn
        .prepare("SELECT run_id FROM runs WHERE project_id = ?1 AND state = 'DISCOVERING' ORDER BY rowid")
        .map_err(sqlite_error)?
        .query_map(params![project_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
    for run in runs {
        if !recorded(conn, &run)? {
            out.push(run);
        }
    }
    Ok(out)
}

/// Write run `run_id`'s baseline `payload` (the discovery's object; the run id is added): only while the run is in
/// DISCOVERING and has none, else nothing is written. Whether it was written.
pub(super) fn record(
    conn: &mut Connection,
    run_id: &str,
    payload: Value,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let run: Option<(String, String)> = tx
        .query_row(
            "SELECT project_id, state FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, state)) = run else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    if writer::parse::<RunState>(&state)? != RunState::Discovering || recorded(&tx, run_id)? {
        return Ok(false);
    }
    let Value::Object(fields) = payload else {
        return Err(PersistError::IntegrityViolation(
            "a baseline is an object".to_owned(),
        ));
    };
    let mut payload = serde_json::Map::new();
    payload.insert("run_id".to_owned(), Value::String(run_id.to_owned()));
    payload.extend(fields);
    writer::write(
        &tx,
        &Commit {
            events: vec![NewEvent {
                intent: Intent::Progress,
                project_id,
                event_id: event_id(run_id),
                run_id: Some(run_id.to_owned()),
                kind: KIND.to_owned(),
                payload: Value::Object(payload),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    writer::commit(tx, ctx)?;
    Ok(true)
}
