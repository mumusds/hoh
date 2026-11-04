//! Runtime validation of Commander proposals (REQ-035, docs/adr/ADR-011.md §3). Pure: persistence reads the
//! [`Snapshot`] inside the transaction that records the outcome, so the verdict and the write see the same state.
//! The first failing check names the rejection reason.

use crate::lifecycle::{self, Lifecycle};
use crate::protocol::{
    self, DecisionPayload, DecodeError, Message, MessageKind, RejectionReason, RunState, TaskState,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// The role whose proposals these are.
pub const COMMANDER: &str = "commander";

/// A Commander proposal: the identity and fences of a `decision` message plus its schema-checked payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// The message id; also the `decisions` row id.
    pub decision_id: String,
    pub run_id: String,
    pub role_epoch: u64,
    /// The run revision the Commander decided on.
    pub expected_revision: u64,
    /// The specification the Commander decided on.
    pub spec_sha256: String,
    pub payload: DecisionPayload,
}

/// Why a message is not a proposal.
#[derive(Debug)]
pub enum ProposalError {
    NotADecision(MessageKind),
    NotFromCommander(String),
    /// The payload violates `$defs.DecisionPayload`.
    Payload(DecodeError),
}

impl fmt::Display for ProposalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADecision(kind) => write!(f, "a {kind:?} message is not a decision"),
            Self::NotFromCommander(role) => {
                write!(f, "decisions come from the commander, not {role}")
            }
            Self::Payload(e) => write!(f, "decision payload: {e}"),
        }
    }
}

impl std::error::Error for ProposalError {}

impl Proposal {
    /// The proposal a `decision` message carries; its payload is validated against `$defs.DecisionPayload`.
    pub fn from_message(message: &Message) -> Result<Self, ProposalError> {
        if message.kind != MessageKind::Decision {
            return Err(ProposalError::NotADecision(message.kind));
        }
        if message.sender_role != COMMANDER {
            return Err(ProposalError::NotFromCommander(message.sender_role.clone()));
        }
        let payload = protocol::decode::<DecisionPayload>(&Value::Object(message.payload.clone()))
            .map_err(ProposalError::Payload)?;
        Ok(Self {
            decision_id: message.message_id.clone(),
            run_id: message.run_id.clone(),
            role_epoch: message.role_epoch,
            expected_revision: message.expected_revision,
            spec_sha256: message.spec_sha256.clone(),
            payload,
        })
    }

    /// The payload's `kind`.
    pub fn kind(&self) -> &'static str {
        match self.payload {
            DecisionPayload::Dispatch { .. } => "dispatch",
            DecisionPayload::TransitionTask { .. } => "transition_task",
            DecisionPayload::TransitionRun { .. } => "transition_run",
            DecisionPayload::RequestUserDecision { .. } => "request_user_decision",
            DecisionPayload::CreateTasks { .. } => "create_tasks",
            DecisionPayload::RecordAssumption { .. } => "record_assumption",
        }
    }

    /// Whether this kind is applied yet: create_tasks arrives with plan validation (M2-T03), request_user_decision
    /// with the NEED_DECISION flow (M2-T13); until then they are refused before anything is written.
    pub fn applicable(&self) -> bool {
        !matches!(
            self.payload,
            DecisionPayload::CreateTasks { .. } | DecisionPayload::RequestUserDecision { .. }
        )
    }

    /// The task the payload names.
    pub fn task_id(&self) -> Option<&str> {
        match &self.payload {
            DecisionPayload::Dispatch { task_id, .. }
            | DecisionPayload::TransitionTask { task_id, .. } => Some(task_id),
            _ => None,
        }
    }

    /// The role a dispatch goes to.
    pub fn target_role(&self) -> Option<&str> {
        match &self.payload {
            DecisionPayload::Dispatch { role, .. } => Some(role),
            _ => None,
        }
    }

    /// Whether the verdict depends on acceptance evidence.
    pub fn needs_evidence(&self) -> bool {
        match &self.payload {
            DecisionPayload::TransitionTask { to_state, .. } => *to_state == TaskState::Accepted,
            DecisionPayload::TransitionRun {
                to_state,
                accepted_requirements,
                ..
            } => {
                *to_state == RunState::Completed
                    || accepted_requirements
                        .as_ref()
                        .is_some_and(|r| !r.is_empty())
            }
            _ => false,
        }
    }
}

/// Inputs the store does not hold yet, supplied by the caller and recorded with the verdict: the required
/// capabilities of each role's contract (team configuration, M2-T07) and the budget that can still be reserved, in
/// `cost_estimate` units (budget reservation, M2-T19; `None` = no limit).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub required_capabilities: BTreeMap<String, BTreeSet<String>>,
    pub budget_headroom: Option<u32>,
}

/// The run a proposal is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunView {
    pub state: RunState,
    pub revision: u64,
    pub spec_id: Option<String>,
    pub spec_sha256: Option<String>,
}

/// The task a proposal names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    pub task_id: String,
    pub state: TaskState,
    pub revision: u64,
    pub spec_id: String,
    pub role: String,
    pub cost_estimate: u32,
    /// Each dependency with its state.
    pub dependencies: Vec<(String, TaskState)>,
    /// The task's requirements under the run's current specification.
    pub requirements: BTreeSet<String>,
}

/// The binding of the role a dispatch goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingView {
    pub role_epoch: u64,
    /// `role_bindings.state` is ACTIVE.
    pub active: bool,
    /// The probed capabilities of the bound session (`sessions.capabilities_json` keys that are true).
    pub capabilities: BTreeSet<String>,
}

/// Acceptance evidence under the run's current specification (ADR-011 §3, the minimal fail-closed gate).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evidence {
    /// The required_release requirements of the specification.
    pub required: BTreeSet<String>,
    /// For each eligible candidate of the run, the requirements whose every acceptance criterion passed.
    pub candidates: BTreeMap<String, BTreeSet<String>>,
}

impl Evidence {
    /// Whether one candidate has passed every requirement in `needed` (which must not be empty).
    fn covers(&self, needed: &BTreeSet<String>) -> bool {
        !needed.is_empty()
            && self
                .candidates
                .values()
                .any(|passed| needed.is_subset(passed))
    }
}

/// Everything a verdict reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub run: RunView,
    /// The epoch of the run's commander binding.
    pub commander_epoch: Option<u64>,
    /// The task the payload names, if the run has it.
    pub task: Option<TaskView>,
    /// The binding of the dispatch target role.
    pub target: Option<BindingView>,
    /// The capabilities the contract of the named task's role requires; `None` when no contract names the role.
    pub required_capabilities: Option<BTreeSet<String>>,
    pub budget_headroom: Option<u32>,
    pub evidence: Evidence,
}

/// A refused proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub reason: RejectionReason,
    pub detail: String,
}

/// What an accepted proposal does; persistence applies it in the same transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// The task goes READY -> LEASED and a dispatch message to `role` (at its epoch) is queued.
    Dispatch {
        task_id: String,
        task_revision: u64,
        role: String,
        role_epoch: u64,
    },
    TransitionTask {
        task_id: String,
        task_revision: u64,
        from: TaskState,
        to: TaskState,
    },
    TransitionRun {
        from: RunState,
        to: RunState,
    },
    /// Only the event and the run revision.
    RecordAssumption,
    /// A kind that is not applied yet ([`Proposal::applicable`]).
    Unsupported(&'static str),
}

fn reject<T>(reason: RejectionReason, detail: String) -> Result<T, Rejection> {
    Err(Rejection { reason, detail })
}

/// The task moves the Commander may propose (ADR-011 §3.3); every other move follows attempts, the verification
/// barrier or blocking facts and belongs to the Runtime.
const COMMANDER_TASK_MOVES: [(TaskState, TaskState); 7] = [
    (TaskState::Pending, TaskState::Ready),
    (TaskState::Pending, TaskState::Cancelled),
    (TaskState::Ready, TaskState::Pending),
    (TaskState::Ready, TaskState::Cancelled),
    (TaskState::Implemented, TaskState::Ready),
    (TaskState::Implemented, TaskState::Cancelled),
    (TaskState::Verifying, TaskState::Accepted),
];

/// Judge `proposal` against `snapshot`.
pub fn validate(proposal: &Proposal, snapshot: &Snapshot) -> Result<Effect, Rejection> {
    use RejectionReason::*;
    if snapshot.commander_epoch != Some(proposal.role_epoch) {
        return reject(
            StaleRoleEpoch,
            format!(
                "role_epoch {} is not the commander's current epoch {:?}",
                proposal.role_epoch, snapshot.commander_epoch
            ),
        );
    }
    if proposal.expected_revision != snapshot.run.revision {
        return reject(
            StaleRevision,
            format!(
                "expected_revision {} but the run is at revision {}",
                proposal.expected_revision, snapshot.run.revision
            ),
        );
    }
    if let Some(current) = &snapshot.run.spec_sha256
        && *current != proposal.spec_sha256
    {
        return reject(
            SpecMismatch,
            format!(
                "decided on specification {} but the run's is {current}",
                proposal.spec_sha256
            ),
        );
    }
    if !lifecycle::is_working(snapshot.run.state) {
        return reject(
            IllegalTransition,
            format!(
                "the run is {}; decisions apply to working runs only",
                snapshot.run.state.as_str()
            ),
        );
    }
    match &proposal.payload {
        DecisionPayload::Dispatch { task_id, role, .. } => dispatch(snapshot, task_id, role),
        DecisionPayload::TransitionTask {
            task_id, to_state, ..
        } => transition_task(snapshot, task_id, *to_state),
        DecisionPayload::TransitionRun {
            to_state,
            accepted_requirements,
            ..
        } => transition_run(
            snapshot,
            *to_state,
            accepted_requirements.as_deref().unwrap_or_default(),
        ),
        DecisionPayload::RecordAssumption { .. } => Ok(Effect::RecordAssumption),
        DecisionPayload::RequestUserDecision { .. } | DecisionPayload::CreateTasks { .. } => {
            Ok(Effect::Unsupported(proposal.kind()))
        }
    }
}

fn named_task<'a>(snapshot: &'a Snapshot, task_id: &str) -> Result<&'a TaskView, Rejection> {
    match &snapshot.task {
        Some(task) if task.task_id == task_id => Ok(task),
        _ => reject(
            RejectionReason::IllegalTransition,
            format!("{task_id} is not a task of this run"),
        ),
    }
}

fn dependencies_accepted(task: &TaskView) -> Result<(), Rejection> {
    let pending: Vec<String> = task
        .dependencies
        .iter()
        .filter(|(_, state)| *state != TaskState::Accepted)
        .map(|(id, state)| format!("{id} ({})", state.as_str()))
        .collect();
    if pending.is_empty() {
        Ok(())
    } else {
        reject(
            RejectionReason::DependencyNotAccepted,
            format!(
                "{} depends on tasks not ACCEPTED: {}",
                task.task_id,
                pending.join(", ")
            ),
        )
    }
}

fn table<S: Lifecycle>(from: S, to: S) -> Result<(), Rejection> {
    lifecycle::check(from, to).map_err(|e| Rejection {
        reason: e.reason(),
        detail: e.to_string(),
    })
}

fn dispatch(snapshot: &Snapshot, task_id: &str, role: &str) -> Result<Effect, Rejection> {
    use RejectionReason::*;
    let task = named_task(snapshot, task_id)?;
    if Some(&task.spec_id) != snapshot.run.spec_id.as_ref() {
        return reject(
            SpecMismatch,
            format!(
                "{task_id} belongs to specification {} but the run's is {:?}",
                task.spec_id, snapshot.run.spec_id
            ),
        );
    }
    dependencies_accepted(task)?;
    table(task.state, TaskState::Leased)?;
    let binding = match &snapshot.target {
        Some(binding) if binding.active => binding,
        _ => {
            return reject(
                RoleCapabilityMissing,
                format!("role {role} has no active binding"),
            );
        }
    };
    let Some(required) = &snapshot.required_capabilities else {
        return reject(
            RoleCapabilityMissing,
            format!(
                "no role contract states the capabilities {task_id} (role {}) needs",
                task.role
            ),
        );
    };
    let missing: Vec<&str> = required
        .difference(&binding.capabilities)
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return reject(
            RoleCapabilityMissing,
            format!(
                "role {role} lacks {} required by {task_id} (role {})",
                missing.join(", "),
                task.role
            ),
        );
    }
    if let Some(headroom) = snapshot.budget_headroom
        && task.cost_estimate > headroom
    {
        return reject(
            BudgetNotReservable,
            format!(
                "{task_id} costs {} but only {headroom} can be reserved",
                task.cost_estimate
            ),
        );
    }
    Ok(Effect::Dispatch {
        task_id: task_id.to_owned(),
        task_revision: task.revision,
        role: role.to_owned(),
        role_epoch: binding.role_epoch,
    })
}

fn transition_task(snapshot: &Snapshot, task_id: &str, to: TaskState) -> Result<Effect, Rejection> {
    let task = named_task(snapshot, task_id)?;
    table(task.state, to)?;
    if !COMMANDER_TASK_MOVES.contains(&(task.state, to)) {
        return reject(
            RejectionReason::IllegalTransition,
            format!(
                "task {} -> {} is the Runtime's move, not a decision",
                task.state.as_str(),
                to.as_str()
            ),
        );
    }
    if to == TaskState::Ready {
        dependencies_accepted(task)?;
    }
    if to == TaskState::Accepted && !snapshot.evidence.covers(&task.requirements) {
        return reject(
            RejectionReason::EvidenceGateUnmet,
            format!(
                "no candidate has passing evidence for every acceptance criterion of {task_id}'s requirements {:?}",
                task.requirements
            ),
        );
    }
    Ok(Effect::TransitionTask {
        task_id: task_id.to_owned(),
        task_revision: task.revision,
        from: task.state,
        to,
    })
}

fn transition_run(
    snapshot: &Snapshot,
    to: RunState,
    accepted: &[String],
) -> Result<Effect, Rejection> {
    let from = snapshot.run.state;
    table(from, to)?;
    if !(lifecycle::is_working(to)
        || matches!(
            to,
            RunState::Incomplete | RunState::Failed | RunState::Completed
        ))
    {
        return reject(
            RejectionReason::IllegalTransition,
            format!(
                "run {} -> {} follows user commands and Runtime facts, not decisions",
                from.as_str(),
                to.as_str()
            ),
        );
    }
    let mut needed: BTreeSet<String> = accepted.iter().cloned().collect();
    if to == RunState::Completed {
        needed.extend(snapshot.evidence.required.iter().cloned());
        if snapshot.evidence.required.is_empty() {
            return reject(
                RejectionReason::EvidenceGateUnmet,
                "the specification has no required requirement to accept".to_owned(),
            );
        }
    }
    if !needed.is_empty() && !snapshot.evidence.covers(&needed) {
        return reject(
            RejectionReason::EvidenceGateUnmet,
            format!(
                "no candidate has passing evidence for every acceptance criterion of {needed:?}"
            ),
        );
    }
    Ok(Effect::TransitionRun { from, to })
}

#[cfg(test)]
mod tests;
