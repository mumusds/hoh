//! SetMode and its durable safe point (REQ-007; ADR-036).

use super::commands::SubmitCommand;
use super::writer::{self, Change, Commit, Ctx, Entity, Intent, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{Mode, RunState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// The latest pending command, unless a later change already fulfilled it.
pub(super) fn pending(conn: &Connection, run: &str) -> Result<Option<Value>, PersistError> {
    let raw: Option<String> = conn.query_row(
        "SELECT payload_json FROM events WHERE run_id = ?1 AND kind = 'run.mode_change_pending' \
         AND seq > (SELECT coalesce(max(seq), 0) FROM events WHERE run_id = ?1 AND kind = 'run.mode_changed') \
         ORDER BY seq DESC LIMIT 1", params![run], |r| r.get(0),
    ).optional().map_err(sqlite_error)?;
    raw.map(|s| {
        serde_json::from_str(&s)
            .map_err(|e| PersistError::IntegrityViolation(format!("pending mode of {run}: {e}")))
    })
    .transpose()
}

fn writing(conn: &Connection, run: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks t JOIN attempts a ON a.task_id = t.task_id \
         WHERE t.run_id = ?1 AND t.writer = 1 AND a.state IN ('PREPARED','STARTING','RUNNING'))",
        params![run],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

pub(super) fn set(
    tx: &Transaction<'_>,
    command: &SubmitCommand,
    mode: Mode,
    ctx: &Ctx<'_>,
) -> Result<(i64, i64), PersistError> {
    let run = super::commands::snapshot_rows(tx, &command.run_id)?
        .map(|(run, _)| run)
        .ok_or_else(|| PersistError::RunNotFound(command.run_id.clone()))?;
    let from: Mode = serde_json::from_value(json!(run.mode))
        .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
    let transition = crate::mode::transition(
        writer::parse::<RunState>(&run.state)?,
        from,
        mode,
        writing(tx, &run.run_id)?,
    );
    if transition == crate::mode::Transition::Refused {
        return Err(PersistError::IllegalTransition {
            entity: Entity::Run,
            id: run.run_id,
            from: run.state.clone(),
            to: "SetMode".to_owned(),
            why: "an ended run cannot change mode".to_owned(),
            details: Some(Box::new(json!({"state":run.state}))),
        });
    }
    if run.revision != command.expected_revision {
        return Err(PersistError::StaleRevision {
            entity: Entity::Run,
            id: run.run_id,
            expected: command.expected_revision,
            actual: Some(run.revision),
        });
    }
    let waiting = transition == crate::mode::Transition::Pending;
    change(tx, &run, &command.command_id, mode, waiting, true, ctx)
}

fn change(
    tx: &Transaction<'_>,
    run: &super::commands::RunRow,
    command: &str,
    mode: Mode,
    waiting: bool,
    touch: bool,
    ctx: &Ctx<'_>,
) -> Result<(i64, i64), PersistError> {
    let to = match mode {
        Mode::Plan => "PLAN",
        Mode::Build => "BUILD",
        Mode::Auto => "AUTO",
    };
    if !waiting {
        tx.execute(
            "UPDATE runs SET mode = ?2 WHERE run_id = ?1",
            params![run.run_id, to],
        )
        .map_err(sqlite_error)?;
    }
    let done = writer::write(
        tx,
        &Commit {
            cas: if touch {
                vec![StateCas {
                    id: run.run_id.clone(),
                    expected_revision: run.revision,
                    change: Change::Touch(Entity::Run),
                }]
            } else {
                Vec::new()
            },
            events: vec![NewEvent {
                intent: Intent::Progress,
                project_id: run.project_id.clone(),
                run_id: Some(run.run_id.clone()),
                event_id: crate::ids::child_id(
                    command,
                    if waiting {
                        "mode_pending"
                    } else {
                        "mode_changed"
                    },
                ),
                kind: if waiting {
                    "run.mode_change_pending"
                } else {
                    "run.mode_changed"
                }
                .to_owned(),
                payload: json!({"from":run.mode,"to":to,"command_id":command}),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok((
        done.revisions.first().copied().unwrap_or(run.revision),
        done.event_seqs[0],
    ))
}

pub(super) fn step(conn: &mut Connection, run: &str, ctx: &Ctx<'_>) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let Some(pending) = pending(&tx, run)? else {
        return Ok(false);
    };
    let row = super::commands::snapshot_rows(&tx, run)?
        .map(|(run, _)| run)
        .ok_or_else(|| PersistError::RunNotFound(run.to_owned()))?;
    if writer::parse::<RunState>(&row.state)?.is_terminal() || writing(&tx, run)? {
        return Ok(false);
    }
    let command = pending["command_id"].as_str().ok_or_else(|| {
        PersistError::IntegrityViolation("pending mode has no command_id".to_owned())
    })?;
    let mode: Mode = serde_json::from_value(pending["to"].clone())
        .map_err(|e| PersistError::IntegrityViolation(format!("pending mode: {e}")))?;
    change(&tx, &row, command, mode, false, true, ctx)?;
    writer::commit(tx, ctx)?;
    Ok(true)
}

pub(super) fn waiting(conn: &Connection, project: &str) -> Result<Vec<String>, PersistError> {
    let mut stmt = conn.prepare(
        "SELECT r.run_id FROM runs r WHERE r.project_id = ?1 AND r.state NOT IN ('COMPLETED','FAILED','CANCELLED','INCOMPLETE') \
         AND (SELECT max(seq) FROM events WHERE run_id = r.run_id AND kind = 'run.mode_change_pending') \
         > (SELECT coalesce(max(seq),0) FROM events WHERE run_id = r.run_id AND kind = 'run.mode_changed')"
    ).map_err(sqlite_error)?;
    stmt.query_map(params![project], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)
}

/// A mode intent derived by the Runtime from an explicit user action, using SetMode's safe point.
pub(super) fn intent(
    tx: &Transaction<'_>,
    run_id: &str,
    command: &str,
    mode: Mode,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let row = super::commands::snapshot_rows(tx, run_id)?
        .map(|(r, _)| r)
        .ok_or_else(|| PersistError::RunNotFound(run_id.into()))?;
    change(tx, &row, command, mode, writing(tx, run_id)?, false, ctx)?;
    Ok(())
}
