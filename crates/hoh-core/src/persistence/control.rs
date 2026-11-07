//! Pause and cancel apart from the scheduler (REQ-043, docs/adr/ADR-021.md decisions 7 and 8): the record of one
//! cancel pass over an attempt's processes, and the run's last cancel transaction once nothing of it runs.

use super::attempts::{self, AttemptFacts, AttemptMove};
use super::writer::{self, Change, Commit, Ctx, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::cancel::{PassEnd, Signal};
use crate::ids::child_id;
use crate::lifecycle::AttemptState;
use crate::protocol::{RunState, TaskState};
use crate::time::rfc3339_utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::time::SystemTime;

/// One cancel pass over an attempt's processes, as the host drove it (ADR-021 decision 8c).
#[derive(Debug, Clone, PartialEq)]
pub struct CancelPass {
    pub attempt_id: String,
    /// What was sent, when, in order.
    pub sent: Vec<(Signal, SystemTime)>,
    pub end: PassEnd,
    /// When the pass ended.
    pub at: SystemTime,
}

/// What [`finish_cancel`] came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finished {
    /// The run is CANCELLED.
    Cancelled,
    /// Attempts of it still start or run: the run stays CANCEL_REQUESTED.
    Running(usize),
    /// The run is not cancelling (anymore).
    NotCancelling,
}

pub(super) enum ControlJob {
    CancelPass(CancelPass),
    FinishCancel {
        run_id: String,
        workspace: Value,
        at: SystemTime,
    },
}

pub(super) enum ControlDone {
    Recorded(i64),
    Finished(Finished),
}

pub(super) fn run(
    conn: &mut Connection,
    job: ControlJob,
    ctx: &Ctx<'_>,
) -> Result<ControlDone, PersistError> {
    match job {
        ControlJob::CancelPass(pass) => record_pass(conn, &pass, ctx).map(ControlDone::Recorded),
        ControlJob::FinishCancel {
            run_id,
            workspace,
            at,
        } => finish_cancel(conn, &run_id, &workspace, at, ctx).map(ControlDone::Finished),
    }
}

/// The steps of one pass and how it ended, committed together in their order (ADR-019 decision 7); a pass that found
/// the group empty ends the attempt CANCELLED with its time charged (ADR-020 decision 7) and its leases released.
fn record_pass(
    conn: &mut Connection,
    pass: &CancelPass,
    ctx: &Ctx<'_>,
) -> Result<i64, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let row: Option<(String, String, String, String)> = tx
        .query_row(
            "SELECT a.state, a.task_id, t.run_id, r.project_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             JOIN runs r ON r.run_id = t.run_id WHERE a.attempt_id = ?1",
            params![pass.attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, _task_id, run_id, project_id)) = row else {
        return Err(PersistError::IntegrityViolation(format!(
            "no attempt {}",
            pass.attempt_id
        )));
    };
    let state = writer::parse::<AttemptState>(&state)?;
    let number: i64 = tx
        .query_row(
            "SELECT count(*) FROM events WHERE kind IN ('cancel.reaped', 'cancel.lost', 'cancel.unreaped') \
             AND json_extract(payload_json, '$.attempt_id') = ?1",
            params![pass.attempt_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let event = |kind: &str, at: SystemTime| NewEvent {
        project_id: project_id.clone(),
        event_id: child_id(&pass.attempt_id, &format!("cancel/{number}/{kind}")),
        run_id: Some(run_id.clone()),
        kind: format!("cancel.{kind}"),
        payload: json!({
            "attempt_id": pass.attempt_id,
            "at": rfc3339_utc(at),
            "at_ms": at.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()),
        }),
    };
    let mut events: Vec<NewEvent> = pass
        .sent
        .iter()
        .map(|(signal, at)| event(signal.as_str(), *at))
        .collect();
    let end = match pass.end {
        PassEnd::Empty => "reaped",
        PassEnd::Lost => "lost",
        PassEnd::Unreaped => "unreaped",
    };
    events.push(event(end, pass.at));
    if pass.end == PassEnd::Empty && matches!(state, AttemptState::Starting | AttemptState::Running)
    {
        attempts::move_attempt(
            &tx,
            &AttemptMove {
                attempt_id: pass.attempt_id.clone(),
                from: state,
                to: AttemptState::Cancelled,
                facts: AttemptFacts {
                    ended_at: Some(rfc3339_utc(pass.at)),
                    ..AttemptFacts::default()
                },
            },
            ctx,
        )?;
        // Its work, started and never observed, is not known to have happened or not (ADR-021 decision 8e).
        tx.execute(
            "UPDATE effects SET state = 'UNKNOWN' WHERE attempt_id = ?1 AND state = 'STARTED'",
            params![pass.attempt_id],
        )
        .map_err(sqlite_error)?;
        super::retry::accrue_wall(&tx, &run_id, &pass.attempt_id, pass.at, None)?;
    }
    let done = writer::write(
        &tx,
        &Commit {
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    tx.commit().map_err(sqlite_error)?;
    Ok(done.event_seqs.last().copied().unwrap_or(0))
}

/// The run's last cancel transaction (ADR-021 decision 8f), once none of its attempts starts or runs: its workspace
/// record, its attempts not yet started and its open tasks CANCELLED, its pending control notices acknowledged, then
/// the run CANCELLED. No candidate is touched, so an unfinished one is never marked verified (REQ-043 behaviour 4).
fn finish_cancel(
    conn: &mut Connection,
    run_id: &str,
    workspace: &Value,
    at: SystemTime,
    ctx: &Ctx<'_>,
) -> Result<Finished, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let run: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT state, revision, project_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, revision, project_id)) = run else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    if writer::parse::<RunState>(&state)? != RunState::CancelRequested {
        return Ok(Finished::NotCancelling);
    }
    let running: i64 = tx
        .query_row(
            "SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1 \
             AND a.state IN ('STARTING', 'RUNNING')",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if running > 0 {
        return Ok(Finished::Running(
            usize::try_from(running).unwrap_or(usize::MAX),
        ));
    }
    let prepared: Vec<String> = {
        let mut statement = tx
            .prepare(
                "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1 \
                 AND a.state = 'PREPARED' ORDER BY a.rowid",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![run_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for attempt_id in prepared {
        attempts::move_attempt(
            &tx,
            &AttemptMove {
                attempt_id,
                from: AttemptState::Prepared,
                to: AttemptState::Cancelled,
                facts: AttemptFacts {
                    ended_at: Some(rfc3339_utc(at)),
                    ..AttemptFacts::default()
                },
            },
            ctx,
        )?;
    }
    let tasks: Vec<(String, i64)> = {
        let mut statement = tx
            .prepare(
                "SELECT task_id, revision FROM tasks WHERE run_id = ?1 AND state NOT IN ('ACCEPTED', 'CANCELLED') \
                 ORDER BY created_seq, task_id",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    tx.execute(
        "UPDATE outbox SET state = 'ACKED' WHERE run_id = ?1 AND state = 'PENDING' \
         AND json_extract(payload_json, '$.kind') = 'run.control'",
        params![run_id],
    )
    .map_err(sqlite_error)?;
    let mut cas: Vec<StateCas> = tasks
        .into_iter()
        .map(|(id, expected_revision)| StateCas {
            id,
            expected_revision,
            change: Change::Task(TaskState::Cancelled),
        })
        .collect();
    cas.push(StateCas {
        id: run_id.to_owned(),
        expected_revision: revision,
        change: Change::Run(RunState::Cancelled),
    });
    let event = |kind: &str, payload: Value| NewEvent {
        project_id: project_id.clone(),
        event_id: child_id(run_id, kind),
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    writer::write(
        &tx,
        &Commit {
            cas,
            events: vec![
                event("cancel.workspace", workspace.clone()),
                event(
                    "run.cancelled",
                    json!({"state": "CANCELLED", "at": rfc3339_utc(at)}),
                ),
            ],
            ..Commit::default()
        },
        ctx,
    )?;
    tx.commit().map_err(sqlite_error)?;
    Ok(Finished::Cancelled)
}

#[cfg(test)]
mod tests;
