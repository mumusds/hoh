//! [`super::Store::accept_task_result`]: the Developer's TaskResult (REQ-022 b7, AC-022-04; docs/adr/ADR-018.md
//! decision 5). A COMPLETED result naming a frozen candidate of the run's current specification moves the task to
//! VERIFYING and the candidate to VERIFYING, writes `candidate_ready`, and queues one verification handoff each for
//! qa and reviewer, all in one transaction; a stale or mismatched result is one `task_result.rejected` event.

use super::decide::{child_id, load_binding};
use super::writer::{self, Change, Commit, Intent, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::protocol::{self, Candidate, RejectionReason, TaskResult, TaskState, TaskStatus};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// The roles a candidate is handed to (REQ-037: QA and Review verify the same candidate in parallel).
pub const VERIFIERS: [&str; 2] = ["qa", "reviewer"];

/// At most this many objects a handover package references (docs/adr/ADR-026.md decision 10).
pub(super) const REFERENCES: usize = 20;

/// What became of a TaskResult.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskResultOutcome {
    /// Accepted now: the `candidate_ready` event and the handoff message ids.
    Accepted {
        event_seq: i64,
        handoffs: Vec<String>,
    },
    /// The same result again: what it did the first time, nothing written now.
    Replayed { handoffs: Vec<String> },
    /// Refused: one `task_result.rejected` event, nothing else changed.
    Rejected {
        reason: RejectionReason,
        detail: String,
        event_seq: i64,
    },
}

struct Run {
    project_id: String,
    spec_id: Option<String>,
    spec_sha256: Option<String>,
    spec_revision: Option<i64>,
}

pub(super) fn accept(
    conn: &mut Connection,
    result: &TaskResult,
    ctx: &super::writer::Ctx<'_>,
) -> Result<TaskResultOutcome, PersistError> {
    if result.status != TaskStatus::Completed {
        return Err(PersistError::Unsupported(
            "a TaskResult that is not COMPLETED",
        ));
    }
    let tx = super::writer::begin(conn, ctx)?;
    let outcome = accept_in(&tx, result, false, ctx)?;
    if !matches!(outcome, TaskResultOutcome::Replayed { .. }) {
        super::writer::commit_unhooked(tx, ctx)?;
    }
    Ok(outcome)
}

/// [`accept`] inside the caller's transaction (docs/adr/ADR-028.md decision 4: a writer attempt's completion freezes,
/// accepts and settles in one commit). Writes what `accept` writes, never commits; a refusal is written too (its
/// `task_result.rejected` event), so the caller commits either way.
/// With `on_verifying`, the result's candidate may be the run's current candidate already under verification: the
/// same content frozen again, which gets a new round for this task alone (docs/adr/ADR-028.md decision 3's row).
pub(super) fn accept_in(
    tx: &Transaction<'_>,
    result: &TaskResult,
    on_verifying: bool,
    ctx: &super::writer::Ctx<'_>,
) -> Result<TaskResultOutcome, PersistError> {
    if result.status != TaskStatus::Completed {
        return Err(PersistError::Unsupported(
            "a TaskResult that is not COMPLETED",
        ));
    }
    let ready_id = child_id(&result.attempt_id, "result");
    let rejected_id = child_id(&result.attempt_id, "result-rejected");
    // The same attempt's result again: the same one replays; another one is a reuse of its key (REQ-022 b4,
    // M2T04R-52).
    let submitted = protocol::encode(result)
        .map_err(|e| PersistError::IntegrityViolation(format!("task result: {e}")))?;
    // Compared as stored text, not reparsed numbers (M2T04R-152).
    let submitted = submitted.to_string();
    let same = |payload: &Value, id: &str| -> Result<(), PersistError> {
        if payload.get("task_result_json").and_then(Value::as_str) == Some(submitted.as_str()) {
            Ok(())
        } else {
            Err(PersistError::KeyReuse(id.to_owned()))
        }
    };
    if let Some((seq, payload)) = event(tx, &ready_id)? {
        same(&payload, &ready_id)?;
        let handoffs = queued(tx, seq)?;
        return Ok(TaskResultOutcome::Replayed { handoffs });
    }
    if let Some((seq, payload)) = event(tx, &rejected_id)? {
        same(&payload, &rejected_id)?;
        let reason = serde_json::from_value(payload["reason_code"].clone())
            .map_err(|e| PersistError::IntegrityViolation(format!("{rejected_id}: {e}")))?;
        return Ok(TaskResultOutcome::Rejected {
            reason,
            detail: payload["detail"].as_str().unwrap_or_default().to_owned(),
            event_seq: seq,
        });
    }
    let run = run(tx, &result.run_id)?;
    let task: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT state, revision, role FROM tasks WHERE task_id = ?1 AND run_id = ?2",
            params![result.task_id, result.run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, revision, role)) = task else {
        return Err(PersistError::IntegrityViolation(format!(
            "task {} is not a task of run {}",
            result.task_id, result.run_id
        )));
    };
    let attempt: Option<(String, String, i64, bool)> = tx
        .query_row(
            "SELECT task_id, state, role_epoch, \
             NOT EXISTS(SELECT 1 FROM attempts b WHERE b.task_id = a.task_id AND b.rowid > a.rowid) \
             FROM attempts a WHERE attempt_id = ?1",
            params![result.attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((attempt_task, attempt_state, attempt_epoch, latest)) = attempt else {
        return Err(PersistError::IntegrityViolation(format!(
            "no attempt {}",
            result.attempt_id
        )));
    };
    if attempt_task != result.task_id {
        return Err(PersistError::IntegrityViolation(format!(
            "attempt {} is not an attempt of task {}",
            result.attempt_id, result.task_id
        )));
    }
    let binding = load_binding(tx, &result.run_id, &result.role)?;
    let candidate = match &result.candidate_id {
        Some(id) => load_candidate(tx, &result.run_id, id)?,
        None => None,
    };
    let current = super::outbox::current_candidate(tx, &result.run_id)?;
    let refusal = if role == super::wakes::COMMANDER {
        // A Commander turn answers through its outputs (docs/adr/ADR-025.md decision 8), not a TaskResult.
        Some((
            RejectionReason::IllegalTransition,
            format!(
                "task {} is a supervision task: a Commander turn reports no TaskResult",
                result.task_id
            ),
        ))
    } else if result.role != role {
        Some((
            RejectionReason::IllegalTransition,
            format!("task {} is {role}'s, not {}'s", result.task_id, result.role),
        ))
    } else if binding.as_ref().map(|b| b.role_epoch) != Some(result.role_epoch) {
        Some((
            RejectionReason::StaleRoleEpoch,
            format!(
                "role epoch {} is not the current {:?}",
                result.role_epoch,
                binding.as_ref().map(|b| b.role_epoch)
            ),
        ))
    } else if i64::try_from(result.role_epoch).ok() != Some(attempt_epoch) {
        Some((
            RejectionReason::StaleRoleEpoch,
            format!(
                "attempt {} ran at role epoch {attempt_epoch}, not {}",
                result.attempt_id, result.role_epoch
            ),
        ))
    } else if !matches!(attempt_state.as_str(), "RUNNING" | "SETTLED") || !latest {
        // A late result of an attempt that failed, was cancelled, or was followed by a newer attempt of the same task
        // (M2T04R-53). Not the lease row: that is the project's slot, which other tasks take after this one settles
        // (M2T04R-150).
        Some((
            RejectionReason::StaleLeaseEpoch,
            format!(
                "attempt {} is {attempt_state}{} and is not the task's live attempt",
                result.attempt_id,
                if latest {
                    ""
                } else {
                    ", followed by a newer attempt,"
                }
            ),
        ))
    } else if state != "RUNNING" {
        Some((
            RejectionReason::IllegalTransition,
            format!("task {} is {state}, not RUNNING", result.task_id),
        ))
    } else {
        match &candidate {
            None => Some((
                RejectionReason::CandidateMismatch,
                format!(
                    "candidate {:?} is not a frozen candidate of run {}",
                    result.candidate_id, result.run_id
                ),
            )),
            Some((c, _)) if Some(c.spec_sha256.as_str()) != run.spec_sha256.as_deref() => Some((
                RejectionReason::SpecMismatch,
                format!(
                    "candidate {} is for specification {}, not the run's",
                    c.id, c.spec_sha256
                ),
            )),
            Some((c, state)) if state != "FROZEN" && !(on_verifying && state == "VERIFYING") => {
                Some((
                    RejectionReason::CandidateMismatch,
                    format!("candidate {} is {state}, not FROZEN", c.id),
                ))
            }
            Some((c, _)) if current.as_ref() != Some(c) => Some((
                RejectionReason::CandidateMismatch,
                format!(
                    "candidate {} is not the run's current {:?}",
                    c.id,
                    current.as_ref().map(|c| &c.id)
                ),
            )),
            Some(_) => None,
        }
    };
    if let Some((reason, detail)) = refusal {
        let done = writer::write(
            tx,
            &Commit {
                events: vec![NewEvent {
                    intent: Intent::Progress,
                    project_id: run.project_id,
                    event_id: rejected_id,
                    run_id: Some(result.run_id.clone()),
                    kind: "task_result.rejected".to_owned(),
                    payload: json!({
                        "task_id": result.task_id,
                        "attempt_id": result.attempt_id,
                        "candidate_id": result.candidate_id,
                        "reason_code": reason,
                        "detail": detail,
                        "task_result_json": submitted,
                    }),
                }],
                ..Commit::default()
            },
            ctx,
        )?;
        return Ok(TaskResultOutcome::Rejected {
            reason,
            detail,
            event_seq: done.event_seqs[0],
        });
    }
    let (candidate, candidate_state) =
        candidate.ok_or_else(|| PersistError::Storage("no candidate".to_owned()))?;
    let (spec_id, spec_sha256, spec_revision) =
        match (run.spec_id, run.spec_sha256, run.spec_revision) {
            (Some(id), Some(sha), Some(rev)) => (id, sha, rev),
            _ => {
                return Err(PersistError::IntegrityViolation(format!(
                    "run {} has no specification",
                    result.run_id
                )));
            }
        };
    // The round this result opens (docs/adr/ADR-028.md decision 9): this task, and, on a newly frozen candidate, the
    // VERIFYING sources of the candidates it superseded; the same content under verification gets a round of its own.
    let frozen = candidate_state == "FROZEN";
    let mut sources = vec![super::verification::Source {
        task_id: result.task_id.clone(),
        revision: revision + 2,
    }];
    if frozen {
        sources.extend(super::verification::superseded_sources(
            tx,
            &result.run_id,
            &result.task_id,
        )?);
    }
    let round = super::verification::open_in(
        tx,
        &super::verification::Opening {
            project_id: &run.project_id,
            run_id: &result.run_id,
            spec_id: &spec_id,
            spec_sha256: &spec_sha256,
            spec_revision: u64::try_from(spec_revision)
                .map_err(|_| PersistError::IntegrityViolation("spec revision".to_owned()))?,
            candidate: &candidate,
            sources,
            attempt_id: &result.attempt_id,
            cause: &ready_id,
            outbox_event: 0,
            follows: None,
        },
    )?;
    let task = |expected: i64, to| StateCas {
        id: result.task_id.clone(),
        expected_revision: expected,
        change: Change::Task(to),
    };
    let done = writer::write(
        tx,
        &Commit {
            cas: vec![
                task(revision, TaskState::Implemented),
                task(revision + 1, TaskState::Verifying),
            ],
            events: vec![
                NewEvent {
                    intent: Intent::Progress,
                    project_id: run.project_id,
                    event_id: ready_id,
                    run_id: Some(result.run_id.clone()),
                    kind: "candidate_ready".to_owned(),
                    payload: json!({
                        "task_id": result.task_id,
                        "candidate_id": candidate.id,
                        "attempt_id": result.attempt_id,
                        "round_id": round.round_id,
                        "handoffs": round.handoffs,
                        "task_result_json": submitted,
                    }),
                },
                round.event,
            ],
            outbox: round.outbox,
        },
        ctx,
    )?;
    if frozen {
        let moved = tx
            .execute(
                "UPDATE candidates SET state = 'VERIFYING' WHERE candidate_id = ?1 AND state = 'FROZEN'",
                params![candidate.id],
            )
            .map_err(sqlite_error)?;
        if moved != 1 {
            return Err(PersistError::IntegrityViolation(format!(
                "candidate {} left FROZEN during the transaction",
                candidate.id
            )));
        }
    }
    let messages = round.handoffs;
    Ok(TaskResultOutcome::Accepted {
        event_seq: done.event_seqs[0],
        handoffs: messages,
    })
}

fn event(tx: &Transaction<'_>, event_id: &str) -> Result<Option<(i64, Value)>, PersistError> {
    let row: Option<(i64, String)> = tx
        .query_row(
            "SELECT seq, payload_json FROM events WHERE event_id = ?1",
            params![event_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    row.map(|(seq, payload)| {
        serde_json::from_str(&payload)
            .map(|p| (seq, p))
            .map_err(|e| PersistError::IntegrityViolation(format!("{event_id}: {e}")))
    })
    .transpose()
}

fn queued(tx: &Transaction<'_>, seq: i64) -> Result<Vec<String>, PersistError> {
    let mut statement = tx
        .prepare("SELECT message_id FROM outbox WHERE event_seq = ?1 ORDER BY message_id")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![seq], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

fn run(tx: &Transaction<'_>, run_id: &str) -> Result<Run, PersistError> {
    tx.query_row(
        "SELECT r.project_id, r.spec_id, s.sha256, s.revision FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id \
         WHERE r.run_id = ?1",
        params![run_id],
        |r| {
            Ok(Run {
                project_id: r.get(0)?,
                spec_id: r.get(1)?,
                spec_sha256: r.get(2)?,
                spec_revision: r.get(3)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| PersistError::RunNotFound(run_id.to_owned()))
}

/// The candidate `id` of `run_id` (its identity and state).
pub(super) fn load_candidate(
    tx: &Transaction<'_>,
    run_id: &str,
    id: &str,
) -> Result<Option<(Candidate, String)>, PersistError> {
    tx.query_row(
        "SELECT candidate_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256, state \
         FROM candidates WHERE candidate_id = ?1 AND run_id = ?2",
        params![id, run_id],
        |r| {
            Ok((
                Candidate {
                    id: r.get(0)?,
                    git_commit: r.get(1)?,
                    git_tree: r.get(2)?,
                    spec_sha256: r.get(3)?,
                    environment_sha256: r.get(4)?,
                    test_plan_sha256: r.get(5)?,
                },
                r.get(6)?,
            ))
        },
    )
    .optional()
    .map_err(sqlite_error)
}

/// The task's acceptance criteria under the specification, sorted.
/// The run's goal as its StartRun command gave it, if it gave one that is not blank.
pub(super) fn started_goal(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<String>, PersistError> {
    let started: Option<Option<String>> = conn
        .query_row(
            "SELECT json_extract(payload_json, '$.goal') FROM commands WHERE run_id = ?1 AND kind = 'StartRun' \
             ORDER BY accepted_at LIMIT 1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(started.flatten().filter(|g| !g.trim().is_empty()))
}

/// The run's goal (its StartRun command), or a fixed account of what is verified.
pub(super) fn goal(
    tx: &Transaction<'_>,
    run_id: &str,
    task_id: &str,
    candidate: &str,
) -> Result<String, PersistError> {
    Ok(started_goal(tx, run_id)?
        .unwrap_or_else(|| format!("验证任务 {task_id} 的候选 {candidate} 是否满足其验收标准")))
}

/// Run `run_id`'s result as its `run.result` carries it (docs/adr/ADR-031.md decisions 3 and 5), read in the
/// transaction that writes it, whose own seq is `seq`: its state and that state's exit code, its blocked reason when
/// BLOCKED, its specification's revision; the requirements of that specification accepted under it — the union of what
/// each ACCEPTED candidate of the run passed (`requirements_passed`, docs/adr/ADR-029.md decision 12; never the
/// per-criterion acceptance view, CR-020) — the total and the rest in ascending order; its cost (CR-009).
pub(super) fn compute(
    conn: &Connection,
    run_id: &str,
    seq: i64,
) -> Result<protocol::RunResult, PersistError> {
    use crate::lifecycle::Lifecycle as _;
    let view = view(conn, run_id, seq)?;
    let exit_code = view.exit_code.ok_or_else(|| {
        PersistError::IntegrityViolation(format!(
            "run {run_id}: {} is not a state with a result",
            view.state.as_str()
        ))
    })?;
    Ok(protocol::RunResult {
        run_id: view.run_id,
        state: view.state,
        exit_code,
        blocked_reason: view.blocked_reason,
        spec_revision: view.spec_revision,
        accepted: view.accepted,
        total: view.total,
        last_event_seq: view.last_event_seq,
        cost_usd: view.cost_usd,
        cost_status: view.cost_status,
        remaining_requirements: view.remaining_requirements,
    })
}

/// A run's result as of event `seq`, whatever its state (docs/adr/ADR-031.md decisions 3, 5 and 12): the fields of
/// its `run.result`, with `exit_code` none for a state that ends no client. [`compute`] gives a stopped run's
/// `run.result` from it; getRun shows it as the run's `projection.result`, at the watermark it reads the run at.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResultView {
    pub run_id: String,
    pub state: protocol::RunState,
    pub exit_code: Option<u8>,
    pub blocked_reason: Option<protocol::BlockedReason>,
    pub spec_revision: Option<u64>,
    pub accepted: u64,
    pub total: u64,
    pub last_event_seq: u64,
    pub cost_usd: Option<f64>,
    pub cost_status: protocol::CostStatus,
    pub remaining_requirements: Vec<String>,
}

/// [`ResultView`] of run `run_id` as of event `seq`.
pub(super) fn view(conn: &Connection, run_id: &str, seq: i64) -> Result<ResultView, PersistError> {
    let (state, blocked, spec): (String, Option<String>, Option<(String, i64)>) = conn
        .query_row(
            "SELECT r.state, r.blocked_reason, r.spec_id, s.revision FROM runs r \
             LEFT JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| {
                let spec: Option<String> = r.get(2)?;
                let revision: Option<i64> = r.get(3)?;
                Ok((r.get(0)?, r.get(1)?, spec.zip(revision)))
            },
        )
        .map_err(sqlite_error)?;
    let state = writer::parse::<protocol::RunState>(&state)?;
    let integrity = |why: String| PersistError::IntegrityViolation(format!("run {run_id}: {why}"));
    let exit_code = crate::exit::run_code(state);
    let blocked_reason = match (state, blocked) {
        (protocol::RunState::Blocked, Some(reason)) => Some(
            protocol::BlockedReason::parse(&reason)
                .ok_or_else(|| integrity(format!("unknown blocked reason {reason:?}")))?,
        ),
        (protocol::RunState::Blocked, None) => {
            return Err(integrity("BLOCKED without a blocked reason".to_owned()));
        }
        _ => None,
    };
    let (spec_revision, accepted, required) = match &spec {
        None => (None, Vec::new(), Vec::new()),
        Some((spec_id, revision)) => {
            let ids = |sql: &str, key: &str| -> Result<Vec<String>, PersistError> {
                let mut statement = conn.prepare(sql).map_err(sqlite_error)?;
                let rows = statement
                    .query_map(params![key], |r| r.get(0))
                    .map_err(sqlite_error)?;
                rows.collect::<Result<_, _>>().map_err(sqlite_error)
            };
            let required = ids(
                "SELECT requirement_id FROM requirements WHERE spec_id = ?1 ORDER BY requirement_id",
                spec_id,
            )?;
            let mut passed = std::collections::BTreeSet::new();
            for candidate in ids(
                "SELECT candidate_id FROM candidates WHERE run_id = ?1 AND state = 'ACCEPTED' \
                 ORDER BY candidate_id",
                run_id,
            )? {
                passed.extend(super::decide::requirements_passed(
                    conn, run_id, spec_id, &candidate,
                )?);
            }
            let accepted = required
                .iter()
                .filter(|r| passed.contains(*r))
                .cloned()
                .collect();
            (u64::try_from(*revision).ok(), accepted, required)
        }
    };
    let remaining: Vec<String> = required
        .iter()
        .filter(|r| !accepted.contains(*r))
        .cloned()
        .collect();
    let (cost_usd, cost_status) = super::retry::run_cost(conn, run_id)?;
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    Ok(ResultView {
        run_id: run_id.to_owned(),
        state,
        exit_code,
        blocked_reason,
        spec_revision,
        accepted: count(accepted.len()),
        total: count(required.len()),
        last_event_seq: u64::try_from(seq).map_err(|_| integrity(format!("seq {seq}")))?,
        cost_usd,
        cost_status,
        remaining_requirements: remaining,
    })
}

/// Test-only: the events of the database at `path` after seq `after` end with run `run`'s one `run.result` in `state`,
/// a valid `$defs.RunResultEvent` at its own seq, its id from the run's revision (docs/adr/ADR-031.md E1). Its
/// payload.
#[cfg(test)]
pub(super) fn assert_ends_with_result(
    path: &std::path::Path,
    after: i64,
    run: &str,
    state: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(
        "SELECT seq, project_id, event_id, run_id, kind, payload_json, created_at FROM events WHERE seq > ?1 \
         ORDER BY seq",
    )?;
    let events: Vec<Value> = statement
        .query_map([after], |r| {
            Ok(json!({
                "schema_version": "1.0", "seq": r.get::<_, i64>(0)?, "project_id": r.get::<_, String>(1)?,
                "event_id": r.get::<_, String>(2)?, "run_id": r.get::<_, Option<String>>(3)?, "attempt_id": null,
                "kind": r.get::<_, String>(4)?,
                "payload": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                "at": r.get::<_, String>(6)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    let results: Vec<&Value> = events
        .iter()
        .filter(|e| e["kind"] == "run.result")
        .collect();
    let [result] = results.as_slice() else {
        return Err(format!("not one run.result after seq {after}: {events:?}").into());
    };
    if events.last() != Some(*result) {
        return Err(format!("the run.result is not the last event: {events:?}").into());
    }
    crate::schema::protocol()
        .validate_def("RunResultEvent", result)?
        .map_err(|v| format!("{v:?}: {result}"))?;
    let revision: i64 =
        conn.query_row("SELECT revision FROM runs WHERE run_id = ?1", [run], |r| {
            r.get(0)
        })?;
    let id = crate::ids::child_id(run, &format!("result/{revision}"));
    if result["run_id"] != run
        || result["payload"]["state"] != state
        || result["payload"]["last_event_seq"] != result["seq"]
        || result["event_id"] != id.as_str()
    {
        return Err(format!("not {run}'s {state} result at its own seq, id {id}: {result}").into());
    }
    Ok(result["payload"].clone())
}

#[cfg(test)]
mod tests;
