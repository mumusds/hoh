//! A task's failure, waiting and blocking, and a run's pace (REQ-042; docs/adr/ADR-020.md decisions 3-7).
//!
//! [`fail_task`] is the one place a failed attempt's task is decided, inside the transaction that ends the attempt:
//! its `attempt_count`, its `next_retry_at` or its blocked reason, its state and the events (every failure path uses
//! it, ADR-020 decision 3). [`pace`] is the per-tick look at a run as a whole: a WAITING_EXTERNAL run whose retry is
//! due works again; a working run with a cost limit it cannot enforce, a task its session cannot run, a stopped
//! budget, nothing left but waiting, or nothing able to move on, becomes BLOCKED, INCOMPLETE or WAITING_EXTERNAL.

use super::blockers;
use super::decide::{child_id, load_binding};
use super::writer::{self, Change, Commit, Ctx, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::budget::{self, Budget, Ledger};
use crate::decision::Context;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{BlockedReason, RunState, TaskState};
use crate::retry::{AfterFailure, FailureClass};
use crate::time::rfc3339_utc;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::time::SystemTime;

/// A failed attempt, as its task is decided by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub class: FailureClass,
    /// When it failed (the injected clock; the store keeps the second).
    pub at: SystemTime,
    /// A uniform 32-bit draw for the jitter.
    pub draw: u32,
}

/// The task a failure is for, as the caller read it in its transaction.
pub(super) struct TaskRef<'a> {
    pub project_id: &'a str,
    pub run_id: &'a str,
    pub task_id: &'a str,
    pub state: TaskState,
    pub revision: i64,
}

/// The task's move and events for the caller's commit (the columns are written already).
pub(super) struct Failed {
    pub cas: Option<StateCas>,
    pub events: Vec<NewEvent>,
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

fn millis(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Decide `task` after `attempt_id` failed with `failure` (ADR-020 decisions 2 and 3): one more failure in its
/// `attempt_count`, then READY at its `next_retry_at`, or BLOCKED for authentication, capability or a decision once
/// its retries are used up or a retry would be refused again (with a `need_decision` event). A task no longer LEASED or RUNNING is left alone.
pub(super) fn fail_task(
    tx: &Transaction<'_>,
    task: &TaskRef<'_>,
    attempt_id: &str,
    failure: &Failure,
) -> Result<Failed, PersistError> {
    if !matches!(task.state, TaskState::Running | TaskState::Leased) {
        return Ok(Failed {
            cas: None,
            events: Vec::new(),
        });
    }
    // A supervision turn is not retried by a task's policy: its task is CANCELLED, nothing counted, and its wake is
    // rearmed by the supervisor with the failure's class (docs/adr/ADR-025.md decision 3).
    if super::wakes::is_supervision(tx, task.task_id)? {
        return Ok(Failed {
            cas: Some(StateCas {
                id: task.task_id.to_owned(),
                expected_revision: task.revision,
                change: Change::Task(TaskState::Cancelled),
            }),
            events: vec![event(
                task.project_id,
                task.run_id,
                child_id(attempt_id, "turn_failed"),
                "supervisor.turn_failed",
                json!({"task_id": task.task_id, "attempt_id": attempt_id, "failure": failure.class.to_json()}),
            )],
        });
    }
    let count: i64 = tx
        .query_row(
            "SELECT attempt_count FROM tasks WHERE task_id = ?1",
            params![task.task_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let failures = u32::try_from(count.saturating_add(1)).unwrap_or(u32::MAX);
    let after = crate::retry::after_failure(failure.class, failures, failure.at, failure.draw);
    let mut events = Vec::new();
    let (to, next, reason) = match after {
        AfterFailure::Retry {
            next_retry_at,
            delay,
            bound,
        } => {
            let next = rfc3339_utc(next_retry_at);
            events.push(event(
                task.project_id,
                task.run_id,
                child_id(attempt_id, "retry"),
                "task.retry_scheduled",
                json!({
                    "task_id": task.task_id,
                    "attempt_id": attempt_id,
                    "attempt_count": failures,
                    "failure": failure.class.to_json(),
                    "failed_at": rfc3339_utc(failure.at),
                    "next_retry_at": next,
                    "delay_ms": millis(delay),
                    "bound_ms": bound.map(millis),
                    "draw": failure.draw,
                }),
            ));
            (TaskState::Ready, Some(next), None)
        }
        AfterFailure::Blocked(reason) => (TaskState::Blocked, None, Some(reason)),
        AfterFailure::Exhausted => {
            events.push(event(
                task.project_id,
                task.run_id,
                child_id(attempt_id, "need_decision"),
                "need_decision",
                json!({
                    "run_id": task.run_id,
                    "task_id": task.task_id,
                    "attempt_id": attempt_id,
                    "reason": "retries_exhausted",
                    "attempt_count": failures,
                    "detail": format!(
                        "{} failed {failures} times; {} retries are used up",
                        task.task_id,
                        crate::retry::MAX_RETRIES
                    ),
                }),
            ));
            (TaskState::Blocked, None, Some(BlockedReason::NeedDecision))
        }
        // Refused before anything ran, and the same input would be refused again (docs/adr/ADR-026.md decision 9):
        // a decision at once; the attempt's failure reason and its `attempt.start_refused` say why.
        AfterFailure::Decide => {
            events.push(event(
                task.project_id,
                task.run_id,
                child_id(attempt_id, "need_decision"),
                "need_decision",
                json!({
                    "run_id": task.run_id,
                    "task_id": task.task_id,
                    "attempt_id": attempt_id,
                    "reason": "refused_deterministically",
                    "attempt_count": failures,
                    "detail": format!(
                        "{} was refused before anything ran, and the same input would be refused again",
                        task.task_id
                    ),
                }),
            ));
            (TaskState::Blocked, None, Some(BlockedReason::NeedDecision))
        }
    };
    if let Some(reason) = reason {
        events.push(event(
            task.project_id,
            task.run_id,
            child_id(attempt_id, "blocked"),
            "task.blocked",
            json!({
                "task_id": task.task_id,
                "attempt_id": attempt_id,
                "reason": reason.as_str(),
                "attempt_count": failures,
                "failure": failure.class.to_json(),
            }),
        ));
    }
    tx.execute(
        "UPDATE tasks SET attempt_count = ?1, next_retry_at = ?2, blocked_reason = COALESCE(?3, blocked_reason) \
         WHERE task_id = ?4",
        params![
            i64::from(failures),
            next,
            reason.map(|r| r.as_str()),
            task.task_id
        ],
    )
    .map_err(sqlite_error)?;
    Ok(Failed {
        cas: Some(StateCas {
            id: task.task_id.to_owned(),
            expected_revision: task.revision,
            change: Change::Task(to),
        }),
        events,
    })
}

/// How long an attempt that started at `started` ran by `end`, in whole seconds: never less than its own host life
/// recorded it ran, to `recorded_end` (its work's OBSERVED end, by the same life's clock as its start), whatever a
/// later life's `end` says (ADR-020 decision 4, M2T19R-200, -450). A lease time is no evidence of running.
fn ran(started: SystemTime, end: SystemTime, recorded_end: Option<SystemTime>) -> u64 {
    let to = |end: SystemTime| end.duration_since(started).map_or(0, |d| d.as_secs());
    to(end).max(recorded_end.map_or(0, to))
}

/// The end its work's OBSERVED record holds for `attempt_id`, if any.
fn observed_end(conn: &Connection, attempt_id: &str) -> Result<Option<SystemTime>, PersistError> {
    let observed = child_id(&crate::reconcile::work_key(attempt_id), "effect.observed");
    let ended: Option<String> = conn
        .query_row(
            "SELECT json_extract(payload_json, '$.detail.ended_at') FROM events WHERE event_id = ?1",
            params![observed],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    Ok(ended.as_deref().and_then(crate::time::parse_rfc3339))
}

/// How far `at` is behind the start of `attempt_id` (a wall clock set back across a restart), if it is: the
/// reconcile's diagnostic (ADR-020 decision 4).
pub(super) fn behind_start(
    conn: &Connection,
    attempt_id: &str,
    at: SystemTime,
) -> Result<Option<std::time::Duration>, PersistError> {
    let started: Option<String> = conn
        .query_row(
            "SELECT started_at FROM attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    Ok(started
        .as_deref()
        .and_then(crate::time::parse_rfc3339)
        .and_then(|started| started.duration_since(at).ok())
        .filter(|d| !d.is_zero()))
}

/// Add the attempt's running time to its run's `wall_seconds` budget, if it has one (ADR-020 decision 7): the used
/// duration changes only when an attempt ends, so a waiting run's is the same across a crash and a restart. The time
/// is [`ran`], with `recorded_end` its work's OBSERVED end if the caller has one.
pub(super) fn accrue_wall(
    tx: &Transaction<'_>,
    run_id: &str,
    attempt_id: &str,
    ended_at: SystemTime,
    recorded_end: Option<SystemTime>,
) -> Result<(), PersistError> {
    let started: Option<String> = tx
        .query_row(
            "SELECT started_at FROM attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    let Some(started) = started.as_deref().and_then(crate::time::parse_rfc3339) else {
        return Ok(());
    };
    let ended = crate::time::parse_rfc3339(&rfc3339_utc(ended_at)).unwrap_or(ended_at);
    let seconds = ran(started, ended, recorded_end);
    tx.execute(
        "UPDATE budgets SET used_value = used_value + ?1 WHERE scope_kind = 'run' AND scope_id = ?2 \
         AND kind = 'wall_seconds'",
        params![seconds as f64, run_id],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// The run's budget at `now`: its run-scope rows and its deadline, the duration its unfinished attempts have run so
/// far counted as reserved (M2T19R-02: a running attempt's time counts before it ends; settled into `used` when it
/// does, so nothing is counted twice).
pub(super) fn load_budget(
    tx: &Transaction<'_>,
    run_id: &str,
    now: SystemTime,
) -> Result<Budget, PersistError> {
    let mut budget = Budget::default();
    let deadline: Option<String> = tx
        .query_row(
            "SELECT deadline_at FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    if let Some(text) = deadline {
        budget.deadline_at = Some(crate::time::parse_rfc3339(&text).ok_or_else(|| {
            PersistError::IntegrityViolation(format!("runs.deadline_at of {run_id}: {text}"))
        })?);
    }
    let mut statement = tx
        .prepare(
            "SELECT kind, limit_value, used_value, reserved_value, price_known FROM budgets \
             WHERE scope_kind = 'run' AND scope_id = ?1",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })
        .map_err(sqlite_error)?;
    for row in rows {
        let (kind, limit, used, reserved, price_known) = row.map_err(sqlite_error)?;
        let ledger = Ledger {
            limit,
            used,
            reserved,
        };
        match kind.as_str() {
            "iterations" => budget.iterations = Some(ledger),
            "wall_seconds" => budget.wall_seconds = Some(ledger),
            "usd" => budget.usd = Some((ledger, price_known)),
            _ => {}
        }
    }
    drop(statement);
    if let Some(wall) = &mut budget.wall_seconds {
        let mut statement = tx
            .prepare(
                "SELECT a.attempt_id, a.started_at FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE t.run_id = ?1 AND a.state IN ('STARTING', 'RUNNING') AND a.started_at IS NOT NULL",
            )
            .map_err(sqlite_error)?;
        let started = statement
            .query_map(params![run_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        for (attempt_id, at) in started {
            if let Some(since) = crate::time::parse_rfc3339(&at) {
                // As its settlement will count it (M2T19R-450).
                wall.reserved += ran(since, now, observed_end(tx, &attempt_id)?) as f64;
            }
        }
    }
    Ok(budget)
}

/// Why run `run_id`'s budget lets nothing more be dispatched at `context`'s time, if it does: a stop (the deadline by
/// the wall clock) or a cost limit it cannot enforce; one rule for the rounds, the pace and the decide path
/// (M2T19R-01).
pub(super) fn budget_stop(
    tx: &Transaction<'_>,
    run_id: &str,
    context: &Context,
) -> Result<Option<&'static str>, PersistError> {
    let budget = load_budget(tx, run_id, context.now())?;
    Ok(if budget::unenforceable(&budget) {
        Some("max_usd with an unknown price")
    } else {
        budget::stop(&budget, context.wall()).map(budget::Stop::as_str)
    })
}

/// What a pace did to a run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Paced {
    /// A WAITING_EXTERNAL run whose retry came due, back in its state.
    pub woke: bool,
    /// The run now waits for this retry (WAITING_EXTERNAL).
    pub waiting_until: Option<String>,
    /// The run is now BLOCKED, for these reasons.
    pub blocked: Vec<BlockedReason>,
    /// A BLOCKED run whose claims were all released, back in its state.
    pub unblocked: bool,
    /// The run is now INCOMPLETE, for this stop.
    pub incomplete: Option<&'static str>,
    /// Tasks moved to BLOCKED because their session lacks a required capability.
    pub capability_blocked: Vec<String>,
}

struct TaskRow {
    task_id: String,
    state: TaskState,
    revision: i64,
    role: String,
    next_retry_at: Option<String>,
    blocked_reason: Option<String>,
    open: i64,
}

/// What blocks the run's BLOCKED tasks, by precedence, each once.
fn blocked_reasons(tasks: &[TaskRow]) -> Vec<BlockedReason> {
    blockers::ordered(
        tasks
            .iter()
            .filter(|t| t.state == TaskState::Blocked)
            .filter_map(|t| t.blocked_reason.as_deref())
            .filter_map(BlockedReason::parse),
    )
}

/// The run's tasks not yet ACCEPTED or CANCELLED, as `run.incomplete` lists them.
fn remaining(tasks: &[TaskRow]) -> Vec<&str> {
    tasks
        .iter()
        .filter(|t| !matches!(t.state, TaskState::Accepted | TaskState::Cancelled))
        .map(|t| t.task_id.as_str())
        .collect()
}

/// One look at run `run_id` at `context.now()` (ADR-020 decisions 4, 5 and 7; docs/adr/ADR-025.md decision 5): a
/// working, WAITING_EXTERNAL or BLOCKED run. A budget that stops it ends it INCOMPLETE once nothing of it runs.
/// Otherwise the budget's claim (a cost limit it cannot enforce) and the pace's (what blocks the tasks of a run none of
/// whose tasks can move on) are recomputed as a whole; a run with claims open is BLOCKED (its decision-class
/// dispatches withdrawn), a BLOCKED one whose claims are all released goes back to its resume state; a working run
/// with nothing to do but a retry waits for it (WAITING_EXTERNAL), a waiting one is woken when one is due or nothing
/// waits any more. A BLOCKED run never parks. A pace that changes nothing writes nothing.
pub(super) fn pace(
    conn: &mut Connection,
    run_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Paced, PersistError> {
    let now = context.now();
    let at = rfc3339_utc(now);
    let tx = writer::begin(conn, ctx)?;
    let run: Option<(String, String, i64, Option<String>)> = tx
        .query_row(
            "SELECT project_id, state, revision, resume_state FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, state, revision, resume)) = run else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    let state = writer::parse::<RunState>(&state)?;
    let working = matches!(state, RunState::Running | RunState::Verifying);
    let blocked = state == RunState::Blocked;
    if !working && !blocked && state != RunState::WaitingExternal {
        return Ok(Paced::default());
    }
    let mut statement = tx
        .prepare(
            "SELECT t.task_id, t.state, t.revision, t.role, t.next_retry_at, t.blocked_reason, \
             (SELECT count(*) FROM task_dependencies d JOIN tasks u ON u.task_id = d.depends_on \
              WHERE d.task_id = t.task_id AND u.state <> 'ACCEPTED') \
             FROM tasks t WHERE t.run_id = ?1 AND t.role <> 'commander' ORDER BY t.created_seq, t.task_id",
        )
        .map_err(sqlite_error)?;
    let mut tasks = Vec::new();
    for row in statement
        .query_map(params![run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .map_err(sqlite_error)?
    {
        let (task_id, state, revision, role, next_retry_at, blocked_reason, open) =
            row.map_err(sqlite_error)?;
        tasks.push(TaskRow {
            task_id,
            state: writer::parse::<TaskState>(&state)?,
            revision,
            role,
            next_retry_at,
            blocked_reason,
            open,
        });
    }
    drop(statement);
    // Which tasks are due (READY, or PENDING with every dependency ACCEPTED, and no retry in the future) and which
    // wait for a retry.
    let due = |t: &TaskRow| {
        (t.state == TaskState::Ready || (t.state == TaskState::Pending && t.open == 0))
            && t.next_retry_at
                .as_deref()
                .is_none_or(|next| next <= at.as_str())
    };
    let waiting = |t: &TaskRow| {
        t.state == TaskState::Ready
            && t.next_retry_at
                .as_deref()
                .is_some_and(|next| next > at.as_str())
    };
    let budget = load_budget(&tx, run_id, now)?;
    let mut paced = Paced::default();
    let mut commit = Commit::default();
    let run_cas = |to: RunState| StateCas {
        id: run_id.to_owned(),
        expected_revision: revision,
        change: Change::Run(to),
    };
    let stop = budget::stop(&budget, context.wall());
    // Two counts of the attempts not ended (docs/adr/ADR-025.md decision 3): every one, supervision turns too, for a
    // budget stop, which waits for them; the work attempts only for whether the run can move on or only waits.
    let (unfinished, unfinished_work): (i64, i64) = tx
        .query_row(
            "SELECT count(*), count(*) FILTER (WHERE t.role <> 'commander') FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1 AND a.state IN ('PREPARED', 'STARTING', 'RUNNING')",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let back = |resume: &Option<String>| -> Result<RunState, PersistError> {
        Ok(resume
            .as_deref()
            .map(writer::parse::<RunState>)
            .transpose()?
            .unwrap_or(RunState::Running))
    };
    if let Some(stop) = stop {
        // A budget that stops the run: INCOMPLETE once nothing of it runs, whatever blocks it (REQ-042 behaviour 6).
        if unfinished == 0 {
            commit.cas.push(run_cas(RunState::Incomplete));
            commit.events.push(event(
                &project_id,
                run_id,
                child_id(run_id, &format!("incomplete/{revision}")),
                "run.incomplete",
                json!({"stop": stop.as_str(), "from": state.as_str(), "remaining": remaining(&tasks)}),
            ));
            paced.incomplete = Some(stop.as_str());
        }
    } else {
        if state != RunState::WaitingExternal {
            // Tasks their session cannot run are blocked, not left out silently (AC-042-03 R2; ADR-020 decision 5).
            for task in tasks.iter_mut().filter(|t| due(t)) {
                let (Some(binding), Some(required)) = (
                    load_binding(&tx, run_id, &task.role)?,
                    context.required_capabilities.get(&task.role),
                ) else {
                    continue;
                };
                if !binding.active || required.is_subset(&binding.capabilities) {
                    continue;
                }
                let missing: Vec<&String> = required.difference(&binding.capabilities).collect();
                tx.execute(
                    "UPDATE tasks SET blocked_reason = ?1, next_retry_at = NULL WHERE task_id = ?2",
                    params![BlockedReason::BlockedCapability.as_str(), task.task_id],
                )
                .map_err(sqlite_error)?;
                commit.cas.push(StateCas {
                    id: task.task_id.clone(),
                    expected_revision: task.revision,
                    change: Change::Task(TaskState::Blocked),
                });
                commit.events.push(event(
                    &project_id,
                    run_id,
                    child_id(&task.task_id, &format!("blocked/{}", task.revision)),
                    "task.blocked",
                    json!({"task_id": task.task_id, "reason": BlockedReason::BlockedCapability.as_str(), "missing": missing}),
                ));
                paced.capability_blocked.push(task.task_id.clone());
                task.state = TaskState::Blocked;
                task.blocked_reason = Some(BlockedReason::BlockedCapability.as_str().to_owned());
            }
        }
        // In flight: only what can still move on (docs/adr/ADR-028.md decision 10, M2T16D-202): a verification round's
        // source waiting for its round, and its verifier task once done, are not; a source waiting at a passable
        // candidate's barrier for the join is (M2-T16's transition until M2-T18's join).
        let verifiers = super::verification::verifier_tasks(&tx, run_id)?;
        let (sources, joining) = super::verification::sources_and_joining(&tx, run_id)?;
        let in_flight = tasks.iter().any(|t| match t.state {
            TaskState::Leased | TaskState::Running => true,
            TaskState::Implemented => !verifiers.contains(&t.task_id),
            TaskState::Verifying => !sources.contains(&t.task_id) || joining.contains(&t.task_id),
            _ => false,
        });
        let any_due = tasks.iter().any(due);
        let any_waiting = tasks.iter().any(waiting);
        // Stalled: nothing runs, nothing is in flight, nothing is due and nothing waits for a retry.
        let stalled = unfinished_work == 0 && !in_flight && !any_due && !any_waiting;
        let mut claims = blockers::Claims::load(&tx, &project_id, run_id)?;
        let unenforceable = budget::unenforceable(&budget);
        claims.set(
            blockers::BUDGET,
            if unenforceable {
                &[BlockedReason::BudgetUnenforceable]
            } else {
                &[]
            },
        );
        claims.set(
            blockers::PACE,
            &if stalled {
                blocked_reasons(&tasks)
            } else {
                Vec::new()
            },
        );
        let reasons = claims.reasons();
        let changed = claims.changed();
        commit.events.extend(claims.store(&tx)?);
        if !reasons.is_empty() {
            if blocked {
                if changed {
                    commit.cas.push(StateCas {
                        id: run_id.to_owned(),
                        expected_revision: revision,
                        change: Change::Touch(writer::Entity::Run),
                    });
                }
            } else {
                // Blocked in this transaction: its decision-class dispatches not yet taken are withdrawn
                // (docs/adr/ADR-025.md decision 3).
                let (moves, withdrawn) = super::control::withdraw(
                    &tx,
                    &project_id,
                    run_id,
                    &at,
                    super::control::Withdraw::Blocked,
                    ctx,
                )?;
                commit.cas.extend(moves);
                commit.events.extend(withdrawn);
                commit.cas.push(run_cas(RunState::Blocked));
                let detail = if unenforceable {
                    "a cost limit is set while the price is unknown"
                } else if state == RunState::WaitingExternal {
                    "nothing of the run waits any more and no task can move on"
                } else {
                    "no task of the run can move on"
                };
                commit.events.push(event(
                    &project_id,
                    run_id,
                    child_id(run_id, &format!("blocked/{revision}")),
                    "run.blocked",
                    json!({"reasons": reasons.iter().map(|r| r.as_str()).collect::<Vec<_>>(), "detail": detail}),
                ));
                paced.blocked = reasons;
            }
        } else if blocked || (state == RunState::WaitingExternal && (any_due || !any_waiting)) {
            // Back to work: its claims all released, its retry due, or nothing of it waits any more (M2T19R-05).
            let to = back(&resume)?;
            commit.cas.push(if blocked {
                StateCas {
                    id: run_id.to_owned(),
                    expected_revision: revision,
                    change: Change::Unblock(to),
                }
            } else {
                run_cas(to)
            });
            commit.events.push(event(
                &project_id,
                run_id,
                child_id(run_id, &format!("resumed/{revision}")),
                "run.resumed",
                json!({"from": state.as_str(), "to": to.as_str(), "due": tasks.iter().filter(|t| due(t)).map(|t| t.task_id.as_str()).collect::<Vec<_>>()}),
            ));
            if blocked {
                paced.unblocked = true;
            } else {
                paced.woke = true;
            }
        } else if working
            && unfinished_work == 0
            && !in_flight
            && !any_due
            && let Some(next) = tasks
                .iter()
                .filter(|t| waiting(t))
                .filter_map(|t| t.next_retry_at.clone())
                .min()
        {
            commit.cas.push(run_cas(RunState::WaitingExternal));
            commit.events.push(event(
                &project_id,
                run_id,
                child_id(run_id, &format!("waiting/{revision}")),
                "run.waiting",
                json!({"next_retry_at": next, "waiting": tasks.iter().filter(|t| waiting(t)).map(|t| t.task_id.as_str()).collect::<Vec<_>>()}),
            ));
            paced.waiting_until = Some(next);
        }
    }
    if commit.cas.is_empty() && commit.events.is_empty() {
        return Ok(paced);
    }
    writer::write(&tx, &commit, ctx)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(paced)
}

/// A repair a task asks for (REQ-042 behaviour 5, ADR-020 decision 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repair {
    /// A local repair of the issue with this fingerprint.
    Local { fingerprint: String },
    /// A protocol-format correction of this attempt.
    Protocol { attempt_id: String },
    /// One more verification of the same sources on the same candidate, keyed by the first round of its chain
    /// (docs/adr/ADR-028.md decision 12; limit [`REVERIFY_LIMIT`]).
    Reverify { root: String },
}

/// How many times a round's verification is done again on the same candidate (docs/adr/ADR-028.md decision 12, M2T16D-260):
/// a constant, changed only by a CR (docs/22).
pub const REVERIFY_LIMIT: u32 = 2;

impl Repair {
    /// The kind of the events that count it.
    fn kind(&self) -> &'static str {
        match self {
            Repair::Local { .. } => "repair.local",
            Repair::Protocol { .. } => "repair.protocol",
            Repair::Reverify { .. } => "repair.reverify",
        }
    }

    /// The payload field naming what it repairs, and its value.
    fn key(&self) -> (&'static str, &str) {
        match self {
            Repair::Local { fingerprint } => ("fingerprint", fingerprint),
            Repair::Protocol { attempt_id } => ("attempt_id", attempt_id),
            Repair::Reverify { root } => ("round_root", root),
        }
    }

    /// The `need_decision` reason once its limit is reached.
    fn exhausted(&self) -> &'static str {
        match self {
            Repair::Local { .. } => "local_repairs_exhausted",
            Repair::Protocol { .. } => "protocol_repairs_exhausted",
            Repair::Reverify { .. } => "reverify_exhausted",
        }
    }
}

/// A request for one more repair of `task_id`, `limit` being `max_local_repairs` or `max_protocol_repairs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairRequest {
    pub run_id: String,
    pub task_id: String,
    pub repair: Repair,
    pub limit: u32,
    pub detail: String,
    /// The action asking for it, when the caller may ask again for the same action (a barrier's close, a correction
    /// replayed after a crash): asked again, it is answered as the first time and counted once (docs/adr/ADR-028.md
    /// decision 12, M2T16D-09).
    pub action_id: Option<String>,
}

/// The answer to a [`RepairRequest`]: granted as the `count`-th, or refused with `count` already done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairVerdict {
    Granted { count: u32 },
    Refused { count: u32 },
}

/// How many repairs of `repair` run `run_id` was granted: its counting events (append-only, so a restart keeps them).
pub(super) fn repair_count(
    conn: &Connection,
    run_id: &str,
    repair: &Repair,
) -> Result<u32, PersistError> {
    let (field, key) = repair.key();
    let n: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM events WHERE run_id = ?1 AND kind = ?2 \
                 AND json_extract(payload_json, '$.{field}') = ?3"
            ),
            params![run_id, repair.kind(), key],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Grant `request` as one more counted repair, or, once its limit is reached, dispatch nothing: one `need_decision`
/// event and the task BLOCKED for a decision (REQ-042 behaviour 5; ADR-020 decision 10).
pub(super) fn request_repair(
    conn: &mut Connection,
    request: &RepairRequest,
    ctx: &Ctx<'_>,
) -> Result<RepairVerdict, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let (verdict, commit) = repair_in(&tx, request)?;
    writer::write(&tx, &commit, ctx)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(verdict)
}

/// [`request_repair`] inside the caller's transaction: the verdict and what to commit with the caller's own writes
/// (its counting or `need_decision` event and, when refused, the task's move to BLOCKED). A caller that also moves the
/// task asks first and moves it only when granted. With an `action_id` the same action asked again is answered as the
/// first time and adds nothing (docs/adr/ADR-028.md decision 12, M2T16D-09).
pub(super) fn repair_in(
    tx: &Connection,
    request: &RepairRequest,
) -> Result<(RepairVerdict, Commit), PersistError> {
    let task: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT r.project_id, t.state, t.revision FROM tasks t JOIN runs r ON r.run_id = t.run_id \
             WHERE t.task_id = ?1 AND t.run_id = ?2",
            params![request.task_id, request.run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, state, revision)) = task else {
        return Err(PersistError::IntegrityViolation(format!(
            "no task {} in run {}",
            request.task_id, request.run_id
        )));
    };
    if super::wakes::is_supervision(tx, &request.task_id)? {
        return Err(PersistError::IntegrityViolation(format!(
            "{} is a supervision task: a Commander turn takes no repair (docs/adr/ADR-025.md decision 3)",
            request.task_id
        )));
    }
    if let Some(action) = &request.action_id {
        let earlier: Option<(String, i64)> = tx
            .query_row(
                "SELECT kind, json_extract(payload_json, '$.count') FROM events WHERE run_id = ?1 \
                 AND kind IN (?2, 'need_decision') AND json_extract(payload_json, '$.action_id') = ?3 \
                 ORDER BY seq LIMIT 1",
                params![request.run_id, request.repair.kind(), action],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some((kind, count)) = earlier {
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            let verdict = if kind == "need_decision" {
                RepairVerdict::Refused { count }
            } else {
                RepairVerdict::Granted { count }
            };
            return Ok((verdict, Commit::default()));
        }
    }
    let state = writer::parse::<TaskState>(&state)?;
    let count = repair_count(tx, &request.run_id, &request.repair)?;
    let (field, key) = request.repair.key();
    let action = request.action_id.as_deref();
    let mut commit = Commit::default();
    let verdict = if count < request.limit {
        let n = count + 1;
        commit.events.push(event(
            &project_id,
            &request.run_id,
            child_id(&request.run_id, &format!("{}/{key}/{n}", request.repair.kind())),
            request.repair.kind(),
            json!({
                "task_id": request.task_id, field: key, "count": n, "limit": request.limit, "detail": request.detail,
                "action_id": action,
            }),
        ));
        RepairVerdict::Granted { count: n }
    } else {
        let refused: i64 = tx
            .query_row(
                &format!(
                    "SELECT count(*) FROM events WHERE run_id = ?1 AND kind = 'need_decision' \
                     AND json_extract(payload_json, '$.reason') = ?2 AND json_extract(payload_json, '$.{field}') = ?3"
                ),
                params![request.run_id, request.repair.exhausted(), key],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        commit.events.push(event(
            &project_id,
            &request.run_id,
            child_id(
                &request.run_id,
                &format!("need_decision/{}/{key}/{}", request.repair.kind(), refused + 1),
            ),
            "need_decision",
            json!({
                "run_id": request.run_id, "task_id": request.task_id, "reason": request.repair.exhausted(), field: key,
                "count": count, "limit": request.limit, "detail": request.detail, "action_id": action,
            }),
        ));
        if !matches!(
            state,
            TaskState::Blocked | TaskState::Accepted | TaskState::Cancelled
        ) {
            tx.execute(
                "UPDATE tasks SET blocked_reason = ?1, next_retry_at = NULL WHERE task_id = ?2",
                params![BlockedReason::NeedDecision.as_str(), request.task_id],
            )
            .map_err(sqlite_error)?;
            commit.cas.push(StateCas {
                id: request.task_id.clone(),
                expected_revision: revision,
                change: Change::Task(TaskState::Blocked),
            });
        }
        RepairVerdict::Refused { count }
    };
    Ok((verdict, commit))
}

/// The cost of run `run_id` (CR-009, ADR-020 decision 9): known only when its price is known (its run-scope usd row
/// says so) and every attempt has a billed cost, then their sum (0 with no attempt); otherwise unknown and null,
/// never an estimate.
pub(super) fn run_cost(
    conn: &Connection,
    run_id: &str,
) -> Result<(Option<f64>, crate::protocol::CostStatus), PersistError> {
    use crate::protocol::CostStatus;
    let price_known: bool = conn
        .query_row(
            "SELECT price_known FROM budgets WHERE scope_kind = 'run' AND scope_id = ?1 AND kind = 'usd'",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .unwrap_or(false);
    let (unbilled, total): (i64, f64) = conn
        .query_row(
            "SELECT count(*) FILTER (WHERE a.cost_usd IS NULL), COALESCE(sum(a.cost_usd), 0) FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    Ok(if price_known && unbilled == 0 {
        (Some(total), CostStatus::Known)
    } else {
        (None, CostStatus::Unknown)
    })
}

#[cfg(test)]
mod tests;
