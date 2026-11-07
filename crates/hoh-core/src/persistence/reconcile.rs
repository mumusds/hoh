//! The reconcile at a host's start, its reads and writes (REQ-041 b1–b2; docs/adr/ADR-019.md decision 5): the
//! project's unfinished attempts with their tasks and effects, then per attempt one transaction with its
//! disposition, its effects, its task and its events, between a `reconcile.started` and a `reconcile.completed`.

use super::attempts::{self, AttemptFacts, AttemptMove};
use super::writer::{self, Change, Commit, Ctx, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::{AttemptState, Lifecycle};
use crate::protocol::TaskState;
use crate::reconcile::{Disposition, EffectMove, EffectState, Process};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

/// One effect of an unfinished attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRow {
    pub key: String,
    pub state: EffectState,
    pub external_ref: Option<String>,
}

/// An unfinished attempt (PREPARED, STARTING or RUNNING) with its task, its process record and its effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnfinishedRow {
    pub project_id: String,
    pub run_id: String,
    pub attempt_id: String,
    pub task_id: String,
    pub state: AttemptState,
    pub task_state: TaskState,
    pub task_revision: i64,
    pub pid: Option<i64>,
    pub boot_id: Option<String>,
    pub process_start_id: Option<String>,
    pub effects: Vec<EffectRow>,
    /// A lease it holds expired at the time of the scan.
    pub lease_expired: bool,
    /// The epoch of the lease it holds (the latest), if any.
    pub lease_epoch: Option<i64>,
    /// The outcome its work's `effect.observed` event recorded, if any.
    pub work_outcome: Option<bool>,
}

/// One step of a reconcile.
#[derive(Debug, Clone)]
pub(super) enum ReconcileJob {
    Scan(String, std::time::SystemTime),
    Started {
        project_id: String,
        host_epoch: i64,
        unfinished: usize,
    },
    /// The disposition, what was found of the process, and why (`reconcile`, `lease_expired`).
    Apply(Box<(UnfinishedRow, Disposition, Process, &'static str)>),
    Completed {
        project_id: String,
        host_epoch: i64,
        summary: Value,
    },
}

/// What a step gave: the rows scanned, or the seq of the step's (last) event.
#[derive(Debug)]
pub(super) enum ReconcileDone {
    Scanned(Vec<UnfinishedRow>),
    Event(i64),
}

pub(super) fn run(
    conn: &mut Connection,
    job: ReconcileJob,
    ctx: &Ctx<'_>,
) -> Result<ReconcileDone, PersistError> {
    match job {
        ReconcileJob::Scan(project_id, now) => {
            scan(conn, &project_id, now).map(ReconcileDone::Scanned)
        }
        ReconcileJob::Started {
            project_id,
            host_epoch,
            unfinished,
        } => host_event(
            conn,
            ctx,
            &project_id,
            host_epoch,
            "reconcile.started",
            json!({"host_epoch": host_epoch, "unfinished": unfinished}),
        ),
        ReconcileJob::Apply(step) => {
            let (row, disposition, process, cause) = *step;
            apply(conn, &row, &disposition, &process, cause, ctx)
        }
        ReconcileJob::Completed {
            project_id,
            host_epoch,
            summary,
        } => host_event(
            conn,
            ctx,
            &project_id,
            host_epoch,
            "reconcile.completed",
            json!({"host_epoch": host_epoch, "attempts": summary}),
        ),
    }
}

/// The project's unfinished attempts in the order they were made, as of `now`.
fn scan(
    conn: &Connection,
    project_id: &str,
    now: std::time::SystemTime,
) -> Result<Vec<UnfinishedRow>, PersistError> {
    type Raw = (
        (String, String, String, String, String, i64),
        (
            Option<i64>,
            Option<String>,
            Option<String>,
            bool,
            Option<i64>,
        ),
    );
    let mut statement = conn
        .prepare(
            "SELECT r.run_id, a.attempt_id, a.task_id, a.state, t.state, t.revision, a.pid, a.boot_id, \
             a.process_start_id, EXISTS(SELECT 1 FROM leases l WHERE l.attempt_id = a.attempt_id AND l.expires_at <= ?2), \
             (SELECT max(l.epoch) FROM leases l WHERE l.attempt_id = a.attempt_id) \
             FROM attempts a JOIN tasks t ON t.task_id = a.task_id JOIN runs r ON r.run_id = t.run_id \
             WHERE r.project_id = ?1 AND a.state IN ('PREPARED', 'STARTING', 'RUNNING') ORDER BY a.rowid",
        )
        .map_err(sqlite_error)?;
    let raws: Vec<Raw> = statement
        .query_map(params![project_id, crate::time::rfc3339_utc(now)], |r| {
            Ok((
                (
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ),
                (r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?),
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut rows = Vec::with_capacity(raws.len());
    for (ids, process) in raws {
        let (run_id, attempt_id, task_id, state, task_state, task_revision) = ids;
        let (pid, boot_id, start, lease_expired, lease_epoch) = process;
        let effects = effects_of(conn, &attempt_id)?;
        let observed = child_id(&crate::reconcile::work_key(&attempt_id), "effect.observed");
        let work_outcome: Option<bool> = conn
            .query_row(
                "SELECT json_extract(payload_json, '$.detail.success') FROM events WHERE event_id = ?1",
                params![observed],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?
            .flatten();
        rows.push(UnfinishedRow {
            project_id: project_id.to_owned(),
            run_id,
            state: writer::parse::<AttemptState>(&state)?,
            task_state: writer::parse::<TaskState>(&task_state)?,
            attempt_id,
            task_id,
            task_revision,
            pid,
            boot_id,
            process_start_id: start,
            effects,
            lease_expired,
            lease_epoch,
            work_outcome,
        });
    }
    Ok(rows)
}

fn effects_of(conn: &Connection, attempt_id: &str) -> Result<Vec<EffectRow>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT effect_key, state, external_ref FROM effects WHERE attempt_id = ?1 ORDER BY rowid",
        )
        .map_err(sqlite_error)?;
    let raws: Vec<(String, String, Option<String>)> = statement
        .query_map(params![attempt_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    raws.into_iter()
        .map(|(key, state, external_ref)| {
            let state = EffectState::parse(&state).ok_or_else(|| {
                PersistError::IntegrityViolation(format!("effect {key}: unknown state {state:?}"))
            })?;
            Ok(EffectRow {
                key,
                state,
                external_ref,
            })
        })
        .collect()
}

/// A host-level event (no run) of the reconcile.
fn host_event(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    project_id: &str,
    host_epoch: i64,
    kind: &str,
    payload: Value,
) -> Result<ReconcileDone, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let done = writer::write(
        &tx,
        &Commit {
            events: vec![NewEvent {
                project_id: project_id.to_owned(),
                event_id: child_id(&format!("host-epoch/{host_epoch}"), kind),
                run_id: None,
                kind: kind.to_owned(),
                payload,
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    tx.commit().map_err(sqlite_error)?;
    Ok(ReconcileDone::Event(
        done.event_seqs.last().copied().unwrap_or(0),
    ))
}

fn process_json(process: &Process) -> Value {
    match process {
        Process::NotStarted => json!({"found": "not_started"}),
        Process::Gone { basis } => json!({"found": "gone", "basis": basis, "signals": 0}),
        Process::Stopped { steps, reaped } => {
            json!({"found": "ours", "steps": steps, "reaped": reaped})
        }
        Process::Uncertain { basis } => json!({"found": "uncertain", "basis": basis, "signals": 0}),
    }
}

/// One attempt's disposition in one transaction: its effects, its move (which lets its leases go at an end), its
/// task and its events (a diagnostic too when an effect is unknown or the process could not be reaped).
fn apply(
    conn: &mut Connection,
    row: &UnfinishedRow,
    disposition: &Disposition,
    process: &Process,
    cause: &str,
    ctx: &Ctx<'_>,
) -> Result<ReconcileDone, PersistError> {
    let none: &[EffectMove] = &[];
    let (name, attempt_to, task_to, moves, unknown): (_, _, _, &[EffectMove], &[String]) =
        match disposition {
            Disposition::Redeliver => ("redeliver", None, None, none, &[]),
            Disposition::Unreaped => ("unreaped", None, None, none, &[]),
            Disposition::Ended {
                to,
                task_to,
                effects,
            } => ("ended", Some(*to), Some(*task_to), effects.as_slice(), &[]),
            Disposition::Completed { effects, task_to } => (
                "completed",
                Some(AttemptState::Settled),
                *task_to,
                effects.as_slice(),
                &[],
            ),
            Disposition::UnknownEffect { effects, unknown } => (
                "unknown_effect",
                Some(AttemptState::UnknownEffect),
                None,
                effects.as_slice(),
                unknown.as_slice(),
            ),
        };
    let tx = writer::begin(conn, ctx)?;
    // An attempt left unfinished (redelivered, unreaped) is disposed of again later: each pass its own event ids.
    let pass: i64 = tx
        .query_row(
            "SELECT count(*) FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.attempt_id') = ?1",
            params![row.attempt_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    for m in moves {
        let changed = tx
            .execute(
                "UPDATE effects SET state = ?1, external_ref = COALESCE(?2, external_ref) \
                 WHERE effect_key = ?3 AND attempt_id = ?4 AND state = ?5",
                params![
                    m.to.as_str(),
                    m.external_ref,
                    m.key,
                    row.attempt_id,
                    m.from.as_str()
                ],
            )
            .map_err(sqlite_error)?;
        if changed != 1 {
            return Err(PersistError::IntegrityViolation(format!(
                "effect {} of {} is no longer {}",
                m.key,
                row.attempt_id,
                m.from.as_str()
            )));
        }
    }
    if let Some(to) = attempt_to {
        attempts::move_attempt(
            &tx,
            &AttemptMove {
                attempt_id: row.attempt_id.clone(),
                from: row.state,
                to,
                facts: AttemptFacts {
                    ended_at: Some(crate::time::now_rfc3339()),
                    ..AttemptFacts::default()
                },
            },
            ctx,
        )?;
    }
    let mut events = vec![NewEvent {
        project_id: row.project_id.clone(),
        event_id: child_id(&row.attempt_id, &format!("{cause}/{pass}/attempt")),
        run_id: Some(row.run_id.clone()),
        kind: "reconcile.attempt".to_owned(),
        payload: json!({
            "attempt_id": row.attempt_id,
            "task_id": row.task_id,
            "cause": cause,
            "disposition": name,
            "attempt": {"from": row.state.as_str(), "to": attempt_to.map(|s| s.as_str())},
            "task": {"from": row.task_state.as_str(), "to": task_to.map(|s| s.as_str())},
            "process": process_json(process),
            "recorded": {"pid": row.pid, "boot_id": row.boot_id, "process_start_id": row.process_start_id},
            "effects": moves.iter().map(|m| json!({
                "effect_key": m.key, "from": m.from.as_str(), "to": m.to.as_str(), "external_ref": m.external_ref,
            })).collect::<Vec<_>>(),
        }),
    }];
    let diagnostic = match disposition {
        Disposition::UnknownEffect { .. } => Some(json!({
            "code": "UNKNOWN_EFFECT", "attempt_id": row.attempt_id, "effect_keys": unknown,
            "detail": "an effect that started cannot be confirmed; its task gets no new attempt by itself",
        })),
        Disposition::Unreaped => Some(json!({
            "code": "PROCESS_NOT_REAPED", "attempt_id": row.attempt_id, "process": process_json(process),
            "detail": "its process group may still run (it outlived its stop, or cannot be told apart); its lease stays held and no writer lease is granted",
        })),
        _ => None,
    };
    if let Some(payload) = diagnostic {
        events.push(NewEvent {
            project_id: row.project_id.clone(),
            event_id: child_id(&row.attempt_id, &format!("{cause}/{pass}/diagnostic")),
            run_id: Some(row.run_id.clone()),
            kind: "reconcile.diagnostic".to_owned(),
            payload,
        });
    }
    let cas = task_to
        .map(|to| StateCas {
            id: row.task_id.clone(),
            expected_revision: row.task_revision,
            change: Change::Task(to),
        })
        .into_iter()
        .collect();
    let done = writer::write(
        &tx,
        &Commit {
            cas,
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    tx.commit().map_err(sqlite_error)?;
    Ok(ReconcileDone::Event(
        done.event_seqs.last().copied().unwrap_or(0),
    ))
}
