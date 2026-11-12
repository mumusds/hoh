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
    /// The candidate the Commander's verdict is about; acceptance is judged on it alone (CONTRACT 7).
    pub candidate_id: Option<String>,
    /// What it answers: the wake its turn was given (docs/adr/ADR-025.md decision 6), from the message's causation.
    pub causation_id: Option<String>,
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
            candidate_id: message.candidate_id.clone(),
            causation_id: message.causation_id.clone(),
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
    /// The time leases are judged and granted and retries come due at (`None`: the system clock).
    pub now: Option<std::time::SystemTime>,
    /// The wall-clock time absolute deadlines are judged at (`None`: `now`, else the system clock; ADR-020
    /// decision 4).
    pub wall: Option<std::time::SystemTime>,
    /// `execution.reader_slots` (REQ-037; `None`: the initial value 2).
    pub reader_slots: Option<u32>,
    /// `execution.reserve_supervisor_slot` (docs/adr/ADR-025.md decision 3; `None`: the initial value true).
    pub reserve_supervisor_slot: Option<bool>,
    /// The registry's last actual look at each agent it looked at (docs/adr/ADR-025.md decision 5); `None`: the caller
    /// gives none (a run's Commander is then judged without them).
    pub observations: Option<BTreeMap<String, Observation>>,
    /// Where Commander sessions get their own working directories, outside every project
    /// (`$XDG_STATE_HOME/hoh/commander`; docs/adr/ADR-025.md decision 3); `None`: none recorded.
    pub commander_dir: Option<std::path::PathBuf>,
}

/// One actual look of the host's registry at an agent (a probe or a login check; a cache unchanged is none): its identity
/// `(host_epoch, n)`, ordered host epoch first, and what it found (docs/adr/ADR-025.md decision 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    pub host_epoch: i64,
    pub n: u64,
    pub available: bool,
    pub logged_in: bool,
}

impl Context {
    /// The initial `execution.reader_slots` (REQ-037).
    pub const READER_SLOTS: u32 = 2;

    pub fn now(&self) -> std::time::SystemTime {
        self.now.unwrap_or_else(std::time::SystemTime::now)
    }

    pub fn wall(&self) -> std::time::SystemTime {
        self.wall
            .or(self.now)
            .unwrap_or_else(std::time::SystemTime::now)
    }

    pub fn reader_slots(&self) -> u32 {
        self.reader_slots.unwrap_or(Self::READER_SLOTS)
    }

    pub fn reserve_supervisor_slot(&self) -> bool {
        self.reserve_supervisor_slot.unwrap_or(true)
    }
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
    /// It writes the work tree, so it needs the writer lease (REQ-037); otherwise a reader slot.
    pub writer: bool,
    /// Each dependency with its state.
    pub dependencies: Vec<(String, TaskState)>,
    /// The task's requirements under the run's current specification.
    pub requirements: BTreeSet<String>,
    /// Its persisted `next_retry_at` while it is still ahead of the decision's time (REQ-042 behaviour 2); `None` when
    /// it is due or has no retry.
    pub retry_at: Option<String>,
    /// A verification round's verifier task: the Runtime's, which no decision moves or dispatches (docs/adr/ADR-028.md
    /// decision 10).
    pub runtime_owned: bool,
    /// A writer an open round with an open Issue holds: not dispatched until the round closes (docs/adr/ADR-028.md
    /// decision 13).
    pub held_by_open_round: bool,
}

/// The binding of the role a dispatch goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingView {
    pub role_epoch: u64,
    /// `role_bindings.state` is ACTIVE, a session is bound to it, and no team change waits for its checkpoint
    /// (docs/adr/ADR-022.md decision 8): only then is the role dispatched.
    pub active: bool,
    /// The probed capabilities of the bound session (`sessions.capabilities_json` keys that are true).
    pub capabilities: BTreeSet<String>,
}

/// Acceptance evidence under the run's current specification (ADR-011 §3, the minimal fail-closed gate).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evidence {
    /// The required_release requirements of the specification.
    pub required: BTreeSet<String>,
    /// The run's candidates in use (FROZEN, VERIFYING or ACCEPTED).
    pub live: BTreeSet<String>,
    /// For each eligible candidate of the run, the requirements whose every acceptance criterion passed.
    pub candidates: BTreeMap<String, BTreeSet<String>>,
}

impl Evidence {
    /// The gate for `what`: `candidate` must be the run's one candidate in use and have passed every requirement in
    /// `needed`, which must not be empty.
    fn gate(
        &self,
        candidate: Option<&str>,
        needed: &BTreeSet<String>,
        what: &str,
    ) -> Result<(), Rejection> {
        let Some(candidate) = candidate else {
            return reject(
                RejectionReason::EvidenceGateUnmet,
                format!("{what}: the proposal names no candidate"),
            );
        };
        if self.live.len() != 1 || !self.live.contains(candidate) {
            return reject(
                RejectionReason::CandidateMismatch,
                format!(
                    "{what}: {candidate} is not the run's one candidate in use {:?}",
                    self.live
                ),
            );
        }
        if needed.is_empty()
            || !self
                .candidates
                .get(candidate)
                .is_some_and(|passed| needed.is_subset(passed))
        {
            return reject(
                RejectionReason::EvidenceGateUnmet,
                format!(
                    "{what}: {candidate} lacks passing evidence for every acceptance criterion of {needed:?}"
                ),
            );
        }
        Ok(())
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
    /// For a dispatch, why the run's persisted budget lets nothing more be dispatched (a stop, or a cost limit with
    /// an unknown price), if it does (REQ-042 behaviour 6 and 7; M2T19R-01).
    pub budget_stop: Option<String>,
    /// For a dispatch, the free slot the named task needs (`writer` or `reader-k`); `None` when it is held (CR-007).
    pub free_slot: Option<String>,
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
    /// The task goes READY -> LEASED with a PREPARED attempt holding the lease `lease_kind`, and a dispatch message to
    /// `role` (at its epoch) is queued, all in the decision's transaction (AC-035-02).
    Dispatch {
        task_id: String,
        task_revision: u64,
        role: String,
        role_epoch: u64,
        lease_kind: String,
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
    // BLOCKED and WAITING_EXTERNAL judge task moves, assumptions and requests for a person only: a run's moves out
    // of them are the user's commands and the Runtime's facts (docs/adr/ADR-025.md decision 6, ADR-011 §3.4); asking
    // a person moves no run, and BLOCKED is when one is asked (v13, M2T08R-02).
    let suspended_ok = matches!(
        snapshot.run.state,
        RunState::Blocked | RunState::WaitingExternal
    ) && matches!(
        proposal.payload,
        DecisionPayload::TransitionTask { .. }
            | DecisionPayload::RecordAssumption { .. }
            | DecisionPayload::RequestUserDecision { .. }
    );
    if !lifecycle::is_working(snapshot.run.state) && !suspended_ok {
        return reject(
            IllegalTransition,
            format!(
                "the run is {}; decisions apply to working runs only (BLOCKED and WAITING_EXTERNAL: task moves, \
                 assumptions and requests for a person)",
                snapshot.run.state.as_str()
            ),
        );
    }
    match &proposal.payload {
        DecisionPayload::Dispatch { task_id, role, .. } => dispatch(snapshot, task_id, role),
        DecisionPayload::TransitionTask {
            task_id, to_state, ..
        } => transition_task(
            snapshot,
            proposal.candidate_id.as_deref(),
            task_id,
            *to_state,
        ),
        DecisionPayload::TransitionRun {
            to_state,
            accepted_requirements,
            ..
        } => transition_run(
            snapshot,
            proposal.candidate_id.as_deref(),
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
        // A supervision task is the Commander's own turn: no decision dispatches or moves it (docs/adr/ADR-025.md
        // appendix).
        Some(task) if task.task_id == task_id && task.role == "commander" => reject(
            RejectionReason::IllegalTransition,
            format!(
                "{task_id} is a supervision task, the Commander's own turn; no decision moves it"
            ),
        ),
        // A round's verifier task is the Runtime's: dispatched by the scheduling round, ended by the reconcile
        // (docs/adr/ADR-028.md decision 10).
        Some(task) if task.task_id == task_id && task.runtime_owned => reject(
            RejectionReason::IllegalTransition,
            format!(
                "{task_id} is a verification round's task, the Runtime's; no decision moves or dispatches it"
            ),
        ),
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
    if task.held_by_open_round {
        return reject(
            IllegalTransition,
            format!(
                "{task_id} writes what an open verification round with an open Issue covers; it waits for the round to \
                 close"
            ),
        );
    }
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
    if let Some(next) = &task.retry_at {
        return reject(
            RetryNotDue,
            format!("{task_id} waits for its retry at {next}"),
        );
    }
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
    if let Some(stop) = &snapshot.budget_stop {
        return reject(
            BudgetNotReservable,
            format!("the run's budget dispatches nothing more: {stop}"),
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
    let Some(lease_kind) = &snapshot.free_slot else {
        return reject(
            LeaseUnavailable,
            format!(
                "the {} lease {task_id} needs is held",
                if task.writer { "writer" } else { "reader" }
            ),
        );
    };
    Ok(Effect::Dispatch {
        task_id: task_id.to_owned(),
        task_revision: task.revision,
        role: role.to_owned(),
        role_epoch: binding.role_epoch,
        lease_kind: lease_kind.clone(),
    })
}

fn transition_task(
    snapshot: &Snapshot,
    candidate: Option<&str>,
    task_id: &str,
    to: TaskState,
) -> Result<Effect, Rejection> {
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
    if to == TaskState::Accepted {
        snapshot.evidence.gate(
            candidate,
            &task.requirements,
            &format!("accepting {task_id}"),
        )?;
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
    candidate: Option<&str>,
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
    if !needed.is_empty() {
        snapshot.evidence.gate(
            candidate,
            &needed,
            &format!("run {} -> {}", from.as_str(), to.as_str()),
        )?;
    }
    Ok(Effect::TransitionRun { from, to })
}

#[cfg(test)]
mod tests;

/// The capabilities a session's `capabilities_json` grants: the names set to `true` in an object; a probed capability
/// that is false or unknown is none, and anything but an object grants none. Dispatch checks a role's required
/// capabilities against these (`ROLE_CAPABILITY_MISSING`).
pub fn granted_capabilities(capabilities: &Value) -> BTreeSet<String> {
    capabilities
        .as_object()
        .map(|map| {
            map.iter()
                .filter(|(_, v)| **v == Value::Bool(true))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default()
}
