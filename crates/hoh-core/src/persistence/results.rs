//! [`super::Store::accept_task_result`]: the Developer's TaskResult (REQ-022 b7, AC-022-04; docs/adr/ADR-018.md
//! decision 5). A COMPLETED result naming a frozen candidate of the run's current specification moves the task to
//! VERIFYING and the candidate to VERIFYING, writes `candidate_ready`, and queues one verification handoff each for
//! qa and reviewer, all in one transaction; a stale or mismatched result is one `task_result.rejected` event.

use super::decide::{child_id, load_binding};
use super::writer::{self, Change, Commit, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::handoff::{self, Recipient, Verification};
use crate::protocol::{self, Candidate, RejectionReason, TaskResult, TaskState, TaskStatus};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

/// The roles a candidate is handed to (REQ-037: QA and Review verify the same candidate in parallel).
pub const VERIFIERS: [&str; 2] = ["qa", "reviewer"];

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
    after_events: &dyn Fn(),
) -> Result<TaskResultOutcome, PersistError> {
    if result.status != TaskStatus::Completed {
        return Err(PersistError::Unsupported(
            "a TaskResult that is not COMPLETED",
        ));
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
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
    if let Some((seq, payload)) = event(&tx, &ready_id)? {
        same(&payload, &ready_id)?;
        let handoffs = queued(&tx, seq)?;
        return Ok(TaskResultOutcome::Replayed { handoffs });
    }
    if let Some((seq, payload)) = event(&tx, &rejected_id)? {
        same(&payload, &rejected_id)?;
        let reason = serde_json::from_value(payload["reason_code"].clone())
            .map_err(|e| PersistError::IntegrityViolation(format!("{rejected_id}: {e}")))?;
        return Ok(TaskResultOutcome::Rejected {
            reason,
            detail: payload["detail"].as_str().unwrap_or_default().to_owned(),
            event_seq: seq,
        });
    }
    let run = run(&tx, &result.run_id)?;
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
    let binding = load_binding(&tx, &result.run_id, &result.role)?;
    let candidate = match &result.candidate_id {
        Some(id) => load_candidate(&tx, &result.run_id, id)?,
        None => None,
    };
    let current = super::outbox::current_candidate(&tx, &result.run_id)?;
    let refusal = if result.role != role {
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
            Some((c, state)) if state != "FROZEN" => Some((
                RejectionReason::CandidateMismatch,
                format!("candidate {} is {state}, not FROZEN", c.id),
            )),
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
            &tx,
            &Commit {
                events: vec![NewEvent {
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
            after_events,
        )?;
        tx.commit().map_err(sqlite_error)?;
        return Ok(TaskResultOutcome::Rejected {
            reason,
            detail,
            event_seq: done.event_seqs[0],
        });
    }
    let (candidate, _) =
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
    let acceptance_ids = acceptance(&tx, &result.task_id, &spec_id)?;
    let mut recipients = Vec::with_capacity(VERIFIERS.len());
    for role in VERIFIERS {
        let binding = load_binding(&tx, &result.run_id, role)?.ok_or_else(|| {
            PersistError::IntegrityViolation(format!("run {} has no {role} bound", result.run_id))
        })?;
        recipients.push(Recipient {
            role: role.to_owned(),
            role_epoch: binding.role_epoch,
        });
    }
    let verification = Verification {
        run_id: result.run_id.clone(),
        task_id: result.task_id.clone(),
        attempt_id: result.attempt_id.clone(),
        candidate: candidate.clone(),
        spec_sha256,
        spec_revision: u64::try_from(spec_revision)
            .map_err(|_| PersistError::IntegrityViolation("spec revision".to_owned()))?,
        acceptance_ids,
        goal: goal(&tx, &result.run_id, &result.task_id, &candidate.id)?,
        task_revision: u64::try_from(revision + 2)
            .map_err(|_| PersistError::IntegrityViolation("task revision".to_owned()))?,
        cause: ready_id.clone(),
    };
    // No acceptance criteria, nothing to verify against: refused whole, no QA attempt (REQ-022).
    let messages = handoff::verification(&verification, &recipients).ok_or_else(|| {
        PersistError::IntegrityViolation(format!(
            "task {} has no acceptance criteria under the run's specification",
            result.task_id
        ))
    })?;
    let outbox = messages
        .iter()
        .map(|m| NewOutbox::hcp(0, m))
        .collect::<Result<Vec<_>, _>>()?;
    let task = |expected: i64, to| StateCas {
        id: result.task_id.clone(),
        expected_revision: expected,
        change: Change::Task(to),
    };
    let done = writer::write(
        &tx,
        &Commit {
            cas: vec![
                task(revision, TaskState::Implemented),
                task(revision + 1, TaskState::Verifying),
            ],
            events: vec![NewEvent {
                project_id: run.project_id,
                event_id: ready_id,
                run_id: Some(result.run_id.clone()),
                kind: "candidate_ready".to_owned(),
                payload: json!({
                    "task_id": result.task_id,
                    "candidate_id": candidate.id,
                    "attempt_id": result.attempt_id,
                    "handoffs": messages.iter().map(|m| m.message_id.clone()).collect::<Vec<_>>(),
                    "task_result_json": submitted,
                }),
            }],
            outbox,
        },
        after_events,
    )?;
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
    tx.commit().map_err(sqlite_error)?;
    Ok(TaskResultOutcome::Accepted {
        event_seq: done.event_seqs[0],
        handoffs: messages.into_iter().map(|m| m.message_id).collect(),
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
fn load_candidate(
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
fn acceptance(
    tx: &Transaction<'_>,
    task_id: &str,
    spec_id: &str,
) -> Result<Vec<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT ac.ac_id FROM task_requirements tr JOIN acceptance_criteria ac \
             ON ac.requirement_id = tr.requirement_id AND ac.spec_id = tr.spec_id \
             WHERE tr.task_id = ?1 AND tr.spec_id = ?2 ORDER BY ac.ac_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![task_id, spec_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The run's goal (its StartRun command), or a fixed account of what is verified.
fn goal(
    tx: &Transaction<'_>,
    run_id: &str,
    task_id: &str,
    candidate: &str,
) -> Result<String, PersistError> {
    let started: Option<Option<String>> = tx
        .query_row(
            "SELECT json_extract(payload_json, '$.goal') FROM commands WHERE run_id = ?1 AND kind = 'StartRun' \
             ORDER BY accepted_at LIMIT 1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(started
        .flatten()
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| format!("验证任务 {task_id} 的候选 {candidate} 是否满足其验收标准")))
}
