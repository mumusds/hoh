//! The Readiness Gate's reads and its step (REQ-031 behaviour 5 to 7; docs/adr/ADR-035.md decisions 1, 4 and 8): the
//! run's approved revision, plan and specification review read for `crate::readiness`, and the tick's step that marks
//! the plan's tasks that are not ready BLOCKED(READINESS_NOT_MET), releases them once ready, and moves a SPECIFYING run
//! whose readiness passes to READY with `readiness.passed`. A step that changes nothing writes nothing.

use super::writer::{self, Change, Commit, Ctx, Intent, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{BlockedReason, RunState, TaskState};
use crate::readiness::{
    self, Criterion, Finding, OpenQuestion, PlanTask, Readiness, Requirement, Review, ReviewState,
    ReviewStatus, ReviewView, Revision, Severity,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The project event an approval writes for its new revision's open questions (docs/adr/ADR-035.md decision 1).
pub(super) const OPEN_QUESTIONS: &str = "spec.open_questions";
/// The event of a run moved from SPECIFYING to READY by its readiness (REQ-031 behaviour 7).
pub(super) const PASSED: &str = "readiness.passed";

/// An open question of a specification revision as the run's projection lists it (REQ-031 behaviour 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecQuestion {
    pub id: String,
    pub text: String,
    pub blocking: bool,
    pub spec_revision: i64,
}

impl SpecQuestion {
    /// As `projection.open_questions[]` carries it.
    pub fn json(&self) -> Value {
        json!({"id": self.id, "text": self.text, "blocking": self.blocking, "spec_revision": self.spec_revision})
    }
}

/// The `spec.open_questions` event of approved revision `spec_id` (none when it has none), in the caller's
/// transaction: `child_id(spec_id, "open_questions")`.
pub(super) fn open_questions_event(
    template: &NewEvent,
    spec_id: &str,
    revision: i64,
    questions: &[crate::specindex::Question],
) -> Option<NewEvent> {
    if questions.is_empty() {
        return None;
    }
    let listed: Vec<Value> = questions
        .iter()
        .map(|q| json!({"id": q.id, "text": q.text, "blocking": q.blocking, "path": q.path, "line": q.line}))
        .collect();
    Some(NewEvent {
        event_id: child_id(spec_id, "open_questions"),
        kind: OPEN_QUESTIONS.to_owned(),
        payload: json!({"spec_id": spec_id, "spec_revision": revision, "questions": listed}),
        ..template.clone()
    })
}

/// The open questions of approved revision `spec_id`.
pub(super) fn spec_questions(
    conn: &Connection,
    spec_id: &str,
) -> Result<Vec<SpecQuestion>, PersistError> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE kind = ?1 AND json_extract(payload_json, '$.spec_id') = ?2 \
             ORDER BY seq DESC LIMIT 1",
            params![OPEN_QUESTIONS, spec_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(payload) = payload else {
        return Ok(Vec::new());
    };
    let p: Value = serde_json::from_str(&payload).map_err(|e| {
        PersistError::IntegrityViolation(format!("{OPEN_QUESTIONS} of {spec_id}: {e}"))
    })?;
    let revision = p["spec_revision"].as_i64().unwrap_or_default();
    Ok(p["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|q| {
            Some(SpecQuestion {
                id: q["id"].as_str()?.to_owned(),
                text: q["text"].as_str().unwrap_or_default().to_owned(),
                // C4: blocking unless marked otherwise.
                blocking: q["blocking"].as_bool().unwrap_or(true),
                spec_revision: revision,
            })
        })
        .collect())
}

/// The open questions of run `run_id`'s current revision (none without one).
pub(super) fn run_questions(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<SpecQuestion>, PersistError> {
    let spec: Option<String> = conn
        .query_row(
            "SELECT spec_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    match spec {
        Some(spec) => spec_questions(conn, &spec),
        None => Ok(Vec::new()),
    }
}

/// Approved revision `spec_id` as readiness reads it: its requirements, their class and criteria (a criterion's
/// `then` is its indexed body's, or the body itself for a row that is not indexed JSON), and its open questions.
pub(super) fn revision(conn: &Connection, spec_id: &str) -> Result<Revision, PersistError> {
    let (number, sha256): (i64, String) = conn
        .query_row(
            "SELECT revision, sha256 FROM specs WHERE spec_id = ?1",
            params![spec_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let mut statement = conn
        .prepare(
            "SELECT requirement_id, release_class FROM requirements WHERE spec_id = ?1 ORDER BY requirement_id",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(String, String)> = statement
        .query_map(params![spec_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut criteria = conn
        .prepare(
            "SELECT ac_id, body, check_id FROM acceptance_criteria WHERE spec_id = ?1 AND requirement_id = ?2 \
             ORDER BY ac_id",
        )
        .map_err(sqlite_error)?;
    let mut requirements = Vec::with_capacity(rows.len());
    for (id, class) in rows {
        let found: Vec<(String, String, String)> = criteria
            .query_map(params![spec_id, id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        requirements.push(Requirement {
            id,
            required: class != "degradable",
            criteria: found
                .into_iter()
                .map(|(id, body, check)| Criterion {
                    id,
                    then: Some(match serde_json::from_str::<Value>(&body) {
                        Ok(Value::Object(indexed)) => indexed
                            .get("then")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        _ => body,
                    }),
                    check: Some(check),
                })
                .collect(),
        });
    }
    let questions = spec_questions(conn, spec_id)?
        .into_iter()
        .map(|q| OpenQuestion {
            id: q.id,
            blocking: q.blocking,
        })
        .collect();
    Ok(Revision {
        revision: number,
        sha256,
        requirements,
        questions,
    })
}

/// The tasks of run `run_id`'s plan (every task a `plan.imported` of it listed): their requirements under `spec_id`
/// and their dependencies. Runtime tasks (verification, specification review) are not the plan's.
pub(super) fn plan(
    conn: &Connection,
    run_id: &str,
    spec_id: &str,
) -> Result<Vec<PlanTask>, PersistError> {
    let ids = plan_ids(conn, run_id)?;
    let mut requirements = conn
        .prepare(
            "SELECT requirement_id FROM task_requirements WHERE task_id = ?1 AND spec_id = ?2 ORDER BY requirement_id",
        )
        .map_err(sqlite_error)?;
    let mut dependencies = conn
        .prepare("SELECT depends_on FROM task_dependencies WHERE task_id = ?1 ORDER BY depends_on")
        .map_err(sqlite_error)?;
    let mut tasks = Vec::with_capacity(ids.len());
    for task_id in ids {
        let requirement_ids = requirements
            .query_map(params![task_id, spec_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        let depends_on = dependencies
            .query_map(params![task_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        tasks.push(PlanTask {
            task_id,
            requirement_ids,
            depends_on,
        });
    }
    Ok(tasks)
}

/// The ids of run `run_id`'s planned tasks that exist.
fn plan_ids(conn: &Connection, run_id: &str) -> Result<BTreeSet<String>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT DISTINCT json_extract(j.value, '$.task_id') FROM events e, \
             json_each(json_extract(e.payload_json, '$.tasks')) j \
             WHERE e.run_id = ?1 AND e.kind = 'plan.imported' \
             AND json_extract(j.value, '$.task_id') IN (SELECT task_id FROM tasks WHERE run_id = ?1)",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// What readiness reads of revision `spec_id`'s specification review: its latest CURRENT report, else its latest
/// report (REJECTED), else none; with the reviewer's identities (its HoH session and stored native ones; the native
/// record's were judged at intake) and those of the revision's drafters and the Commander of the run that opened it
/// (CR-029, docs/adr/ADR-035.md decision 6).
pub(super) fn review(conn: &Connection, spec_id: &str) -> Result<ReviewView, PersistError> {
    let reported: Option<(String, String)> = conn
        .query_row(
            "SELECT project_id, payload_json FROM events WHERE kind = ?1 \
             AND json_extract(payload_json, '$.spec_id') = ?2 \
             ORDER BY json_extract(payload_json, '$.state') = 'CURRENT' DESC, seq DESC LIMIT 1",
            params![super::spec_review::REPORTED, spec_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, payload)) = reported else {
        return Ok(ReviewView::default());
    };
    let p: Value = serde_json::from_str(&payload).map_err(|e| {
        PersistError::IntegrityViolation(format!("a review report of {spec_id}: {e}"))
    })?;
    let sha256: String = conn
        .query_row(
            "SELECT sha256 FROM specs WHERE spec_id = ?1",
            params![p["spec_id"].as_str()],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let state = if p["state"] == "CURRENT" {
        ReviewState::Current
    } else {
        ReviewState::Rejected {
            reasons: p["reasons"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r.as_str().map(str::to_owned))
                .collect(),
        }
    };
    let status = match p["status"].as_str() {
        Some("PASS") => ReviewStatus::Pass,
        Some("FAIL") => ReviewStatus::Fail,
        _ => ReviewStatus::Unknown,
    };
    let findings = p["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some(Finding {
                id: f["id"].as_str()?.to_owned(),
                severity: match f["severity"].as_str()? {
                    "critical" => Severity::Critical,
                    "high" => Severity::High,
                    "medium" => Severity::Medium,
                    _ => Severity::Low,
                },
            })
        })
        .collect();
    let reviewer = match p["session_id"].as_str() {
        Some(session) => {
            super::spec_review::identities_of(conn, &BTreeSet::from([session.to_owned()]))?
        }
        None => Default::default(),
    };
    let opened_run: Option<String> = conn
        .query_row(
            "SELECT json_extract(payload_json, '$.run_id') FROM events WHERE kind = ?1 \
             AND json_extract(payload_json, '$.review_id') = ?2 ORDER BY seq LIMIT 1",
            params![super::spec_review::OPENED, p["review_id"].as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    let authors = match opened_run {
        Some(run) => super::spec_review::identities_of(
            conn,
            &super::spec_review::authors(conn, &run, &project_id, &sha256)?,
        )?,
        None => Default::default(),
    };
    Ok(ReviewView {
        latest: Some(Review {
            spec_revision: p["spec_revision"].as_i64().unwrap_or_default(),
            spec_sha256: sha256,
            state,
            status,
            findings,
            reviewer,
        }),
        authors,
    })
}

/// Run `run_id`'s readiness now (`None` when it has no specification), as a transition_run to READY reads it.
pub(super) fn run_readiness(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<Readiness>, PersistError> {
    let spec: Option<String> = conn
        .query_row(
            "SELECT spec_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let Some(spec) = spec else {
        return Ok(None);
    };
    Ok(Some(readiness::judge_run(
        &revision(conn, &spec)?,
        &plan(conn, run_id, &spec)?,
        &review(conn, &spec)?,
    )))
}

/// The planned tasks of run `run_id` whose readiness under its current revision does not pass now (none without a
/// revision), as the readiness step would judge them: what a scheduling round leaves undispatched in its own
/// transaction, whatever the step last wrote (M3J-01).
pub(super) fn unready(conn: &Connection, run_id: &str) -> Result<BTreeSet<String>, PersistError> {
    let spec: Option<String> = conn
        .query_row(
            "SELECT spec_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    let Some(spec) = spec else {
        return Ok(BTreeSet::new());
    };
    let plan = plan(conn, run_id, &spec)?;
    if plan.is_empty() {
        return Ok(BTreeSet::new());
    }
    let (revision, review) = (revision(conn, &spec)?, review(conn, &spec)?);
    Ok(plan
        .iter()
        .filter(|t| !readiness::judge_task(&revision, &plan, &review, &t.task_id).is_ready())
        .map(|t| t.task_id.clone())
        .collect())
}

/// Run `run_id`'s readiness step, one transaction (REQ-031 behaviour 5 and 7): in SPECIFYING, READY, RUNNING and
/// VERIFYING only. Each planned PENDING or READY task of the run's revision whose readiness does not pass is BLOCKED
/// (READINESS_NOT_MET, `task.blocked{details: {reasons, paths}}`); each one BLOCKED for it whose readiness passes goes
/// back to READY (its dependencies all ACCEPTED) or PENDING with `task.unblocked`; a SPECIFYING run whose readiness
/// passes moves to READY with `readiness.passed{run_id, spec_revision}`. Whether it wrote anything.
pub(super) fn settle(
    conn: &mut Connection,
    run_id: &str,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    // Judged first outside a write: a step with nothing to change begins no write at all.
    if judged(conn, run_id)?.is_none() {
        return Ok(false);
    }
    let tx = writer::begin(conn, ctx)?;
    let Some((commit, reasons)) = judged(&tx, run_id)? else {
        return Ok(false);
    };
    for (task_id, reason) in reasons {
        tx.execute(
            "UPDATE tasks SET blocked_reason = ?1, next_retry_at = CASE WHEN ?1 IS NULL THEN next_retry_at END \
             WHERE task_id = ?2",
            params![reason, task_id],
        )
        .map_err(sqlite_error)?;
    }
    writer::write(&tx, &commit, ctx)?;
    writer::commit(tx, ctx)?;
    Ok(true)
}

/// What run `run_id`'s readiness step would write, read from `conn`: its commit and each task's `blocked_reason` to
/// set first (none: cleared); `None` when nothing changes.
#[allow(clippy::type_complexity)]
fn judged(
    tx: &Connection,
    run_id: &str,
) -> Result<Option<(Commit, Vec<(String, Option<&'static str>)>)>, PersistError> {
    let run: Option<(String, String, i64, Option<String>)> = tx
        .query_row(
            "SELECT project_id, state, revision, spec_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, state, run_revision, Some(spec_id))) = run else {
        return Ok(None);
    };
    let state = writer::parse::<RunState>(&state)?;
    if !matches!(
        state,
        RunState::Specifying | RunState::Ready | RunState::Running | RunState::Verifying
    ) {
        return Ok(None);
    }
    let revision = revision(tx, &spec_id)?;
    let plan = plan(tx, run_id, &spec_id)?;
    let review = review(tx, &spec_id)?;
    let planned: BTreeSet<&str> = plan.iter().map(|t| t.task_id.as_str()).collect();
    let mut statement = tx
        .prepare(
            "SELECT task_id, state, revision FROM tasks WHERE run_id = ?1 AND spec_id = ?2 \
             AND (state IN ('PENDING', 'READY') OR (state = 'BLOCKED' AND blocked_reason = ?3)) ORDER BY task_id",
        )
        .map_err(sqlite_error)?;
    let tasks: Vec<(String, String, i64)> = statement
        .query_map(
            params![run_id, spec_id, BlockedReason::ReadinessNotMet.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    drop(statement);
    let event = |event_id: String, kind: &str, payload: Value| NewEvent {
        intent: Intent::Progress,
        project_id: project_id.clone(),
        event_id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let mut commit = Commit::default();
    let mut reasons_set = Vec::new();
    for (task_id, task_state, task_revision) in tasks
        .into_iter()
        .filter(|(id, ..)| planned.contains(id.as_str()))
    {
        let task_state = writer::parse::<TaskState>(&task_state)?;
        let judged = readiness::judge_task(&revision, &plan, &review, &task_id);
        let reason = BlockedReason::ReadinessNotMet.as_str();
        match (task_state, judged) {
            (TaskState::Pending | TaskState::Ready, Readiness::NotReady(reasons)) => {
                reasons_set.push((task_id.clone(), Some(reason)));
                commit.cas.push(StateCas {
                    id: task_id.clone(),
                    expected_revision: task_revision,
                    change: Change::Task(TaskState::Blocked),
                });
                let paths = super::plan::task_paths(tx, &task_id)?;
                commit.events.push(event(
                    child_id(&task_id, &format!("readiness/blocked/{task_revision}")),
                    "task.blocked",
                    json!({"task_id": task_id, "reason": reason, "spec_revision": revision.revision,
                           "details": {"reasons": reasons.iter().map(readiness::Reason::json).collect::<Vec<_>>(),
                                       "paths": paths}}),
                ));
            }
            (TaskState::Blocked, Readiness::Ready) => {
                let waiting: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM task_dependencies d JOIN tasks t ON t.task_id = d.depends_on \
                         WHERE d.task_id = ?1 AND t.state <> 'ACCEPTED')",
                        params![task_id],
                        |r| r.get(0),
                    )
                    .map_err(sqlite_error)?;
                let to = if waiting {
                    TaskState::Pending
                } else {
                    TaskState::Ready
                };
                reasons_set.push((task_id.clone(), None));
                commit.cas.push(StateCas {
                    id: task_id.clone(),
                    expected_revision: task_revision,
                    change: Change::Task(to),
                });
                commit.events.push(event(
                    child_id(&task_id, &format!("readiness/unblocked/{task_revision}")),
                    super::staffing::TASK_UNBLOCKED,
                    json!({"task_id": task_id, "reason": reason, "to": to.as_str(),
                           "spec_revision": revision.revision}),
                ));
            }
            _ => {}
        }
    }
    if state == RunState::Specifying && readiness::judge_run(&revision, &plan, &review).is_ready() {
        commit.cas.push(StateCas {
            id: run_id.to_owned(),
            expected_revision: run_revision,
            change: Change::Run(RunState::Ready),
        });
        commit.events.push(event(
            child_id(
                run_id,
                &format!("readiness/passed/{}/{run_revision}", revision.revision),
            ),
            PASSED,
            json!({"run_id": run_id, "spec_id": spec_id, "spec_revision": revision.revision}),
        ));
    }
    if commit.cas.is_empty() {
        return Ok(None);
    }
    Ok(Some((commit, reasons_set)))
}

#[cfg(test)]
mod tests;
