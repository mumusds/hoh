//! The host runner's writes (docs/adr/ADR-019.md decision 8): what is waiting for it (dispatch and run.started
//! notices, the working runs) and each step of an attempt it runs, every step one transaction:
//!
//! 1. received: the dispatch's receipt, its notice ACKED, the attempt STARTING and its work effect PREPARED;
//! 2. running: the process started and held at its start gate; the attempt RUNNING with its process identity, its
//!    work effect STARTED (write-ahead) and its task RUNNING, before the gate opens;
//! 3. the work effect OBSERVED with its outcome when the process ended, then COMMITTED;
//! 4. finished: the attempt SETTLED (its task IMPLEMENTED) or FAILED (its task READY).

use super::attempts::{self, AttemptFacts, AttemptMove};
use super::writer::{self, Change, Commit, Ctx, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::{AttemptState, Lifecycle};
use crate::protocol::TaskState;
use crate::reconcile::EffectState;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// The consumer named in the receipts of the runner's notices.
pub const RUNNER: &str = "runner";

/// A dispatch notice waiting for the runner: its attempt is still PREPARED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchNotice {
    pub message_id: String,
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role: String,
}

/// What is waiting for the runner in a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Waiting {
    pub dispatches: Vec<DispatchNotice>,
    /// run.started notices not yet taken: (message id, run id).
    pub started: Vec<(String, String)>,
    /// The runs that dispatch (RUNNING, VERIFYING).
    pub runs: Vec<String>,
}

/// One step of the runner.
#[derive(Debug, Clone)]
pub(super) enum RunnerJob {
    Waiting(String),
    Received(Box<DispatchNotice>),
    Effect {
        attempt_id: String,
        key: String,
        from: EffectState,
        to: EffectState,
        detail: Value,
    },
    Running {
        attempt_id: String,
        facts: Box<AttemptFacts>,
    },
    Finished {
        attempt_id: String,
        success: bool,
        detail: String,
    },
    Ack(String),
    /// Renew the unexpired leases these attempts hold, at `now` (REQ-041: every 30 s, for 120 s).
    Renew(Vec<String>, std::time::SystemTime),
}

/// What a step gave.
#[derive(Debug)]
pub(super) enum RunnerDone {
    Waiting(Waiting),
    /// Whether the step applied (a dispatch already taken or an attempt no longer PREPARED is not).
    Applied(bool),
    /// How many leases were renewed.
    Renewed(usize),
}

pub(super) fn run(
    conn: &mut Connection,
    job: RunnerJob,
    ctx: &Ctx<'_>,
) -> Result<RunnerDone, PersistError> {
    match job {
        RunnerJob::Waiting(project_id) => waiting(conn, &project_id).map(RunnerDone::Waiting),
        RunnerJob::Received(notice) => received(conn, &notice, ctx).map(RunnerDone::Applied),
        RunnerJob::Effect {
            attempt_id,
            key,
            from,
            to,
            detail,
        } => effect(conn, &attempt_id, &key, from, to, detail, ctx).map(RunnerDone::Applied),
        RunnerJob::Running { attempt_id, facts } => {
            running(conn, &attempt_id, *facts, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Finished {
            attempt_id,
            success,
            detail,
        } => finished(conn, &attempt_id, success, &detail, ctx).map(RunnerDone::Applied),
        RunnerJob::Renew(attempts, now) => {
            let tx = writer::begin(conn, ctx)?;
            let at = crate::time::rfc3339_utc(now);
            let until = crate::time::rfc3339_utc(now + super::LEASE_TTL);
            let mut renewed = 0;
            for attempt_id in &attempts {
                // Only a lease still unexpired: an expired one is the runner's to settle, never to revive.
                renewed += tx
                    .execute(
                        "UPDATE leases SET expires_at = ?1 WHERE attempt_id = ?2 AND expires_at > ?3",
                        params![until, attempt_id, at],
                    )
                    .map_err(sqlite_error)?;
            }
            tx.commit().map_err(sqlite_error)?;
            Ok(RunnerDone::Renewed(renewed))
        }
        RunnerJob::Ack(message_id) => {
            let tx = writer::begin(conn, ctx)?;
            let changed = tx
                .execute(
                    "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE message_id = ?1 \
                     AND state IN ('PENDING', 'DELIVERING') AND json_extract(payload_json, '$.notice') IS 'runtime'",
                    params![message_id],
                )
                .map_err(sqlite_error)?;
            tx.commit().map_err(sqlite_error)?;
            Ok(RunnerDone::Applied(changed == 1))
        }
    }
}

fn waiting(conn: &Connection, project_id: &str) -> Result<Waiting, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT o.message_id, o.run_id, json_extract(o.payload_json, '$.task_id'), \
             json_extract(o.payload_json, '$.attempt_id'), json_extract(o.payload_json, '$.role') \
             FROM outbox o JOIN runs r ON r.run_id = o.run_id \
             JOIN attempts a ON a.attempt_id = json_extract(o.payload_json, '$.attempt_id') \
             WHERE r.project_id = ?1 AND o.state = 'PENDING' AND json_extract(o.payload_json, '$.notice') IS 'runtime' \
             AND json_extract(o.payload_json, '$.kind') IS 'dispatch' AND a.state = 'PREPARED' ORDER BY o.event_seq",
        )
        .map_err(sqlite_error)?;
    let dispatches = statement
        .query_map(params![project_id], |r| {
            Ok(DispatchNotice {
                message_id: r.get(0)?,
                run_id: r.get(1)?,
                task_id: r.get(2)?,
                attempt_id: r.get(3)?,
                role: r.get(4)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut statement = conn
        .prepare(
            "SELECT o.message_id, o.run_id FROM outbox o JOIN runs r ON r.run_id = o.run_id \
             WHERE r.project_id = ?1 AND o.state = 'PENDING' AND json_extract(o.payload_json, '$.notice') IS 'runtime' \
             AND json_extract(o.payload_json, '$.kind') IS 'run.started' ORDER BY o.event_seq",
        )
        .map_err(sqlite_error)?;
    let started = statement
        .query_map(params![project_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut statement = conn
        .prepare(
            "SELECT run_id FROM runs WHERE project_id = ?1 AND state IN ('RUNNING', 'VERIFYING') ORDER BY created_at, run_id",
        )
        .map_err(sqlite_error)?;
    let runs = statement
        .query_map(params![project_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(Waiting {
        dispatches,
        started,
        runs,
    })
}

/// The task of `attempt_id`, its state and revision, its run and project.
fn task_of(
    tx: &Transaction<'_>,
    attempt_id: &str,
) -> Result<(String, TaskState, i64, String, String), PersistError> {
    let row: Option<(String, String, i64, String, String)> = tx
        .query_row(
            "SELECT t.task_id, t.state, t.revision, t.run_id, r.project_id FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id JOIN runs r ON r.run_id = t.run_id WHERE a.attempt_id = ?1",
            params![attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let (task_id, state, revision, run_id, project_id) =
        row.ok_or_else(|| PersistError::IntegrityViolation(format!("no attempt {attempt_id}")))?;
    Ok((
        task_id,
        writer::parse::<TaskState>(&state)?,
        revision,
        run_id,
        project_id,
    ))
}

fn event(project_id: &str, run_id: &str, id: String, kind: &str, payload: Value) -> NewEvent {
    NewEvent {
        project_id: project_id.to_owned(),
        event_id: id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    }
}

fn received(
    conn: &mut Connection,
    notice: &DispatchNotice,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let taken = tx
        .execute(
            "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE message_id = ?1 AND state = 'PENDING'",
            params![notice.message_id],
        )
        .map_err(sqlite_error)?;
    let prepared: Option<String> = tx
        .query_row(
            "SELECT state FROM attempts WHERE attempt_id = ?1",
            params![notice.attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if taken != 1 || prepared.as_deref() != Some(AttemptState::Prepared.as_str()) {
        return Ok(false);
    }
    let now = crate::time::now_rfc3339();
    tx.execute(
        "INSERT INTO receipts(consumer, message_id, applied_at, result_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            RUNNER,
            notice.message_id,
            now,
            json!({"received": notice.attempt_id}).to_string()
        ],
    )
    .map_err(sqlite_error)?;
    attempts::move_attempt(
        &tx,
        &AttemptMove {
            attempt_id: notice.attempt_id.clone(),
            from: AttemptState::Prepared,
            to: AttemptState::Starting,
            facts: AttemptFacts {
                started_at: Some(now),
                ..AttemptFacts::default()
            },
        },
        ctx,
    )?;
    let key = crate::reconcile::work_key(&notice.attempt_id);
    tx.execute(
        "INSERT INTO effects(effect_key, attempt_id, state) VALUES (?1, ?2, 'PREPARED')",
        params![key, notice.attempt_id],
    )
    .map_err(sqlite_error)?;
    let (_, _, _, run_id, project_id) = task_of(&tx, &notice.attempt_id)?;
    writer::write(
        &tx,
        &Commit {
            events: vec![event(
                &project_id,
                &run_id,
                child_id(&notice.attempt_id, "received"),
                "runner.received",
                json!({"attempt_id": notice.attempt_id, "task_id": notice.task_id, "message_id": notice.message_id,
                       "effect_key": key}),
            )],
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

fn effect(
    conn: &mut Connection,
    attempt_id: &str,
    key: &str,
    from: EffectState,
    to: EffectState,
    detail: Value,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let changed = tx
        .execute(
            "UPDATE effects SET state = ?1 WHERE effect_key = ?2 AND attempt_id = ?3 AND state = ?4",
            params![to.as_str(), key, attempt_id, from.as_str()],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return Ok(false);
    }
    let (_, _, _, run_id, project_id) = task_of(&tx, attempt_id)?;
    let kind = format!("effect.{}", to.as_str().to_ascii_lowercase());
    writer::write(
        &tx,
        &Commit {
            events: vec![event(
                &project_id,
                &run_id,
                child_id(key, &kind),
                &kind,
                json!({"effect_key": key, "attempt_id": attempt_id, "from": from.as_str(), "detail": detail}),
            )],
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

/// The attempt's process started, held at its start gate: in one transaction the attempt RUNNING with the process's
/// identity, its work effect STARTED (write-ahead: before the gate opens) and its task RUNNING. The runner opens the
/// gate only after this commits, so an agent never runs unless its identity and its STARTED effect are durable
/// (ADR-019 decision 8; M2T05R-01, -52). False when the attempt is no longer STARTING with its work PREPARED.
fn running(
    conn: &mut Connection,
    attempt_id: &str,
    facts: AttemptFacts,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let (task_id, task_state, revision, run_id, project_id) = task_of(&tx, attempt_id)?;
    let key = crate::reconcile::work_key(attempt_id);
    let started = tx
        .execute(
            "UPDATE effects SET state = 'STARTED' WHERE effect_key = ?1 AND attempt_id = ?2 AND state = 'PREPARED'",
            params![key, attempt_id],
        )
        .map_err(sqlite_error)?;
    if started != 1 {
        return Ok(false);
    }
    attempts::move_attempt(
        &tx,
        &AttemptMove {
            attempt_id: attempt_id.to_owned(),
            from: AttemptState::Starting,
            to: AttemptState::Running,
            facts,
        },
        ctx,
    )?;
    let cas = if task_state == TaskState::Leased {
        vec![StateCas {
            id: task_id,
            expected_revision: revision,
            change: Change::Task(TaskState::Running),
        }]
    } else {
        Vec::new()
    };
    writer::write(
        &tx,
        &Commit {
            cas,
            events: vec![event(
                &project_id,
                &run_id,
                child_id(&key, "effect.started"),
                "effect.started",
                json!({"effect_key": key, "attempt_id": attempt_id, "from": "PREPARED", "detail": {}}),
            )],
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

fn finished(
    conn: &mut Connection,
    attempt_id: &str,
    success: bool,
    detail: &str,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let (task_id, task_state, revision, _, _) = task_of(&tx, attempt_id)?;
    let from: String = tx
        .query_row(
            "SELECT state FROM attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let from = writer::parse::<AttemptState>(&from)?;
    // A process that never started leaves its attempt STARTING: it can only fail.
    let success = success && from == AttemptState::Running;
    let (to, task_to) = if success {
        (AttemptState::Settled, TaskState::Implemented)
    } else {
        (AttemptState::Failed, TaskState::Ready)
    };
    attempts::move_attempt(
        &tx,
        &AttemptMove {
            attempt_id: attempt_id.to_owned(),
            from,
            to,
            facts: AttemptFacts {
                ended_at: Some(crate::time::now_rfc3339()),
                failure_reason: (!success).then(|| detail.to_owned()),
                ..AttemptFacts::default()
            },
        },
        ctx,
    )?;
    let cas = if task_state == TaskState::Running
        || (task_state == TaskState::Leased && task_to == TaskState::Ready)
    {
        vec![StateCas {
            id: task_id,
            expected_revision: revision,
            change: Change::Task(task_to),
        }]
    } else {
        Vec::new()
    };
    writer::write(
        &tx,
        &Commit {
            cas,
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}
