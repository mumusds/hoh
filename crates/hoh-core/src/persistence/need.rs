//! The questions' writers (docs/adr/ADR-031.md design correction A, E4, E16): every `need_decision` event is built
//! here, and a question's reason is a variant of a closed enum of its subject, so a pair with no exit cannot be written.
//!
//! - A task-level question's reason is a [`TaskReason`]: [`WorkReason`] for a development task's own work (12
//!   reasons), [`SourceReason`] for a round's source task held by its round or join (3), [`VerifierReason`] for a
//!   round's qa or reviewer task (8). A reason that holds only for some subjects has a variant only in their enums:
//!   there is no `VerifierReason::LocalRepairsExhausted` and no `WorkReason::Flaky`.
//! - [`task_event`] is the only builder of a blocking `need_decision`: it checks the variant against the task (a
//!   Verifier variant exactly for a round's verifier task, a Source variant only for a round's source task, a Work
//!   variant only for a task that is no verifier) before anything is written; a mismatch is an IntegrityViolation and
//!   the writer's transaction rolls back. A re-block while a repair refusal stands is [`refusal_reblock`]: no question,
//!   its anchor stays the refusal.
//! - A run-level question's reason is a [`RunReason`]; [`run_question`] claims `runtime:<reason>` and asks it.
//! - `questions::route` and `questions::route_run` match these enums exhaustively: every variant has its exit.
//!
//! The enums are written out (no macro: persistence makes none, scripts/product/persistence_scan.py); each one's
//! `position` is exhaustive, so a variant missing from its `ALL` fails the tests.

use super::PersistError;
use super::blockers::{self, Claims};
use super::writer::{Commit, Ctx, Intent, NewEvent};
use crate::protocol::BlockedReason;
use rusqlite::Connection;
use serde_json::{Value, json};

/// The event kind (REQ-021 behaviour 1's `need_decision`), and the name its writers' event ids are built on.
pub(super) const KIND: &str = "need_decision";

/// A development task's question about its own work (correction A's writers W1, W2, W3, W5, W12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkReason {
    /// W1: its retries are used up (REQ-042 behaviour 3).
    RetriesExhausted,
    /// W2: refused before anything ran, and the same input would be refused again (ADR-026 decision 9).
    RefusedDeterministically,
    /// W3: its protocol corrections are used up.
    ProtocolRepairsExhausted,
    /// W5: its local repairs of a failure item are used up.
    LocalRepairsExhausted,
    /// W12: its agent ended FAILED.
    AgentFailed,
    /// W12: its agent ended BLOCKED.
    AgentBlocked,
    /// W12: its agent ended INCOMPLETE.
    AgentIncomplete,
    /// W12: the host could not freeze its work.
    FreezeFailed,
    /// W12: no acceptance criterion to verify it against.
    NoAcceptanceCriteria,
    /// W12: no qa or reviewer bound to verify it.
    VerificationTeamMissing,
    /// W12: its result names a specification the run does not have.
    SpecMismatch,
    /// Its path scope holds files the user had not committed at the run's baseline (REQ-028 behaviour 4, O3;
    /// docs/adr/ADR-039.md decision 7).
    DirtyWorkspace,
}

impl WorkReason {
    /// Every variant, in order: its [`Self::position`] is its index here.
    pub const ALL: &'static [WorkReason] = &[
        WorkReason::RetriesExhausted,
        WorkReason::RefusedDeterministically,
        WorkReason::ProtocolRepairsExhausted,
        WorkReason::LocalRepairsExhausted,
        WorkReason::AgentFailed,
        WorkReason::AgentBlocked,
        WorkReason::AgentIncomplete,
        WorkReason::FreezeFailed,
        WorkReason::NoAcceptanceCriteria,
        WorkReason::VerificationTeamMissing,
        WorkReason::SpecMismatch,
        WorkReason::DirtyWorkspace,
    ];

    /// Its name: the `reason` of its `need_decision` and of its question.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkReason::RetriesExhausted => "retries_exhausted",
            WorkReason::RefusedDeterministically => "refused_deterministically",
            WorkReason::ProtocolRepairsExhausted => "protocol_repairs_exhausted",
            WorkReason::LocalRepairsExhausted => "local_repairs_exhausted",
            WorkReason::AgentFailed => "agent_failed",
            WorkReason::AgentBlocked => "agent_blocked",
            WorkReason::AgentIncomplete => "agent_incomplete",
            WorkReason::FreezeFailed => "freeze_failed",
            WorkReason::NoAcceptanceCriteria => "no_acceptance_criteria",
            WorkReason::VerificationTeamMissing => "verification_team_missing",
            WorkReason::SpecMismatch => "spec_mismatch",
            WorkReason::DirtyWorkspace => "dirty_workspace",
        }
    }

    /// Its index in [`Self::ALL`] (exhaustive: a new variant has one, and the tests find it missing there).
    pub fn position(self) -> usize {
        match self {
            WorkReason::RetriesExhausted => 0,
            WorkReason::RefusedDeterministically => 1,
            WorkReason::ProtocolRepairsExhausted => 2,
            WorkReason::LocalRepairsExhausted => 3,
            WorkReason::AgentFailed => 4,
            WorkReason::AgentBlocked => 5,
            WorkReason::AgentIncomplete => 6,
            WorkReason::FreezeFailed => 7,
            WorkReason::NoAcceptanceCriteria => 8,
            WorkReason::VerificationTeamMissing => 9,
            WorkReason::SpecMismatch => 10,
            WorkReason::DirtyWorkspace => 11,
        }
    }

    /// The variant named `text`.
    pub fn parse(text: &str) -> Option<WorkReason> {
        Self::ALL.iter().copied().find(|r| r.as_str() == text)
    }
}

/// A round's source task held by its round or its join (W6 to W9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceReason {
    /// W9: the join was passable and its gate refuses the task's requirements.
    JoinGateRefused,
    /// W6, W7: its reverifications (or its re-freezes) are used up.
    ReverifyExhausted,
    /// W8: a criterion is flaky on its candidate.
    Flaky,
}

impl SourceReason {
    /// Every variant, in order: its [`Self::position`] is its index here.
    pub const ALL: &'static [SourceReason] = &[
        SourceReason::JoinGateRefused,
        SourceReason::ReverifyExhausted,
        SourceReason::Flaky,
    ];

    /// Its name: the `reason` of its `need_decision` and of its question.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceReason::JoinGateRefused => "join_gate_refused",
            SourceReason::ReverifyExhausted => "reverify_exhausted",
            SourceReason::Flaky => "flaky",
        }
    }

    /// Its index in [`Self::ALL`] (exhaustive: a new variant has one, and the tests find it missing there).
    pub fn position(self) -> usize {
        match self {
            SourceReason::JoinGateRefused => 0,
            SourceReason::ReverifyExhausted => 1,
            SourceReason::Flaky => 2,
        }
    }

    /// The variant named `text`.
    pub fn parse(text: &str) -> Option<SourceReason> {
        Self::ALL.iter().copied().find(|r| r.as_str() == text)
    }
}

/// A round's verifier task (qa or reviewer; W1 to W4, W10, W11). A verifier owns no failure item, so a local
/// repair's exhaustion cannot be written for it:
///
/// ```compile_fail
/// let _ = hoh_core::persistence::need::VerifierReason::LocalRepairsExhausted;
/// ```
///
/// ```
/// let _ = hoh_core::persistence::need::VerifierReason::ProtocolRepairsExhausted;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VerifierReason {
    /// W1.
    RetriesExhausted,
    /// W2.
    RefusedDeterministically,
    /// W3: its output stayed invalid through its protocol corrections.
    ProtocolRepairsExhausted,
    /// W4: its own legal FAILED, BLOCKED or INCOMPLETE results used its round's reverifications up.
    ReverifyExhausted,
    /// W10: its attempt ended with an effect not confirmed.
    VerifierUnknownEffect,
    /// W11: its round's handoff does not decode or disagrees with its envelope.
    InvalidHandoff,
    /// W11: its session is one a source, the Commander, the Developer or the other verifier uses.
    SessionNotIndependent,
    /// W11: its report was rejected at its intake.
    ReportRejected,
}

impl VerifierReason {
    /// Every variant, in order: its [`Self::position`] is its index here.
    pub const ALL: &'static [VerifierReason] = &[
        VerifierReason::RetriesExhausted,
        VerifierReason::RefusedDeterministically,
        VerifierReason::ProtocolRepairsExhausted,
        VerifierReason::ReverifyExhausted,
        VerifierReason::VerifierUnknownEffect,
        VerifierReason::InvalidHandoff,
        VerifierReason::SessionNotIndependent,
        VerifierReason::ReportRejected,
    ];

    /// Its name: the `reason` of its `need_decision` and of its question.
    pub fn as_str(self) -> &'static str {
        match self {
            VerifierReason::RetriesExhausted => "retries_exhausted",
            VerifierReason::RefusedDeterministically => "refused_deterministically",
            VerifierReason::ProtocolRepairsExhausted => "protocol_repairs_exhausted",
            VerifierReason::ReverifyExhausted => "reverify_exhausted",
            VerifierReason::VerifierUnknownEffect => "verifier_unknown_effect",
            VerifierReason::InvalidHandoff => "INVALID_HANDOFF",
            VerifierReason::SessionNotIndependent => "SESSION_NOT_INDEPENDENT",
            VerifierReason::ReportRejected => "report_rejected",
        }
    }

    /// Its index in [`Self::ALL`] (exhaustive: a new variant has one, and the tests find it missing there).
    pub fn position(self) -> usize {
        match self {
            VerifierReason::RetriesExhausted => 0,
            VerifierReason::RefusedDeterministically => 1,
            VerifierReason::ProtocolRepairsExhausted => 2,
            VerifierReason::ReverifyExhausted => 3,
            VerifierReason::VerifierUnknownEffect => 4,
            VerifierReason::InvalidHandoff => 5,
            VerifierReason::SessionNotIndependent => 6,
            VerifierReason::ReportRejected => 7,
        }
    }

    /// The variant named `text`.
    pub fn parse(text: &str) -> Option<VerifierReason> {
        Self::ALL.iter().copied().find(|r| r.as_str() == text)
    }
}

/// A run-level question of the Runtime (W13, W14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RunReason {
    /// W13: the project has no approved specification for the run (decision 1, `adoption::ask`).
    NoSpec,
    /// W14: the Commander's turns moved the run no further (correction C).
    Stalled,
    /// The project's detected test commands, to run once into the baseline only when the user confirms (REQ-028
    /// behaviour 3, O3; docs/adr/ADR-039.md decision 8).
    BaselineTests,
    /// The committed draft awaits the user’s specification decision.
    SpecApproval,
    /// The drafter needs user input before another attempt.
    DraftQuestion,
}

impl RunReason {
    /// Every variant, in order: its [`Self::position`] is its index here.
    pub const ALL: &'static [RunReason] = &[
        RunReason::NoSpec,
        RunReason::Stalled,
        RunReason::BaselineTests,
        RunReason::SpecApproval,
        RunReason::DraftQuestion,
    ];

    /// Its name: the `reason` of its `need_decision` and of its question.
    pub fn as_str(self) -> &'static str {
        match self {
            RunReason::NoSpec => "no_spec",
            RunReason::Stalled => "stalled",
            RunReason::BaselineTests => "baseline_tests",
            RunReason::SpecApproval => "spec_approval",
            RunReason::DraftQuestion => "draft_question",
        }
    }

    /// Its index in [`Self::ALL`] (exhaustive: a new variant has one, and the tests find it missing there).
    pub fn position(self) -> usize {
        match self {
            RunReason::NoSpec => 0,
            RunReason::Stalled => 1,
            RunReason::BaselineTests => 2,
            RunReason::SpecApproval => 3,
            RunReason::DraftQuestion => 4,
        }
    }

    /// The variant named `text`.
    pub fn parse(text: &str) -> Option<RunReason> {
        Self::ALL.iter().copied().find(|r| r.as_str() == text)
    }
}

/// A task-level question's reason, typed by its subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TaskReason {
    Work(WorkReason),
    Source(SourceReason),
    Verifier(VerifierReason),
}

impl TaskReason {
    /// Its name.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskReason::Work(r) => r.as_str(),
            TaskReason::Source(r) => r.as_str(),
            TaskReason::Verifier(r) => r.as_str(),
        }
    }

    /// Its subject's name: `work`, `source` or `verifier` (decision 6).
    pub fn subject(self) -> &'static str {
        match self {
            TaskReason::Work(_) => "work",
            TaskReason::Source(_) => "source",
            TaskReason::Verifier(_) => "verifier",
        }
    }

    /// The reason `text` of a task's anchor: a verifier task's is a [`VerifierReason`]; another task's a
    /// [`SourceReason`] when it names one, else a [`WorkReason`].
    pub fn of(verifier: bool, text: &str) -> Option<TaskReason> {
        if verifier {
            return VerifierReason::parse(text).map(TaskReason::Verifier);
        }
        SourceReason::parse(text)
            .map(TaskReason::Source)
            .or_else(|| WorkReason::parse(text).map(TaskReason::Work))
    }

    /// Every pair (correction A's 22).
    pub fn all() -> Vec<TaskReason> {
        WorkReason::ALL
            .iter()
            .map(|r| TaskReason::Work(*r))
            .chain(SourceReason::ALL.iter().map(|r| TaskReason::Source(*r)))
            .chain(VerifierReason::ALL.iter().map(|r| TaskReason::Verifier(*r)))
            .collect()
    }
}

/// A blocking `need_decision` of task `task_id` of run `run_id`: its event id (each writer's own, unchanged), its
/// reason and its other fields (an object: attempt, detail, round, join, candidate, counts…).
pub(super) struct TaskNeed<'a> {
    pub project_id: &'a str,
    pub run_id: &'a str,
    pub task_id: &'a str,
    pub event_id: String,
    pub reason: TaskReason,
    pub fields: Value,
}

/// Whether task `task_id` of run `run_id` is a round's verifier task.
pub(super) fn is_verifier(
    conn: &Connection,
    run_id: &str,
    task_id: &str,
) -> Result<bool, PersistError> {
    Ok(super::verification::round_of_task(conn, run_id, task_id)?.is_some())
}

/// The `need_decision` of `need`, its variant checked against its task first (correction A's guard (2)): a Verifier
/// variant exactly for a round's verifier task, a Source variant only for a round's source task, a Work variant only
/// for a task that is no verifier. Any other pair is an IntegrityViolation: nothing is built, the caller's transaction
/// rolls back.
pub(super) fn task_event(conn: &Connection, need: TaskNeed<'_>) -> Result<NewEvent, PersistError> {
    if super::consult::is_child(conn, need.task_id)? {
        return Err(PersistError::IntegrityViolation(format!(
            "consult {} cannot ask the owner",
            need.task_id
        )));
    }

    let verifier = is_verifier(conn, need.run_id, need.task_id)?;
    let fits = match need.reason {
        TaskReason::Verifier(_) => verifier,
        TaskReason::Source(_) => {
            !verifier && super::verification::sources(conn, need.run_id)?.contains(need.task_id)
        }
        TaskReason::Work(_) => !verifier,
    };
    if !fits {
        return Err(PersistError::IntegrityViolation(format!(
            "{} cannot be a question of task {}: its subject is {}",
            need.reason.as_str(),
            need.task_id,
            if verifier {
                "a verifier"
            } else {
                "not that one"
            },
        )));
    }
    Ok(event(
        need.project_id,
        need.run_id,
        need.event_id,
        json!({"run_id": need.run_id, "task_id": need.task_id, "reason": need.reason.as_str()}),
        need.fields,
    ))
}

/// A task blocked again while a repair refusal of it stands (completion.rs's `repair_refused`): no question of its
/// own, its anchor stays the refusal (decision 6).
pub(super) fn refusal_reblock(
    project_id: &str,
    run_id: &str,
    task_id: &str,
    event_id: String,
    fields: Value,
) -> NewEvent {
    event(
        project_id,
        run_id,
        event_id,
        json!({"run_id": run_id, "task_id": task_id, "reason": "repair_refused"}),
        fields,
    )
}

/// An answer the Commander is to decide on (decision 7, decision 8's stalled retry): `need_decision{reason: answered}`
/// with `fields` (question_id, answer, choice_id, fingerprint…), which wakes it with the answer in its turn's context.
pub(super) fn answered(
    project_id: &str,
    run_id: &str,
    event_id: String,
    fields: Value,
) -> NewEvent {
    event(
        project_id,
        run_id,
        event_id,
        json!({"run_id": run_id, "reason": "answered"}),
        fields,
    )
}

fn event(project_id: &str, run_id: &str, event_id: String, base: Value, fields: Value) -> NewEvent {
    let mut payload = base;
    if let (Some(payload), Value::Object(more)) = (payload.as_object_mut(), fields) {
        for (key, value) in more {
            payload.entry(key).or_insert(value);
        }
    }
    NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id,
        run_id: Some(run_id.to_owned()),
        kind: KIND.to_owned(),
        payload,
    }
}

/// The claim source of run-level question `reason`.
pub(super) fn run_source(reason: RunReason) -> String {
    format!("runtime:{}", reason.as_str())
}

/// A run-level question of the Runtime (decision 6): `question_id` is its `question.asked` event's id, `fields` its
/// other fields (text, choices, …).
pub(super) struct RunQuestion<'a> {
    pub project_id: &'a str,
    pub run_id: &'a str,
    pub reason: RunReason,
    pub question_id: String,
    pub fields: Value,
    /// The time of the run's move, and why it is blocked.
    pub at: &'a str,
    pub detail: &'a str,
}

/// Ask run-level question `q` (correction A's guard (2), W13 and W14): NEED_DECISION claimed by `runtime:<reason>`,
/// its `question.asked{question_id, reason, …}` and the run's move (`blockers::follow`: a working run BLOCKED). What
/// to write, in the caller's transaction.
pub(super) fn run_question(
    tx: &rusqlite::Transaction<'_>,
    q: RunQuestion<'_>,
    ctx: &Ctx<'_>,
) -> Result<Commit, PersistError> {
    let mut claims = Claims::load(tx, q.project_id, q.run_id)?;
    claims.set(&run_source(q.reason), &[BlockedReason::NeedDecision]);
    let changed = claims.changed();
    let reasons = claims.reasons();
    let mut events = claims.store(tx)?;
    let mut payload = json!({"question_id": q.question_id, "reason": q.reason.as_str()});
    if let (Some(payload), Value::Object(more)) = (payload.as_object_mut(), q.fields) {
        for (key, value) in more {
            payload.entry(key).or_insert(value);
        }
    }
    events.push(NewEvent {
        intent: Intent::Progress,
        project_id: q.project_id.to_owned(),
        event_id: q.question_id.clone(),
        run_id: Some(q.run_id.to_owned()),
        kind: QUESTION_ASKED.to_owned(),
        payload,
    });
    let follow = blockers::follow(
        tx,
        q.project_id,
        q.run_id,
        changed,
        &reasons,
        q.at,
        q.detail,
        ctx,
    )?;
    events.extend(follow.events);
    Ok(Commit {
        cas: follow.cas,
        events,
        ..Commit::default()
    })
}

/// The kind of the event that asks a question (decision 6).
pub(super) const QUESTION_ASKED: &str = "question.asked";

#[cfg(test)]
mod tests;

/// A bounded discussion asks Commander once without claiming a blocker or masquerading as a task question.
pub(super) fn discussion_limit(
    project: &str,
    run: &str,
    controversy: &str,
    opinions: &[String],
    cause: &str,
) -> NewEvent {
    NewEvent {
        intent: Intent::Progress,
        project_id: project.into(),
        run_id: Some(run.into()),
        event_id: crate::ids::child_id(run, &format!("discussion/{controversy}/limit")),
        kind: KIND.into(),
        payload: json!({"reason":"discussion_limit","fingerprint":controversy,"controversy_id":controversy,"opinions":opinions,"cause":cause}),
    }
}

/// Structured changes to adopted interfaces or decisions require Commander deliberation.
pub(super) fn major_decision(project: &str, run: &str, class: &Value, statement: &str) -> NewEvent {
    let fingerprint = crate::context::sha256_hex(statement.as_bytes());
    NewEvent {
        intent: Intent::Progress,
        project_id: project.into(),
        run_id: Some(run.into()),
        event_id: crate::ids::child_id(run, &format!("major/{fingerprint}")),
        kind: KIND.into(),
        payload: json!({"reason":"major_decision","fingerprint":fingerprint,"decision_class":class,"statement":statement}),
    }
}
