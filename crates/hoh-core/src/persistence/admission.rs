//! The one dispatch admission rule (docs/adr/ADR-035.md decision 12, M3J-100): every work attempt is created only
//! after [`admit`] let its task through, in the transaction that creates it. The scheduling round, the Commander's
//! dispatch decision and a direct creation ([`super::attempts::create`]) all call it; `decide::lease_task` takes an
//! [`Admission`] only [`admit`] (or [`supervision`], for the Commander's own turn) builds, so a leased work attempt
//! cannot bypass it. Each task is judged in order: its revision (the run's current specification), who may dispatch
//! it (a review task is the round's), and its readiness under that revision (REQ-031 behaviours 3 and 5). The task's
//! state move, its lease and its role's epoch stay where they are judged today, after this, in the same transaction.

use super::{PersistError, sqlite_error};
use crate::decision::Rejection;
use crate::protocol::RejectionReason;
use crate::readiness::{self, Readiness, Reason};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use std::collections::BTreeMap;

/// Which path creates the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum By {
    /// The scheduling round (its retries, verifier and review tasks included).
    Round,
    /// A Commander's dispatch decision.
    Commander,
    /// [`super::attempts::create`]: `Store::create_attempt` and a consumer's effect.
    Direct,
}

/// That the attempt of `task_id` may be created now: built by [`admit`] for work and by [`supervision`] for the
/// Commander's own turn, and nowhere else.
#[derive(Debug)]
pub(super) struct Admission {
    task_id: String,
}

impl Admission {
    pub(super) fn task_id(&self) -> &str {
        &self.task_id
    }
}

/// Why [`admit`] refused a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The task is not of the run, or not of its current specification.
    SpecMismatch {
        task_spec: Option<String>,
        run_spec: Option<String>,
    },
    /// A review task, which only the scheduling round dispatches (after its independence pre-check).
    RuntimeOwned,
    /// A planned task whose readiness under the run's current revision does not pass.
    NotReady { revision: i64, reasons: Vec<Reason> },
}

impl Refusal {
    fn describe(&self, task_id: &str) -> String {
        match self {
            Refusal::SpecMismatch {
                task_spec,
                run_spec,
            } => format!(
                "{task_id} belongs to specification {task_spec:?} but the run's is {run_spec:?}"
            ),
            Refusal::RuntimeOwned => format!(
                "{task_id} is a specification review's task: the Runtime dispatches it in a session independent of \
                 its revision's drafters and the Commander"
            ),
            Refusal::NotReady { revision, reasons } => {
                let named: Vec<String> = reasons
                    .iter()
                    .map(|r| format!("{} {}", r.code.as_str(), r.ids.join(",")))
                    .collect();
                format!(
                    "{task_id} is not ready under revision {revision} ({})",
                    named.join("; ")
                )
            }
        }
    }

    fn details(&self) -> Option<serde_json::Value> {
        match self {
            Refusal::NotReady { reasons, .. } => Some(json!({
                "reasons": reasons.iter().map(Reason::json).collect::<Vec<_>>()
            })),
            _ => None,
        }
    }

    /// As a Commander's dispatch is REJECTED: SPEC_MISMATCH, or ILLEGAL_TRANSITION with the readiness reasons in
    /// `details` (the shape of a transition_run to READY refused, decision 4).
    pub(super) fn rejection(&self, task_id: &str) -> Rejection {
        Rejection {
            reason: match self {
                Refusal::SpecMismatch { .. } => RejectionReason::SpecMismatch,
                Refusal::RuntimeOwned | Refusal::NotReady { .. } => {
                    RejectionReason::IllegalTransition
                }
            },
            detail: self.describe(task_id),
            details: self.details(),
        }
    }

    /// As a direct creation is refused: nothing written.
    pub(super) fn error(&self, task_id: &str, state: &str) -> PersistError {
        PersistError::IllegalTransition {
            entity: super::writer::Entity::Task,
            id: task_id.to_owned(),
            from: state.to_owned(),
            to: "LEASED".to_owned(),
            why: self.describe(task_id),
            details: self.details().map(Box::new),
        }
    }
}

/// The verdict on each of `tasks` of run `run_id`, created by `by`, read in the caller's transaction `tx`.
pub(super) fn admit(
    tx: &Connection,
    run_id: &str,
    tasks: &[&str],
    by: By,
) -> Result<BTreeMap<String, Result<Admission, Refusal>>, PersistError> {
    let mut verdicts = BTreeMap::new();
    if tasks.is_empty() {
        return Ok(verdicts);
    }
    let run_spec: Option<String> = tx
        .query_row(
            "SELECT spec_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| PersistError::RunNotFound(run_id.to_owned()))?;
    let reviews = super::spec_review::review_tasks(tx, run_id)?;
    // The run's revision, plan and review, read once for every task (none without a specification).
    let judged = match &run_spec {
        Some(spec) => {
            let plan = super::readiness::plan(tx, run_id, spec)?;
            if plan.is_empty() {
                None
            } else {
                Some((
                    super::readiness::revision(tx, spec)?,
                    plan,
                    super::readiness::review(tx, spec)?,
                ))
            }
        }
        None => None,
    };
    for &task_id in tasks {
        let task: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT run_id, spec_id FROM tasks WHERE task_id = ?1",
                params![task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some((task_run, task_spec)) = task else {
            return Err(PersistError::IntegrityViolation(format!(
                "no task {task_id}"
            )));
        };
        // 1. Its revision: of this run, and of its current specification when it has adopted one.
        let verdict = if task_run != run_id || (run_spec.is_some() && task_spec != run_spec) {
            Err(Refusal::SpecMismatch {
                task_spec,
                run_spec: run_spec.clone(),
            })
        // 2. Who may dispatch it: a review task is the round's (its independence pre-check runs there).
        } else if by != By::Round && reviews.contains(task_id) {
            Err(Refusal::RuntimeOwned)
        } else {
            // 3. Its readiness under the run's current revision, when it is a planned task of that revision.
            let readiness = judged.as_ref().and_then(|(revision, plan, review)| {
                plan.iter().any(|t| t.task_id == task_id).then(|| {
                    (
                        revision.revision,
                        readiness::judge_task(revision, plan, review, task_id),
                    )
                })
            });
            match readiness {
                Some((revision, Readiness::NotReady(reasons))) => {
                    Err(Refusal::NotReady { revision, reasons })
                }
                // 4. M3-T04: the run mode's admission (PLAN/BUILD/AUTO) joins here, as one more refusal.
                _ => Ok(Admission {
                    task_id: task_id.to_owned(),
                }),
            }
        };
        verdicts.insert(task_id.to_owned(), verdict);
    }
    Ok(verdicts)
}

/// The verdict on one task `task_id` of run `run_id`.
pub(super) fn admit_one(
    tx: &Connection,
    run_id: &str,
    task_id: &str,
    by: By,
) -> Result<Result<Admission, Refusal>, PersistError> {
    admit(tx, run_id, &[task_id], by)?
        .remove(task_id)
        .ok_or_else(|| PersistError::Storage(format!("{task_id} left its admission")))
}

/// The admission of a supervision turn's attempt (the Commander's own turn, not work): its task must be the
/// Commander's, so no work task can be leased as one.
pub(super) fn supervision(tx: &Connection, task_id: &str) -> Result<Admission, PersistError> {
    let role: Option<String> = tx
        .query_row(
            "SELECT role FROM tasks WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if role.as_deref() != Some(super::handover::COMMANDER) {
        return Err(PersistError::IntegrityViolation(format!(
            "{task_id} ({role:?}) is no supervision turn: a work attempt goes through the dispatch admission"
        )));
    }
    Ok(Admission {
        task_id: task_id.to_owned(),
    })
}

#[cfg(test)]
mod tests;
