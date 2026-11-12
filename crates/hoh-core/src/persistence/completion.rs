//! [`super::Store::complete_writer_attempt`]: a writer attempt's completion in one commit (docs/adr/ADR-028.md decisions
//! 3 and 4): the freeze's store side, the TaskResult's acceptance, a protocol correction or a decision, and the
//! attempt's settle, so a crash before it leaves nothing written and the write lease held, and the normal end and a
//! reconcile take the same path.

use super::candidates::{Freeze, Frozen};
use super::results::TaskResultOutcome;
use super::retry::{Repair, RepairRequest, RepairVerdict};
use super::runner::Ending;
use super::writer::{self, Change, Commit, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::protocol::{BlockedReason, RejectionReason, TaskResult, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;

/// What the host decided from a successful writer attempt's bridge (docs/adr/ADR-028.md decision 3).
#[derive(Debug, Clone)]
pub enum WriterOutcome {
    /// Its TaskResult is COMPLETED and the workspace was prepared for a freeze: freeze, accept, settle.
    Freeze {
        job: Box<Freeze>,
        result: Box<TaskResult>,
    },
    /// Its output must be corrected (missing, invalid, cut, a candidate id that is not the freeze's, a freeze refused,
    /// content already judged): one counted protocol correction, the task back to READY.
    Correct { reasons: Vec<String> },
    /// It needs a decision (FAILED, BLOCKED, INCOMPLETE, no AC to verify, a specification change, no verification
    /// team): the task BLOCKED for a decision.
    Decide { reason: String, detail: String },
}

/// A writer attempt's completion.
#[derive(Debug, Clone)]
pub struct WriterCompletion {
    pub attempt_id: String,
    pub ending: Ending,
    pub outcome: WriterOutcome,
}

/// What the completion did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterCompleted {
    /// Frozen and accepted: the candidate under verification and its handoff messages.
    Verifying {
        candidate_id: String,
        handoffs: Vec<String>,
    },
    /// A protocol correction: granted (the task READY) or refused (the task BLOCKED for a decision).
    Corrected { verdict: RepairVerdict },
    /// The task BLOCKED for a decision.
    Blocked { reason: String },
    /// The attempt had already ended: nothing written (a completion asked again after its commit).
    AlreadyEnded,
    /// Its run is being cancelled: nothing frozen, accepted or counted; the attempt CANCELLED, its task the cancel's to
    /// move (ADR-021 decision 8e).
    Cancelled,
}

pub(super) fn complete(
    conn: &mut Connection,
    job: &WriterCompletion,
    ctx: &super::writer::Ctx<'_>,
) -> Result<WriterCompleted, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let attempt: Option<(String, bool, String, String, String, i64)> = tx
        .query_row(
            "SELECT a.state, a.cancel_requested, t.task_id, t.run_id, r.project_id, t.revision FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id JOIN runs r ON r.run_id = t.run_id WHERE a.attempt_id = ?1",
            params![job.attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, cancelled, task_id, run_id, project_id, revision)) = attempt else {
        return Err(PersistError::IntegrityViolation(format!(
            "no attempt {}",
            job.attempt_id
        )));
    };
    // Its completion committed already (a reconcile asking again after the commit): nothing more.
    if state != "RUNNING" {
        return Ok(WriterCompleted::AlreadyEnded);
    }
    // A cancel takes precedence over how the work ended (ADR-021 decision 8e): nothing frozen, accepted or counted; the
    // settle ends the attempt CANCELLED (read again in its transaction) and leaves its task to the cancel.
    if cancelled {
        super::runner::finished_in(&tx, &job.attempt_id, &job.ending, ctx)?;
        ctx.before_commit()?;
        tx.commit().map_err(sqlite_error)?;
        return Ok(WriterCompleted::Cancelled);
    }
    let task = Task {
        project_id,
        run_id,
        task_id,
        revision,
    };
    let done = match &job.outcome {
        WriterOutcome::Freeze {
            job: freeze,
            result,
        } => freeze_and_accept(&tx, job, &task, freeze, result, ctx)?,
        WriterOutcome::Correct { reasons } => correct(&tx, job, &task, reasons, ctx)?,
        WriterOutcome::Decide { reason, detail } => decide(&tx, job, &task, reason, detail, ctx)?,
    };
    // The attempt settles in the same commit; its task has moved (VERIFYING, READY or BLOCKED), so the settle leaves it
    // alone, and the write lease goes with the attempt's end.
    super::runner::finished_in(&tx, &job.attempt_id, &job.ending, ctx)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(done)
}

struct Task {
    project_id: String,
    run_id: String,
    task_id: String,
    revision: i64,
}

/// Freeze and accept; a refused acceptance rolls the freeze back (no candidate row, nothing made STALE) and becomes a
/// correction, or a decision for a specification the run does not have.
fn freeze_and_accept(
    tx: &Transaction<'_>,
    job: &WriterCompletion,
    task: &Task,
    freeze: &Freeze,
    result: &TaskResult,
    ctx: &super::writer::Ctx<'_>,
) -> Result<WriterCompleted, PersistError> {
    let id = freeze.sealed.candidate_id.clone();
    if let Some(given) = result.candidate_id.as_ref().filter(|g| **g != id) {
        let reason = format!("the TaskResult names candidate {given}, not the frozen {id}");
        return correct(tx, job, task, &[reason], ctx);
    }
    // What would refuse the acceptance whole is decided before any freeze (M2T16D-353, -1055): no criterion to verify
    // under the run's specification, or no qa or reviewer bound to verify it.
    let criteria: i64 = tx
        .query_row(
            "SELECT count(*) FROM task_requirements tr JOIN runs r ON r.run_id = ?2 \
             JOIN acceptance_criteria ac ON ac.requirement_id = tr.requirement_id AND ac.spec_id = tr.spec_id \
             WHERE tr.task_id = ?1 AND tr.spec_id = r.spec_id",
            params![task.task_id, task.run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if criteria == 0 {
        return decide(
            tx,
            job,
            task,
            "no_acceptance_criteria",
            "the task has no acceptance criterion under the run's specification: nothing to verify (REQ-053 \
             behaviour 8)",
            ctx,
        );
    }
    for role in super::results::VERIFIERS {
        if super::decide::load_binding(tx, &task.run_id, role)?.is_none() {
            return decide(
                tx,
                job,
                task,
                "verification_team_missing",
                &format!("no {role} is bound to verify the candidate (M2-T15's staffing rules)"),
                ctx,
            );
        }
    }
    tx.execute_batch("SAVEPOINT completion_freeze")
        .map_err(sqlite_error)?;
    let rollback = |tx: &Transaction<'_>| {
        tx.execute_batch(
            "ROLLBACK TO SAVEPOINT completion_freeze; RELEASE SAVEPOINT completion_freeze",
        )
        .map_err(sqlite_error)
    };
    // The same content under verification gets a round of its own on that candidate (decision 3's row, M2T16D-101).
    let mut on_verifying = false;
    let refusal = match super::candidates::freeze_in(tx, freeze) {
        Ok(Frozen::New { .. }) => None,
        Ok(Frozen::Existing { state, .. }) if state == "FROZEN" => None,
        Ok(Frozen::Existing { state, .. }) if state == "VERIFYING" => {
            on_verifying = true;
            None
        }
        Ok(Frozen::Existing { state, .. }) => Some(format!(
            "the same content was frozen before in this run and is {state}: commit anew"
        )),
        Err(PersistError::CandidateOfAnotherRun { run_id, .. }) => Some(format!(
            "the same content was frozen in run {run_id}: commit anew"
        )),
        Err(e) => return Err(e),
    };
    if let Some(reason) = refusal {
        rollback(tx)?;
        return correct(tx, job, task, &[reason], ctx);
    }
    let mut accepted = result.clone();
    accepted.candidate_id = Some(id.clone());
    match super::results::accept_in(tx, &accepted, on_verifying, ctx)? {
        TaskResultOutcome::Accepted { handoffs, .. } | TaskResultOutcome::Replayed { handoffs } => {
            tx.execute_batch("RELEASE SAVEPOINT completion_freeze")
                .map_err(sqlite_error)?;
            Ok(WriterCompleted::Verifying {
                candidate_id: id,
                handoffs,
            })
        }
        TaskResultOutcome::Rejected { reason, detail, .. } => {
            rollback(tx)?;
            if reason == RejectionReason::SpecMismatch {
                decide(
                    tx,
                    job,
                    task,
                    "spec_mismatch",
                    &format!("{detail}: a specification change goes through M3-T02's flow"),
                    ctx,
                )
            } else {
                correct(tx, job, task, &[detail], ctx)
            }
        }
    }
}

/// One protocol correction of the task, counted by Repair::Protocol keyed by the first attempt of its chain (the
/// attempts since its last candidate_ready); granted, the task back to READY; refused, BLOCKED for a decision.
fn correct(
    tx: &Transaction<'_>,
    job: &WriterCompletion,
    task: &Task,
    reasons: &[String],
    ctx: &super::writer::Ctx<'_>,
) -> Result<WriterCompleted, PersistError> {
    let root = chain_root(tx, &task.task_id, &job.attempt_id)?;
    let limit = repair_limit(tx, &task.run_id, "protocol")?;
    let request = RepairRequest {
        run_id: task.run_id.clone(),
        task_id: task.task_id.clone(),
        repair: Repair::Protocol {
            attempt_id: root.clone(),
        },
        limit,
        detail: reasons.join("; "),
        action_id: Some(job.attempt_id.clone()),
    };
    let (verdict, mut commit) = super::retry::repair_in(tx, &request)?;
    if let RepairVerdict::Granted { .. } = verdict {
        commit.cas.push(StateCas {
            id: task.task_id.clone(),
            expected_revision: task.revision,
            change: Change::Task(TaskState::Ready),
        });
        commit.events.push(NewEvent {
            project_id: task.project_id.clone(),
            event_id: child_id(&job.attempt_id, "protocol_correction"),
            run_id: Some(task.run_id.clone()),
            kind: "task.protocol_correction".to_owned(),
            payload: json!({
                "task_id": task.task_id, "attempt_id": job.attempt_id, "root": root, "reasons": reasons,
            }),
        });
    }
    writer::write(tx, &commit, ctx)?;
    Ok(WriterCompleted::Corrected { verdict })
}

/// The task BLOCKED for a decision, with the reason.
fn decide(
    tx: &Transaction<'_>,
    job: &WriterCompletion,
    task: &Task,
    reason: &str,
    detail: &str,
    ctx: &super::writer::Ctx<'_>,
) -> Result<WriterCompleted, PersistError> {
    tx.execute(
        "UPDATE tasks SET blocked_reason = ?1, next_retry_at = NULL WHERE task_id = ?2",
        params![BlockedReason::NeedDecision.as_str(), task.task_id],
    )
    .map_err(sqlite_error)?;
    let commit = Commit {
        cas: vec![StateCas {
            id: task.task_id.clone(),
            expected_revision: task.revision,
            change: Change::Task(TaskState::Blocked),
        }],
        events: vec![NewEvent {
            project_id: task.project_id.clone(),
            event_id: child_id(&job.attempt_id, "need_decision"),
            run_id: Some(task.run_id.clone()),
            kind: "need_decision".to_owned(),
            payload: json!({
                "run_id": task.run_id, "task_id": task.task_id, "attempt_id": job.attempt_id, "reason": reason,
                "detail": detail,
            }),
        }],
        ..Commit::default()
    };
    writer::write(tx, &commit, ctx)?;
    Ok(WriterCompleted::Blocked {
        reason: reason.to_owned(),
    })
}

/// The run's repair limit `kind` (`local` or `protocol`) from its configuration snapshot; a run snapshotted before the
/// limits were has the configuration's defaults (`max_local_repairs` 3, `max_protocol_repairs` 2).
pub(super) fn repair_limit(
    tx: &Transaction<'_>,
    run_id: &str,
    kind: &str,
) -> Result<u32, PersistError> {
    let limit: Option<i64> = tx
        .query_row(
            &format!(
                "SELECT json_extract(config_snapshot_json, '$.repairs.{kind}') FROM runs WHERE run_id = ?1"
            ),
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    let budget = crate::config::Config::default().budget;
    let default = if kind == "local" {
        budget.max_local_repairs
    } else {
        budget.max_protocol_repairs
    };
    Ok(limit.and_then(|l| u32::try_from(l).ok()).unwrap_or(default))
}

/// The first attempt of the task since the attempt of its last candidate_ready (insertion order), else its first.
fn chain_root(
    tx: &Transaction<'_>,
    task_id: &str,
    attempt_id: &str,
) -> Result<String, PersistError> {
    let root: Option<String> = tx
        .query_row(
            "SELECT a.attempt_id FROM attempts a WHERE a.task_id = ?1 AND a.rowid > COALESCE((\
               SELECT b.rowid FROM attempts b WHERE b.attempt_id = (\
                 SELECT json_extract(e.payload_json, '$.attempt_id') FROM events e WHERE e.kind = 'candidate_ready' \
                 AND json_extract(e.payload_json, '$.task_id') = ?1 ORDER BY e.seq DESC LIMIT 1)), 0) \
             ORDER BY a.rowid LIMIT 1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(root.unwrap_or_else(|| attempt_id.to_owned()))
}

#[cfg(test)]
mod tests;
