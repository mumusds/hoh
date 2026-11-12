//! Sessions and attempts in the store (REQ-036, docs/adr/ADR-011.md §2). An attempt is created PREPARED and moves along
//! `lifecycle::AttemptState`; the attempts table has no revision column, so a move compares on the state it expects
//! (a different one is `StaleState`, a move outside the table `ILLEGAL_TRANSITION`). Each write is one IMMEDIATE
//! transaction; creating and moving an attempt append their event in it. A session row is metadata of a native
//! session: written again for a resumed session, never duplicated.
//!
//! The end of an attempt's turn and a cancel of it are decided under one [`CancelLatch`] (M1T03R-10): [`end`] commits
//! the terminal move (or UNKNOWN_EFFECT, which ends the turn and leaves the outcome to the reconciler) while holding
//! it, so a cancel requested before that commit makes the end CANCELLED and one requested after it is refused
//! ([`CancelReceipt::AlreadyEnded`]).

use super::decide::child_id;
use super::leases;
use super::writer::{self, Commit, Entity, NewEvent};
use super::{PersistError, sqlite_error};
use crate::lifecycle::{self, AttemptState, Lifecycle};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::watch;

/// Whether a cancel was taken (docs/13: `cancel() -> CancelReceipt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReceipt {
    /// The attempt had not ended; it ends CANCELLED.
    Accepted,
    /// The attempt had already ended; nothing changes.
    AlreadyEnded,
}

#[derive(Debug, Default)]
struct Latch {
    requested: bool,
    ended: bool,
}

/// One attempt's cancel request and its end, decided under one lock (module docs). Clones share the latch.
#[derive(Clone)]
pub struct CancelLatch {
    state: Arc<Mutex<Latch>>,
    wake: Arc<watch::Sender<bool>>,
}

impl std::fmt::Debug for CancelLatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelLatch")
            .field("state", &*self.lock())
            .finish()
    }
}

impl Default for CancelLatch {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelLatch {
    pub fn new() -> Self {
        CancelLatch {
            state: Arc::default(),
            wake: Arc::new(watch::channel(false).0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Latch> {
        // The state is two flags; one a panicking holder left behind is still meaningful.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Ask to cancel: accepted unless the attempt has already ended, and then it ends CANCELLED.
    pub fn request(&self) -> CancelReceipt {
        let mut state = self.lock();
        if state.ended {
            return CancelReceipt::AlreadyEnded;
        }
        state.requested = true;
        drop(state);
        self.wake.send_replace(true);
        CancelReceipt::Accepted
    }

    pub fn is_requested(&self) -> bool {
        self.lock().requested
    }

    /// Wait until a cancel is accepted.
    pub async fn requested(&self) {
        let mut woken = self.wake.subscribe();
        let _ = woken.wait_for(|requested| *requested).await;
    }

    /// Run `commit` holding the latch: it learns whether a cancel was accepted, and once it succeeds no cancel is.
    fn end<T, E>(&self, commit: impl FnOnce(bool) -> Result<T, E>) -> Result<T, E> {
        let mut state = self.lock();
        let ended = commit(state.requested)?;
        state.ended = true;
        Ok(ended)
    }
}

/// A native session (`sessions` row).
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub session_id: String,
    pub agent: String,
    pub role: String,
    /// The agent's own session id (e.g. Claude's `session_id` of the init frame).
    pub native_id: Option<String>,
    pub native_path: Option<String>,
    pub agent_version: String,
    /// Probed capabilities, an object of booleans.
    pub capabilities: Value,
}

/// A new attempt, created PREPARED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttempt {
    pub attempt_id: String,
    pub task_id: String,
    pub session_id: Option<String>,
    pub role_epoch: i64,
    pub host_epoch: i64,
    pub config_revision: i64,
    pub agent: Option<String>,
}

/// Facts recorded with a move; `None` keeps what the row has.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttemptFacts {
    pub session_id: Option<String>,
    pub pid: Option<i64>,
    /// The boot the process runs in and its start time (with `pid`, what names it; REQ-041 b3, ADR-019 decision 4).
    pub boot_id: Option<String>,
    pub process_start_id: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub raw_log_path: Option<String>,
    pub resolved_model: Option<String>,
    pub failure_reason: Option<String>,
    pub cancel_requested: Option<bool>,
    pub usage: Option<Value>,
    pub cost_usd: Option<f64>,
    /// The workspace's record (`host::control::workspace`), taken after the attempt's processes were proven gone: an
    /// interrupted end of the writer lease's holder needs it and writes it as `attempt.workspace` before its lease goes
    /// (REQ-019 behaviour 6, docs/adr/ADR-024.md decision 9).
    pub workspace: Option<Value>,
    /// A writer's workspace when it starts (`{head}`, the HEAD commit or null), written as `attempt.workspace_start` in
    /// its RUNNING commit: the base of its changed files (docs/adr/ADR-028.md decisions 4 and 14).
    pub workspace_start: Option<Value>,
    /// A verifier's handoff delivered as it starts: the message id and its package kept as an object (docs/adr/
    /// ADR-028.md decision 9); its RUNNING commit acknowledges the message and records a `handoffs` row DELIVERED.
    pub handoff_package: Option<Box<(String, super::runner::ArtifactRow)>>,
    /// A verifier's run directory as it starts (`{run_dir, pre_tree, candidate_id, check_id}`), written as
    /// `verification.attempt_started` in its RUNNING commit (docs/adr/ADR-028.md decision 6, M2T16D-358).
    pub verification_start: Option<Box<Value>>,
}

/// Move attempt `attempt_id` from `from` to `to`.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptMove {
    pub attempt_id: String,
    pub from: AttemptState,
    pub to: AttemptState,
    pub facts: AttemptFacts,
}

/// An `attempts` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptRow {
    pub attempt_id: String,
    pub task_id: String,
    pub session_id: Option<String>,
    pub state: AttemptState,
    pub role_epoch: i64,
    pub host_epoch: i64,
    pub config_revision: i64,
    pub pid: Option<i64>,
    pub boot_id: Option<String>,
    pub process_start_id: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub raw_log_path: Option<String>,
    pub agent: Option<String>,
    pub resolved_model: Option<String>,
    pub failure_reason: Option<String>,
    pub cancel_requested: bool,
    pub usage: Option<Value>,
    pub cost_usd: Option<f64>,
}

/// One write of this module.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum AttemptJob {
    Session(Session),
    Create(NewAttempt),
    Move(AttemptMove),
    /// An input queue row, created QUEUED (persistence/inputs.rs).
    Enqueue(super::inputs::NewInput),
    MoveInput(super::inputs::InputMove),
    /// An event of the attempt (e.g. an adapter diagnostic); `name` makes its id unique within the attempt.
    Event {
        attempt_id: String,
        name: String,
        kind: String,
        payload: Value,
    },
}

pub(super) fn apply(
    conn: &mut Connection,
    job: &AttemptJob,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    match job {
        AttemptJob::Session(session) => upsert_session(&tx, session)?,
        AttemptJob::Create(attempt) => create(&tx, attempt, ctx)?,
        AttemptJob::Move(change) => move_attempt(&tx, change, ctx)?,
        AttemptJob::Enqueue(input) => super::inputs::enqueue(&tx, input, ctx)?,
        AttemptJob::MoveInput(change) => super::inputs::move_input(&tx, change, ctx)?,
        AttemptJob::Event {
            attempt_id,
            name,
            kind,
            payload,
        } => {
            let task_id: String = tx
                .query_row(
                    "SELECT task_id FROM attempts WHERE attempt_id = ?1",
                    params![attempt_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite_error)?
                .ok_or_else(|| {
                    PersistError::IntegrityViolation(format!("no attempt {attempt_id}"))
                })?;
            event(
                &tx,
                ctx,
                &task_id,
                child_id(attempt_id, name),
                kind,
                payload.clone(),
            )?
        }
    }
    tx.commit().map_err(sqlite_error)
}

/// The terminal move `m`, or CANCELLED with its facts if a cancel was accepted first; the state written.
pub(super) fn end(
    conn: &mut Connection,
    m: &AttemptMove,
    latch: &CancelLatch,
    ctx: &super::writer::Ctx<'_>,
) -> Result<AttemptState, PersistError> {
    // UNKNOWN_EFFECT ends the turn although not the attempt: the agent is gone and what it did is for the reconciler
    // to find out (M1T04R-07).
    if !m.to.is_terminal() && m.to != AttemptState::UnknownEffect {
        return Err(PersistError::IntegrityViolation(format!(
            "attempt {}: {} is not an end",
            m.attempt_id,
            m.to.as_str()
        )));
    }
    // BEGIN IMMEDIATE waits for the database before the latch is taken, so a cancel is never held up by that wait.
    let tx = super::writer::begin(conn, ctx)?;
    latch.end(|requested| {
        let mut m = m.clone();
        if requested {
            m.to = AttemptState::Cancelled;
            m.facts.failure_reason = None;
            m.facts.cancel_requested = Some(true);
        }
        move_attempt(&tx, &m, ctx)?;
        if requested {
            // The move keeps stored facts it is not given; a cancelled end has no failure reason at all (M1T03R-16).
            tx.execute(
                "UPDATE attempts SET failure_reason = NULL WHERE attempt_id = ?1",
                params![m.attempt_id],
            )
            .map_err(sqlite_error)?;
        }
        tx.commit().map_err(sqlite_error)?;
        Ok(m.to)
    })
}

fn upsert_session(tx: &Transaction<'_>, s: &Session) -> Result<(), PersistError> {
    if !s.capabilities.is_object() {
        return Err(PersistError::IntegrityViolation(format!(
            "session {}: capabilities must be an object",
            s.session_id
        )));
    }
    tx.execute(
        "INSERT INTO sessions(session_id, agent, role, native_id, native_path, agent_version, capabilities_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(session_id) DO UPDATE SET \
         native_id = COALESCE(excluded.native_id, native_id), native_path = COALESCE(excluded.native_path, native_path), \
         agent_version = excluded.agent_version, capabilities_json = excluded.capabilities_json",
        params![
            s.session_id,
            s.agent,
            s.role,
            s.native_id,
            s.native_path,
            s.agent_version,
            s.capabilities.to_string()
        ],
    )
    .map(drop)
    .map_err(sqlite_error)
}

/// (project_id, run_id) of the task an attempt belongs to.
fn owner(tx: &Transaction<'_>, task_id: &str) -> Result<(String, String), PersistError> {
    tx.query_row(
        "SELECT r.project_id, r.run_id FROM tasks t JOIN runs r ON r.run_id = t.run_id WHERE t.task_id = ?1",
        params![task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| PersistError::IntegrityViolation(format!("no task {task_id}")))
}

fn event(
    tx: &Transaction<'_>,
    ctx: &super::writer::Ctx<'_>,
    task_id: &str,
    event_id: String,
    kind: &str,
    payload: Value,
) -> Result<(), PersistError> {
    let (project_id, run_id) = owner(tx, task_id)?;
    let commit = Commit {
        events: vec![NewEvent {
            project_id,
            event_id,
            run_id: Some(run_id),
            kind: kind.to_owned(),
            payload,
        }],
        ..Commit::default()
    };
    writer::write(tx, &commit, ctx).map(drop)
}

/// The row of a new PREPARED attempt, without an event (a dispatch decision's event records it, AC-035-02).
pub(super) fn insert_prepared(tx: &Transaction<'_>, a: &NewAttempt) -> Result<(), PersistError> {
    tx.execute(
        "INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, agent) \
         VALUES (?1, ?2, ?3, 'PREPARED', ?4, ?5, ?6, ?7)",
        params![a.attempt_id, a.task_id, a.session_id, a.role_epoch, a.host_epoch, a.config_revision, a.agent],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

pub(super) fn create(
    tx: &Transaction<'_>,
    a: &NewAttempt,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    insert_prepared(tx, a)?;
    event(
        tx,
        ctx,
        &a.task_id,
        child_id(&a.attempt_id, "PREPARED"),
        "attempt.created",
        json!({"attempt_id": a.attempt_id, "task_id": a.task_id, "state": "PREPARED"}),
    )
}

pub(super) fn move_attempt(
    tx: &Transaction<'_>,
    m: &AttemptMove,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT state, task_id FROM attempts WHERE attempt_id = ?1",
            params![m.attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let stale = |actual: Option<String>| PersistError::StaleState {
        entity: Entity::Attempt,
        id: m.attempt_id.clone(),
        expected: m.from.as_str().to_owned(),
        actual,
    };
    let Some((state, task_id)) = row else {
        return Err(stale(None));
    };
    if state != m.from.as_str() {
        return Err(stale(Some(state)));
    }
    lifecycle::check(m.from, m.to).map_err(|_| PersistError::IllegalTransition {
        entity: Entity::Attempt,
        id: m.attempt_id.clone(),
        from: m.from.as_str().to_owned(),
        to: m.to.as_str().to_owned(),
        why: "not in the transition table".to_owned(),
    })?;
    let f = &m.facts;
    if m.from == AttemptState::Prepared && m.to == AttemptState::Starting {
        let at = instant(&m.attempt_id, "started_at", f.started_at.as_deref())?;
        leases::require_writer(tx, &m.attempt_id, &task_id, &at)?;
    }
    let changed = tx
        .execute(
            "UPDATE attempts SET state = ?1, session_id = COALESCE(?2, session_id), pid = COALESCE(?3, pid), \
             started_at = COALESCE(?4, started_at), ended_at = COALESCE(?5, ended_at), \
             raw_log_path = COALESCE(?6, raw_log_path), resolved_model = COALESCE(?7, resolved_model), \
             failure_reason = COALESCE(?8, failure_reason), cancel_requested = COALESCE(?9, cancel_requested), \
             usage_json = COALESCE(?10, usage_json), cost_usd = COALESCE(?11, cost_usd), \
             boot_id = COALESCE(?14, boot_id), process_start_id = COALESCE(?15, process_start_id) \
             WHERE attempt_id = ?12 AND state = ?13",
            params![
                m.to.as_str(),
                f.session_id,
                f.pid,
                f.started_at,
                f.ended_at,
                f.raw_log_path,
                f.resolved_model,
                f.failure_reason,
                f.cancel_requested.map(i64::from),
                f.usage.as_ref().map(Value::to_string),
                f.cost_usd,
                m.attempt_id,
                m.from.as_str(),
                f.boot_id,
                f.process_start_id
            ],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return Err(stale(Some(state)));
    }
    // A settled attempt ends its task's retry: a task back to READY later (rework, a failed review) is no scheduled
    // retry; the dispatch keeps `next_retry_at` so that a withdrawn retry stays one (docs/adr/ADR-025.md decision 5).
    if m.to == AttemptState::Settled {
        tx.execute(
            "UPDATE tasks SET next_retry_at = NULL WHERE task_id = ?1",
            params![task_id],
        )
        .map_err(sqlite_error)?;
    }
    // An interrupted end of the writer lease's holder (its turn not completed: HoH's own stop ends it FAILED too, an
    // oversize frame or a deadline, M2T17FR-50) lets the lease go only with the workspace's record, written first in
    // this transaction: no other writer can be granted before the workspace it left is recorded (REQ-019 behaviour
    // 6, docs/adr/ADR-024.md decision 9). The row names it whether or not it expired: an expired lease of a holder not
    // ended still holds the slot.
    let interrupted = m.to == AttemptState::UnknownEffect
        || (m.from == AttemptState::Running && m.to.is_terminal() && m.to != AttemptState::Settled);
    if interrupted && leases::names_writer(tx, &m.attempt_id)? {
        let Some(workspace) = &f.workspace else {
            return Err(PersistError::WorkspaceRecordRequired(m.attempt_id.clone()));
        };
        event(
            tx,
            ctx,
            &task_id,
            child_id(&m.attempt_id, "workspace"),
            "attempt.workspace",
            json!({
                "attempt_id": m.attempt_id,
                "task_id": task_id,
                "from": m.from.as_str(),
                "to": m.to.as_str(),
                "workspace": workspace,
            }),
        )?;
    }
    if m.to.is_terminal() || m.to == AttemptState::UnknownEffect {
        // Its leases go with it, at its end (REQ-037; the slot is free once it is both ended and expired). UNKNOWN_EFFECT
        // is entered once its process is gone, so it ends the holding too (ADR-019 decision 5).
        let at = instant(&m.attempt_id, "ended_at", f.ended_at.as_deref())?;
        leases::release_held_by(tx, &m.attempt_id, &at)?;
    }
    event(
        tx,
        ctx,
        &task_id,
        child_id(&m.attempt_id, m.to.as_str()),
        "attempt.moved",
        json!({
            "attempt_id": m.attempt_id,
            "task_id": task_id,
            "from": m.from.as_str(),
            "to": m.to.as_str(),
            "failure_reason": f.failure_reason,
        }),
    )?;
    if m.to == AttemptState::Settled {
        // The shared rule of completion, on every path that settles an attempt (docs/adr/ADR-025.md decision 3): a
        // work task records task.implemented with its risk_reduction (intensive supervision wakes on it). Not a writer's
        // settle that sent its task back for a correction or blocked it for a decision, already moved in this
        // transaction: it implemented nothing (docs/adr/ADR-028.md decision 3).
        let (project_id, run_id, role, risk, state): (String, String, String, i64, String) = tx
            .query_row(
                "SELECT r.project_id, t.run_id, t.role, t.risk_reduction, t.state FROM tasks t \
                 JOIN runs r ON r.run_id = t.run_id WHERE t.task_id = ?1",
                params![task_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(sqlite_error)?;
        if role != super::wakes::COMMANDER && !matches!(state.as_str(), "READY" | "BLOCKED") {
            super::writer::write(
                tx,
                &super::writer::Commit {
                    events: vec![super::writer::NewEvent {
                        project_id,
                        event_id: crate::ids::child_id(&m.attempt_id, "implemented"),
                        run_id: Some(run_id),
                        kind: "task.implemented".to_owned(),
                        payload: serde_json::json!({
                            "task_id": task_id,
                            "attempt_id": m.attempt_id,
                            "risk_reduction": risk,
                        }),
                    }],
                    ..super::writer::Commit::default()
                },
                ctx,
            )?;
        }
    }
    Ok(())
}

/// A time a caller gave, as the store's UTC form (leases compare them as text, so an offset or a fraction must not
/// reach them, M2T03R-01); the current time when none is given.
fn instant(attempt_id: &str, field: &str, given: Option<&str>) -> Result<String, PersistError> {
    match given {
        None => Ok(crate::time::now_rfc3339()),
        Some(text) => crate::time::parse_rfc3339(text)
            .map(crate::time::rfc3339_utc)
            .ok_or_else(|| {
                PersistError::IntegrityViolation(format!(
                    "attempt {attempt_id}: {field} {text:?} is not an RFC 3339 date-time"
                ))
            }),
    }
}

pub(super) fn read_attempt(
    conn: &Connection,
    attempt_id: &str,
) -> Result<Option<AttemptRow>, PersistError> {
    type Raw = (
        (String, String, Option<String>, String, i64, i64, i64),
        (
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
        (
            Option<String>,
            Option<String>,
            i64,
            Option<String>,
            Option<f64>,
        ),
        (Option<String>, Option<String>),
    );
    let raw: Option<Raw> = conn
        .query_row(
            "SELECT attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, pid, started_at, \
             ended_at, raw_log_path, agent, resolved_model, failure_reason, cancel_requested, usage_json, cost_usd, \
             boot_id, process_start_id FROM attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |r| {
                Ok((
                    (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?),
                    (r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?),
                    (r.get(12)?, r.get(13)?, r.get(14)?, r.get(15)?, r.get(16)?),
                    (r.get(17)?, r.get(18)?),
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((
        (id, task, session, state, role_epoch, host_epoch, config_revision),
        (pid, started, ended, log, agent),
        (model, failure, cancel, usage, cost),
        (boot_id, process_start_id),
    )) = raw
    else {
        return Ok(None);
    };
    Ok(Some(AttemptRow {
        attempt_id: id,
        task_id: task,
        session_id: session,
        state: writer::parse::<AttemptState>(&state)?,
        role_epoch,
        host_epoch,
        config_revision,
        pid,
        boot_id,
        process_start_id,
        started_at: started,
        ended_at: ended,
        raw_log_path: log,
        agent,
        resolved_model: model,
        failure_reason: failure,
        cancel_requested: cancel != 0,
        usage: usage
            .map(|u| serde_json::from_str(&u))
            .transpose()
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?,
        cost_usd: cost,
    }))
}

/// (session_id, agent, role, native_id, native_path, agent_version, capabilities_json) of a sessions row.
type SessionRaw = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

pub(super) fn read_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<Session>, PersistError> {
    let raw: Option<SessionRaw> = conn
        .query_row(
            "SELECT session_id, agent, role, native_id, native_path, agent_version, capabilities_json FROM sessions \
             WHERE session_id = ?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    raw.map(
        |(session_id, agent, role, native_id, native_path, agent_version, caps)| {
            Ok(Session {
                session_id,
                agent,
                role,
                native_id,
                native_path,
                agent_version,
                capabilities: serde_json::from_str(&caps)
                    .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?,
            })
        },
    )
    .transpose()
}

/// (kind, payload) of the events of `run_id`, in seq order.
pub(super) fn read_events(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<(String, Value)>, PersistError> {
    let mut statement = conn
        .prepare("SELECT kind, payload_json FROM events WHERE run_id = ?1 ORDER BY seq")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sqlite_error)?;
    rows.map(|row| {
        let (kind, payload) = row.map_err(sqlite_error)?;
        Ok((
            kind,
            serde_json::from_str(&payload)
                .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?,
        ))
    })
    .collect()
}

#[cfg(test)]
mod tests;

/// The keys of `attempt_id`'s effects still STARTED (never observed), in order.
pub(super) fn started_effects(
    tx: &Connection,
    attempt_id: &str,
) -> Result<Vec<String>, PersistError> {
    let mut statement = tx
        .prepare("SELECT effect_key FROM effects WHERE attempt_id = ?1 AND state = 'STARTED' ORDER BY effect_key")
        .map_err(sqlite_error)?;
    statement
        .query_map(params![attempt_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)
}

/// `attempt_id` ends with its effects still STARTED unobserved: they are UNKNOWN, by the one rule its runner's end, its
/// cancel's pass and a reconcile apply (ADR-021 decision 8e; docs/adr/ADR-022.md decision 12 v3, M2T07R-451).
pub(super) fn effects_unknown(tx: &Connection, attempt_id: &str) -> Result<(), PersistError> {
    tx.execute(
        "UPDATE effects SET state = 'UNKNOWN' WHERE attempt_id = ?1 AND state = 'STARTED'",
        params![attempt_id],
    )
    .map(|_| ())
    .map_err(sqlite_error)
}
