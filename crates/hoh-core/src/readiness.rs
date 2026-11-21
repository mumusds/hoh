//! The Readiness Gate (REQ-031, docs/adr/ADR-035.md): whether an approved specification revision, its independent
//! specification review and the run's plan let a run, or one of its tasks, go on. A pure function of what the caller
//! reads (C5: the Runtime judges, the Commander cannot overrule it): READY, or NOT_READY with its reasons, one per code
//! in a fixed order, each naming the requirement, criterion, task, question or finding ids it is about.

use crate::verification::identity::{self, Identities};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// An approved specification revision as readiness reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub revision: i64,
    pub sha256: String,
    pub requirements: Vec<Requirement>,
    pub questions: Vec<OpenQuestion>,
}

/// A requirement of the revision: its id, whether it is required (`release_class`), its acceptance criteria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub id: String,
    pub required: bool,
    pub criteria: Vec<Criterion>,
}

/// An acceptance criterion: what makes it observable is its `then` and its check (none, or blank, is missing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Criterion {
    pub id: String,
    pub then: Option<String>,
    pub check: Option<String>,
}

/// An entry of the revision's open questions and its blocking mark (C4: blocking unless marked otherwise).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenQuestion {
    pub id: String,
    pub blocking: bool,
}

/// A task of the run's plan: its id, its requirements, the tasks it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTask {
    pub task_id: String,
    pub requirement_ids: Vec<String>,
    pub depends_on: Vec<String>,
}

/// A specification review report's state when it was taken in (`spec_review.reported.state`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewState {
    Current,
    Rejected { reasons: Vec<String> },
}

/// A specification review's reported status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStatus {
    Pass,
    Fail,
    Unknown,
}

/// A finding's severity (contracts/spec-review-report.schema.json `$defs.Finding.severity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

/// A finding of a specification review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
}

/// The latest specification review report of the revision, with the reviewer's identities (its HoH session, its native
/// attempt record's session ids and its stored native identities, docs/adr/ADR-035.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub spec_revision: i64,
    pub spec_sha256: String,
    pub state: ReviewState,
    pub status: ReviewStatus,
    pub findings: Vec<Finding>,
    pub reviewer: Identities,
}

/// What readiness reads of the review: the latest report (none when there is none) and the identities of the sessions
/// that drafted the revision and of the Commander's, which the reviewer's must not share (CR-029).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewView {
    pub latest: Option<Review>,
    pub authors: Identities,
}

/// Why something is not ready, in the order reasons are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Code {
    MissingAc,
    Unobservable,
    DagMissingDependency,
    DagCycle,
    Undecided,
    SpecReview,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::MissingAc => "missing_ac",
            Code::Unobservable => "unobservable",
            Code::DagMissingDependency => "DAG_MISSING_DEPENDENCY",
            Code::DagCycle => "DAG_CYCLE",
            Code::Undecided => "undecided",
            Code::SpecReview => "spec_review",
        }
    }
}

/// Why the specification review does not satisfy readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDetail {
    Missing,
    StaleRevision,
    Rejected,
    NotIndependent,
    Fail,
    Unknown,
    BlockingFindings,
}

impl ReviewDetail {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewDetail::Missing => "missing",
            ReviewDetail::StaleRevision => "stale_revision",
            ReviewDetail::Rejected => "rejected",
            ReviewDetail::NotIndependent => "not_independent",
            ReviewDetail::Fail => "fail",
            ReviewDetail::Unknown => "unknown",
            ReviewDetail::BlockingFindings => "blocking_findings",
        }
    }
}

/// One reason: its code, the ids it names (sorted, unique; a cycle's tasks in dependency order) and, for
/// `spec_review`, its detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub code: Code,
    pub ids: Vec<String>,
    pub detail: Option<ReviewDetail>,
}

impl Reason {
    /// As `task.blocked.details.reasons[]` and `decision.rejected.details.reasons[]` carry it.
    pub fn json(&self) -> Value {
        let mut value = json!({"reason": self.code.as_str(), "ids": self.ids});
        if let Some(detail) = self.detail {
            value["detail"] = json!(detail.as_str());
        }
        value
    }
}

/// A readiness judgment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    NotReady(Vec<Reason>),
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Readiness::Ready)
    }

    /// Its reasons (none when ready).
    pub fn reasons(&self) -> &[Reason] {
        match self {
            Readiness::Ready => &[],
            Readiness::NotReady(reasons) => reasons,
        }
    }
}

/// The run's readiness: every requirement of the revision and of the plan, the whole plan, the blocking open questions
/// and the review.
pub fn judge_run(revision: &Revision, plan: &[PlanTask], review: &ReviewView) -> Readiness {
    let tasks: BTreeSet<&str> = plan.iter().map(|t| t.task_id.as_str()).collect();
    let mut requirements: BTreeSet<&str> = revision
        .requirements
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    requirements.extend(
        plan.iter()
            .flat_map(|t| t.requirement_ids.iter().map(String::as_str)),
    );
    judge(revision, plan, review, &requirements, &tasks)
}

/// Task `task_id`'s readiness: only the requirements of the task and of the tasks of its dependency closure, the
/// dependencies of that closure, the blocking open questions and the review (REQ-031 behaviour 5). A task not in the
/// plan is a missing dependency of itself.
pub fn judge_task(
    revision: &Revision,
    plan: &[PlanTask],
    review: &ReviewView,
    task_id: &str,
) -> Readiness {
    let by_id: BTreeMap<&str, &PlanTask> = plan.iter().map(|t| (t.task_id.as_str(), t)).collect();
    let mut closure = BTreeSet::new();
    let mut requirements = BTreeSet::new();
    let mut next = vec![task_id];
    while let Some(id) = next.pop() {
        if !closure.insert(id) {
            continue;
        }
        if let Some(task) = by_id.get(id) {
            requirements.extend(task.requirement_ids.iter().map(String::as_str));
            next.extend(task.depends_on.iter().map(String::as_str));
        }
    }
    judge(revision, plan, review, &requirements, &closure)
}

/// Readiness over `requirements` and the plan's `tasks` (the ones of `plan` among them, with their dependencies).
fn judge(
    revision: &Revision,
    plan: &[PlanTask],
    review: &ReviewView,
    requirements: &BTreeSet<&str>,
    tasks: &BTreeSet<&str>,
) -> Readiness {
    let mut found: BTreeMap<Code, BTreeSet<String>> = BTreeMap::new();
    let mut add = |code: Code, id: &str| {
        found.entry(code).or_default().insert(id.to_owned());
    };
    let known: BTreeMap<&str, &Requirement> = revision
        .requirements
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    for id in requirements {
        match known.get(id) {
            // A requirement the revision does not have has no criterion in it.
            None => add(Code::MissingAc, id),
            Some(requirement) => {
                if requirement.required && requirement.criteria.is_empty() {
                    add(Code::MissingAc, id);
                }
                for criterion in &requirement.criteria {
                    if !observable(criterion.then.as_deref())
                        || !observable(criterion.check.as_deref())
                    {
                        add(Code::Unobservable, &criterion.id);
                    }
                }
            }
        }
    }
    let mut deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let in_plan: BTreeSet<&str> = plan.iter().map(|t| t.task_id.as_str()).collect();
    for id in tasks.iter().filter(|id| !in_plan.contains(*id)) {
        add(Code::DagMissingDependency, id);
    }
    for task in plan.iter().filter(|t| tasks.contains(t.task_id.as_str())) {
        let mine = deps.entry(task.task_id.as_str()).or_default();
        for dep in &task.depends_on {
            if in_plan.contains(dep.as_str()) {
                mine.insert(dep.as_str());
            } else {
                found
                    .entry(Code::DagMissingDependency)
                    .or_default()
                    .insert(dep.clone());
            }
        }
    }
    let mut reasons: Vec<Reason> = found
        .into_iter()
        .map(|(code, ids)| Reason {
            code,
            ids: ids.into_iter().collect(),
            detail: None,
        })
        .collect();
    if let Err(cycle) = crate::dag::order(&deps) {
        reasons.push(Reason {
            code: Code::DagCycle,
            ids: cycle,
            detail: None,
        });
    }
    let undecided: Vec<String> = revision
        .questions
        .iter()
        .filter(|q| q.blocking)
        .map(|q| q.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !undecided.is_empty() {
        reasons.push(Reason {
            code: Code::Undecided,
            ids: undecided,
            detail: None,
        });
    }
    reasons.extend(judge_review(revision, review));
    reasons.sort_by_key(|r| r.code);
    if reasons.is_empty() {
        Readiness::Ready
    } else {
        Readiness::NotReady(reasons)
    }
}

/// A criterion's `then` or check is there: given and not blank.
fn observable(part: Option<&str>) -> bool {
    part.is_some_and(|p| !p.trim().is_empty())
}

/// Why the review does not satisfy readiness for `revision`; none when it does (REQ-031 behaviour 3: only the latest
/// CURRENT PASS of this revision, from a session independent of its drafters and the Commander, with no critical or
/// high finding; the reported status never overrides the report's own findings).
pub fn judge_review(revision: &Revision, review: &ReviewView) -> Option<Reason> {
    let not = |detail: ReviewDetail, ids: Vec<String>| {
        Some(Reason {
            code: Code::SpecReview,
            ids,
            detail: Some(detail),
        })
    };
    let Some(report) = &review.latest else {
        return not(ReviewDetail::Missing, Vec::new());
    };
    if report.spec_revision != revision.revision || report.spec_sha256 != revision.sha256 {
        return not(ReviewDetail::StaleRevision, Vec::new());
    }
    if let ReviewState::Rejected { reasons } = &report.state {
        let independence = reasons
            .iter()
            .any(|r| r.split(':').next() == Some(ReviewDetail::NotIndependent.as_str()));
        return not(
            if independence {
                ReviewDetail::NotIndependent
            } else {
                ReviewDetail::Rejected
            },
            Vec::new(),
        );
    }
    if !identity::shared(&report.reviewer, &review.authors).is_empty() {
        return not(ReviewDetail::NotIndependent, Vec::new());
    }
    match report.status {
        ReviewStatus::Fail => return not(ReviewDetail::Fail, Vec::new()),
        ReviewStatus::Unknown => return not(ReviewDetail::Unknown, Vec::new()),
        ReviewStatus::Pass => {}
    }
    let blocking: BTreeSet<String> = report
        .findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Critical | Severity::High))
        .map(|f| f.id.clone())
        .collect();
    if blocking.is_empty() {
        None
    } else {
        not(
            ReviewDetail::BlockingFindings,
            blocking.into_iter().collect(),
        )
    }
}

#[cfg(test)]
mod tests;
