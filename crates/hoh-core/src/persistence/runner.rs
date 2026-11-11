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
    /// What the attempt runs as (docs/adr/ADR-022.md decisions 9 to 12).
    pub assignment: super::team::Assignment,
}

/// What is waiting for the runner in a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Waiting {
    pub dispatches: Vec<DispatchNotice>,
    /// run.started notices not yet taken: (message id, run id).
    pub started: Vec<(String, String)>,
    /// The runs the runner paces and schedules (RUNNING, VERIFYING, and WAITING_EXTERNAL to be woken).
    pub runs: Vec<String>,
    /// The runs whose pause waits for its safe point (PAUSE_REQUESTED) and those being cancelled (CANCEL_REQUESTED),
    /// which the host loop moves on with or without an executor (docs/adr/ADR-021.md decision 7).
    pub pausing: Vec<String>,
    pub cancelling: Vec<String>,
    /// The runs, not ended, behind the project's current team revision: their checkpoint comes first
    /// (docs/adr/ADR-022.md decision 8).
    pub team_behind: Vec<String>,
    /// The bindings the binder looks at (decision 9).
    pub unbound: Vec<super::team::Unbound>,
    /// The project's current team: each role and the built-in role whose contract it follows (its own, a custom role's
    /// `builtin:<role>`, or none), for the capabilities it needs (decision 10).
    pub roles: Vec<(String, Option<String>)>,
    /// The agents the runner needs looked at: the project's current team's and those of the bindings in runs not ended
    /// (decision 8, third version; decision 10).
    pub agents: std::collections::BTreeSet<String>,
    /// The session.start notices waiting (docs/adr/ADR-024.md decision 4).
    pub session_starts: Vec<super::handover::SessionStart>,
}

/// One step of the runner.
#[derive(Debug, Clone)]
pub(super) enum RunnerJob {
    Waiting(String),
    /// Only the session.start notices waiting in a project (docs/adr/ADR-024.md decision 4), its runner's second look
    /// after a checkpoint or the binder (M2T17R-156).
    SessionStarts(String),
    /// A dispatch taken at this time (the runner's clock: its attempt's start, M2T19R-53).
    Received(Box<DispatchNotice>, std::time::SystemTime),
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
        /// How its processes are tracked (`attempt.tracking`, docs/adr/ADR-021.md decision 8d v5), recorded with it.
        tracking: serde_json::Value,
    },
    Finished {
        attempt_id: String,
        ending: Box<Ending>,
    },
    /// One more repair, or its refusal for a decision (ADR-020 decision 10).
    Repair(Box<super::retry::RepairRequest>),
    /// One step of a run's supervisor (docs/adr/ADR-025.md decision 2).
    Supervise {
        run_id: String,
        context: Box<crate::decision::Context>,
    },
    /// One owed wake of a project given a Commander turn, if one can be (docs/adr/ADR-025.md decision 3).
    Allocate {
        project_id: String,
        context: Box<crate::decision::Context>,
    },
    /// One look at a run as a whole (ADR-020 decisions 4, 5 and 7).
    Pace {
        run_id: String,
        context: Box<crate::decision::Context>,
    },
    Ack(String),
    /// A session.start notice settled (docs/adr/ADR-024.md decision 4): delivered to this session, or superseded.
    SessionStart(String, Option<String>),
    /// A session.start notice the executor did not take, with what it said (decision 4, M2T17R-150).
    SessionStartUndelivered(String, String),
    /// A session the binder grants (docs/adr/ADR-022.md decision 9).
    BindSession(Box<super::team::SessionGrant>),
    /// A binding the binder blocks because its agent is unavailable, and why.
    BlockBinding(Box<super::team::Unbound>, String),
    /// A run's team checkpoint (docs/adr/ADR-022.md decision 8), with the agents the registry found unavailable.
    Checkpoint(String, super::team::Availability),
    /// Renew the unexpired leases these attempts hold, at `now` (REQ-041: every 30 s, for 120 s).
    Renew(Vec<String>, std::time::SystemTime),
}

/// What a step gave.
#[derive(Debug)]
pub(super) enum RunnerDone {
    Waiting(Waiting),
    SessionStarts(Vec<super::handover::SessionStart>),
    /// Whether the step applied (a dispatch already taken or an attempt no longer PREPARED is not).
    Applied(bool),
    /// How many leases were renewed.
    Renewed(usize),
    Paced(super::retry::Paced),
    Supervised(super::supervise::Supervised),
    Allocated(Option<super::wakes::Allocated>),
    Repair(super::retry::RepairVerdict),
}

/// How an attempt ended, as the runner reports it (ADR-020 decisions 1 and 3).
#[derive(Debug, Clone, PartialEq)]
pub struct Ending {
    pub success: bool,
    pub detail: String,
    /// How it failed (unused when it succeeded).
    pub class: crate::retry::FailureClass,
    /// When it ended, by the runner's clock.
    pub at: std::time::SystemTime,
    /// A uniform 32-bit draw for the retry's jitter.
    pub draw: u32,
    /// What the agent's turn showed beyond its verdict, recorded with its end.
    pub observed: Observed,
    /// The turn ended after starting an effect whose outcome it never reported: the attempt ends UNKNOWN_EFFECT, its
    /// task untouched and nothing run again (ADR-019 decision 5, docs/adr/ADR-022.md decision 12, M2T07R-200).
    pub unknown_effect: bool,
}

/// What an agent's turn showed beyond its verdict (docs/adr/ADR-022.md decision 12): written in the commit of its end,
/// so a full writer queue cannot lose it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// The model the agent ran (REQ-018: recorded after the attempt).
    pub resolved_model: Option<String>,
    pub usage: Option<serde_json::Value>,
    /// The agent's own session id, for the session row (resumed by later turns).
    pub native_session: Option<String>,
    /// The attempt's events, in order: (key, kind, payload); each event's id is the attempt's id and its key.
    pub events: Vec<(String, String, serde_json::Value)>,
    /// The prompt it was sent (docs/adr/ADR-022.md decision 11).
    pub prompt: Option<PromptRecord>,
    /// The directory of its raw logs.
    pub raw_log_path: Option<String>,
    /// The workspace's record, taken once its processes were proven gone: its end needs it when it is an interrupted end
    /// of the writer lease's holder, and the store asks for it then (REQ-019 behaviour 6, docs/adr/ADR-024.md decision
    /// 9).
    pub workspace: Option<serde_json::Value>,
}

/// Where a turn's prompt came from and what was sent (decision 11, AC-018-05): a `prompt_revisions` row (the role
/// contract's layer, path and content sha256) and the attempt's `prompt_sha256` of the bytes sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRecord {
    /// `builtin` or `project`.
    pub layer: String,
    pub role: String,
    pub path: String,
    pub content_sha256: String,
    pub sent_sha256: String,
}

impl PromptRecord {
    /// Its `prompt_revisions` id: the contract's layer, role, path and content, so two files of the same content are
    /// two sources (M2T07R-205).
    pub fn revision_id(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.layer, self.role, self.content_sha256, self.path
        )
    }
}

pub(super) fn run(
    conn: &mut Connection,
    job: RunnerJob,
    ctx: &Ctx<'_>,
) -> Result<RunnerDone, PersistError> {
    match job {
        RunnerJob::Waiting(project_id) => waiting(conn, &project_id).map(RunnerDone::Waiting),
        RunnerJob::SessionStarts(project_id) => {
            super::handover::waiting(conn, &project_id).map(RunnerDone::SessionStarts)
        }
        RunnerJob::Received(notice, at) => {
            received(conn, &notice, at, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Effect {
            attempt_id,
            key,
            from,
            to,
            detail,
        } => effect(conn, &attempt_id, &key, from, to, detail, ctx).map(RunnerDone::Applied),
        RunnerJob::Running {
            attempt_id,
            facts,
            tracking,
        } => running(conn, &attempt_id, *facts, tracking, ctx).map(RunnerDone::Applied),
        RunnerJob::Finished { attempt_id, ending } => {
            finished(conn, &attempt_id, &ending, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Repair(request) => {
            super::retry::request_repair(conn, &request, ctx).map(RunnerDone::Repair)
        }
        RunnerJob::Allocate {
            project_id,
            context,
        } => super::wakes::allocate(conn, &project_id, &context, ctx).map(RunnerDone::Allocated),
        RunnerJob::Supervise { run_id, context } => {
            super::supervise::step(conn, &run_id, &context, ctx).map(RunnerDone::Supervised)
        }
        RunnerJob::SessionStart(message_id, delivered_to) => {
            super::handover::settle(conn, &message_id, delivered_to.as_deref(), ctx)
                .map(RunnerDone::Applied)
        }
        RunnerJob::SessionStartUndelivered(message_id, detail) => {
            super::handover::undelivered(conn, &message_id, &detail, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::BindSession(grant) => {
            super::team::bind_session(conn, &grant, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::BlockBinding(unbound, detail) => {
            super::team::block_binding(conn, &unbound, &detail, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Checkpoint(run_id, availability) => {
            super::team::checkpoint(conn, &run_id, &availability, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Pace { run_id, context } => {
            super::retry::pace(conn, &run_id, &context, ctx).map(RunnerDone::Paced)
        }
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
             AND json_extract(o.payload_json, '$.kind') IS 'dispatch' AND a.state = 'PREPARED' \
             AND (r.state IN ('CREATED', 'DISCOVERING', 'SPECIFYING', 'READY', 'RUNNING', 'VERIFYING') \
                  OR (r.state IN ('BLOCKED', 'WAITING_EXTERNAL') AND json_extract(o.payload_json, '$.origin') IS NOT NULL)) \
             ORDER BY o.event_seq",
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
                assignment: super::team::Assignment::default(),
            })
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    let dispatches = dispatches
        .into_iter()
        .map(|mut notice| {
            notice.assignment = super::team::assignment(
                conn,
                project_id,
                &notice.run_id,
                &notice.task_id,
                &notice.attempt_id,
                &notice.role,
            )?;
            Ok(notice)
        })
        .collect::<Result<Vec<_>, PersistError>>()?;
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
            "SELECT run_id FROM runs WHERE project_id = ?1 \
             AND state IN ('RUNNING', 'VERIFYING', 'WAITING_EXTERNAL', 'BLOCKED') ORDER BY created_at, run_id",
        )
        .map_err(sqlite_error)?;
    let runs = statement
        .query_map(params![project_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let in_state = |state: &str| -> Result<Vec<String>, PersistError> {
        let mut statement = conn
            .prepare("SELECT run_id FROM runs WHERE project_id = ?1 AND state = ?2 ORDER BY created_at, run_id")
            .map_err(sqlite_error)?;
        statement
            .query_map(params![project_id, state], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)
    };
    Ok(Waiting {
        session_starts: super::handover::waiting(conn, project_id)?,
        dispatches,
        started,
        runs,
        pausing: in_state("PAUSE_REQUESTED")?,
        cancelling: in_state("CANCEL_REQUESTED")?,
        team_behind: {
            let mut statement = conn
                .prepare(
                    "SELECT run_id FROM runs WHERE project_id = ?1 \
                     AND state NOT IN ('CANCELLED', 'COMPLETED', 'FAILED', 'INCOMPLETE') AND config_revision < \
                     (SELECT coalesce(max(revision), 0) FROM team_revisions WHERE project_id = ?1 AND state = 'APPLIED') \
                     ORDER BY created_at, run_id",
                )
                .map_err(sqlite_error)?;
            statement
                .query_map(params![project_id], |r| r.get(0))
                .map_err(sqlite_error)?
                .collect::<Result<_, _>>()
                .map_err(sqlite_error)?
        },
        unbound: super::team::unbound(conn, project_id)?,
        roles: super::team::roles(conn, project_id)?,
        agents: super::team::agents(conn, project_id)?,
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
    at: std::time::SystemTime,
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
    // Nothing starts for a run that does not work (paused or cancelled meanwhile, docs/adr/ADR-021.md decision 7); a
    // runtime-class dispatch (its notice carries an origin) starts on a BLOCKED or WAITING_EXTERNAL run too
    // (docs/adr/ADR-025.md decision 3, CR-003).
    let working: bool = tx
        .query_row(
            "SELECT r.state IN ('CREATED', 'DISCOVERING', 'SPECIFYING', 'READY', 'RUNNING', 'VERIFYING') \
             OR (r.state IN ('BLOCKED', 'WAITING_EXTERNAL') AND EXISTS (SELECT 1 FROM outbox o WHERE o.message_id = ?2 \
                 AND json_extract(o.payload_json, '$.origin') IS NOT NULL)) FROM runs r WHERE r.run_id = ?1",
            params![notice.run_id, notice.message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .unwrap_or(false);
    if taken != 1 || prepared.as_deref() != Some(AttemptState::Prepared.as_str()) || !working {
        return Ok(false);
    }
    let now = crate::time::rfc3339_utc(at);
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
    tracking: serde_json::Value,
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
            events: vec![
                event(
                    &project_id,
                    &run_id,
                    child_id(&key, "effect.started"),
                    "effect.started",
                    json!({"effect_key": key, "attempt_id": attempt_id, "from": "PREPARED", "detail": {}}),
                ),
                event(
                    &project_id,
                    &run_id,
                    child_id(attempt_id, "tracking"),
                    "attempt.tracking",
                    tracking,
                ),
            ],
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
    ending: &Ending,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let (task_id, task_state, revision, run_id, project_id) = task_of(&tx, attempt_id)?;
    let (from, cancelled): (String, bool) = tx
        .query_row(
            "SELECT state, cancel_requested FROM attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let from = writer::parse::<AttemptState>(&from)?;
    // A process that never started leaves its attempt STARTING: it can only fail. A cancel takes precedence over how
    // the work ended: CANCELLED, not counted, no retry; the run's cancel moves its task (ADR-021 decision 8e).
    // Its effects still STARTED: none was observed (the runner observes and commits the work it saw end first).
    let started = super::attempts::started_effects(&tx, attempt_id)?;
    // A turn that started an effect it never reported, or an effect left STARTED at all: what it did is unknown, so
    // nothing runs it again and its task stays, by the rule a reconcile applies too (ADR-019 decision 5; ADR-022
    // decision 12 v3, M2T07R-200, -451). Only a RUNNING attempt can have started one.
    let unknown = (ending.unknown_effect || !started.is_empty())
        && from == AttemptState::Running
        && !cancelled;
    let success = ending.success && from == AttemptState::Running && !cancelled && !unknown;
    let to = if cancelled {
        AttemptState::Cancelled
    } else if unknown {
        AttemptState::UnknownEffect
    } else if success {
        AttemptState::Settled
    } else {
        AttemptState::Failed
    };
    attempts::move_attempt(
        &tx,
        &AttemptMove {
            attempt_id: attempt_id.to_owned(),
            from,
            to,
            facts: AttemptFacts {
                ended_at: Some(crate::time::rfc3339_utc(ending.at)),
                failure_reason: (!success).then(|| ending.detail.clone()),
                workspace: ending.observed.workspace.clone(),
                resolved_model: ending.observed.resolved_model.clone(),
                usage: ending.observed.usage.clone(),
                raw_log_path: ending.observed.raw_log_path.clone(),
                ..AttemptFacts::default()
            },
        },
        ctx,
    )?;
    super::retry::accrue_wall(&tx, &run_id, attempt_id, ending.at, None)?;
    // The prompt's source and what was sent (decision 11).
    if let Some(prompt) = &ending.observed.prompt {
        let revision = prompt.revision_id();
        tx.execute(
            "INSERT OR IGNORE INTO prompt_revisions(revision_id, layer, role, content_sha256, path, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision,
                prompt.layer,
                prompt.role,
                prompt.content_sha256,
                prompt.path,
                crate::time::rfc3339_utc(ending.at)
            ],
        )
        .map_err(sqlite_error)?;
        tx.execute(
            "UPDATE attempts SET prompt_revision = ?2, prompt_sha256 = ?3 WHERE attempt_id = ?1",
            params![attempt_id, revision, prompt.sent_sha256],
        )
        .map_err(sqlite_error)?;
    }
    // The agent's own session id, from a turn that completed only: an id given before any input (Pi's get_state, Codex's
    // thread/start) names a session that may never have been persisted, and a failed turn must not replace a completed
    // one's (M2T17R-500). The completed turn records it as a session.native event in its own settle's transaction: that
    // record, not the session row (which older versions and direct adapters fill), is what a later turn resumes
    // (docs/adr/ADR-024.md decision 4, M2T17R-600, -601).
    let mut commit = Commit::default();
    if to == AttemptState::Settled
        && let Some(native) = &ending.observed.native_session
    {
        let session: Option<String> = tx
            .query_row(
                "SELECT session_id FROM attempts WHERE attempt_id = ?1",
                params![attempt_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if let Some(session) = session {
            tx.execute(
                "UPDATE sessions SET native_id = ?2 WHERE session_id = ?1",
                params![session, native],
            )
            .map_err(sqlite_error)?;
            writer::write_session_native(&tx, &project_id, &run_id, attempt_id, &session, native)?;
        }
    }
    for (key, kind, payload) in &ending.observed.events {
        commit.events.push(NewEvent {
            project_id: project_id.clone(),
            event_id: crate::ids::child_id(attempt_id, key),
            run_id: Some(run_id.clone()),
            kind: kind.clone(),
            payload: payload.clone(),
        });
    }
    // The effects it left STARTED are UNKNOWN, in its ending's transaction, and named for whoever settles them, as the
    // cancel's own pass and a reconcile record them (ADR-021 decision 8e; ADR-022 decision 12 v3, M2T07R-301, -451).
    if unknown || cancelled {
        super::attempts::effects_unknown(&tx, attempt_id)?;
    }
    if !started.is_empty() && (unknown || cancelled) {
        commit.events.push(NewEvent {
            project_id: project_id.clone(),
            event_id: crate::ids::child_id(attempt_id, "runner/unknown-effect"),
            run_id: Some(run_id.clone()),
            kind: "runner.diagnostic".to_owned(),
            payload: serde_json::json!({
                "code": "UNKNOWN_EFFECT", "attempt_id": attempt_id, "effect_keys": started,
                "detail": ending.detail,
            }),
        });
    }
    // An unknown effect leaves its task as it is; a cancelled attempt's task is the cancel's to move.
    if success && !unknown {
        if task_state == TaskState::Running {
            commit.cas.push(StateCas {
                id: task_id,
                expected_revision: revision,
                change: Change::Task(TaskState::Implemented),
            });
        }
    } else if !unknown && !cancelled {
        // Its task by the retry policy, in this transaction (ADR-020 decision 3).
        let failed = super::retry::fail_task(
            &tx,
            &super::retry::TaskRef {
                project_id: &project_id,
                run_id: &run_id,
                task_id: &task_id,
                state: task_state,
                revision,
            },
            attempt_id,
            &super::retry::Failure {
                class: ending.class,
                at: ending.at,
                draw: ending.draw,
            },
        )?;
        commit.cas.extend(failed.cas);
        commit.events.extend(failed.events);
    }
    writer::write(&tx, &commit, ctx)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}
