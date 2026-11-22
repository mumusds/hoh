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

    /// Whether this kind is applied by the decision itself: a request for a person passes the fences but is applied
    /// by its answer (docs/adr/ADR-031.md decision 7), so `Store::decide` refuses it before anything is written;
    /// create_tasks is applied with its plan (decision 2).
    pub fn applicable(&self) -> bool {
        !matches!(self.payload, DecisionPayload::RequestUserDecision { .. })
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
    /// The registry's one snapshot of the tick (docs/adr/ADR-030.md decision 1): what the pace judges whether a vacant
    /// role's tasks are dispatchable again by (decision 7); `None`: a host without a registry.
    pub staffing: Option<std::sync::Arc<crate::staffing::Snapshot>>,
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
    /// The agent's staffing fact read with it is usable and its compat suite verified it (docs/adr/ADR-030.md decisions
    /// 2, 7 and 11).
    pub verified: bool,
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
    /// `role_bindings.state` is ACTIVE and none of [`Pending`] holds: only then is the role dispatched.
    pub active: bool,
    /// `role_bindings.state` is BLOCKED (its only other state).
    pub blocked: bool,
    /// What keeps an ACTIVE binding from being dispatched now (CR-025): each [`Pending`] that holds.
    pub pending: Vec<Pending>,
    /// The probed capabilities of the bound session (`sessions.capabilities_json` keys that are true).
    pub capabilities: BTreeSet<String>,
}

/// A change of an ACTIVE binding that keeps it from being dispatched now and ends by itself, rejected as
/// ROLE_BINDING_PENDING (CR-025): exactly the cases that made such a binding not active before that code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// A team change of the role waits for its checkpoint (REQ-018, docs/adr/ADR-022.md decision 8).
    TeamChange,
    /// Staffing holds a rebind of the role pending for the attempt in flight (REQ-020, docs/adr/ADR-030.md decision 5).
    Rebind,
    /// The binding has no session yet.
    NoSession,
    /// The session.start notice of the role's epoch, which carries the handover package, is not delivered yet (REQ-019,
    /// docs/adr/ADR-024.md decision 4).
    HandoverUndelivered,
    /// The role's session owes its first input and has an attempt PREPARED, STARTING or RUNNING (M2T17R-201).
    FirstInputInFlight,
}

impl Pending {
    /// Whether the session that will serve the role is the bound one, so a capability it lacks is a real gap: so under
    /// a handover of it; under a team change, a rebind or with no session it is not settled, and no capability is
    /// judged.
    pub fn session_settled(self) -> bool {
        matches!(
            self,
            Pending::HandoverUndelivered | Pending::FirstInputInFlight
        )
    }

    /// As a rejection's detail says it.
    pub fn describe(self) -> &'static str {
        match self {
            Pending::TeamChange => "a team change of it waits for its checkpoint",
            Pending::Rebind => "staffing holds a rebind of it for the attempt in flight",
            Pending::NoSession => "it has no session yet",
            Pending::HandoverUndelivered => {
                "the session.start of its epoch, its handover package, is not delivered yet"
            }
            Pending::FirstInputInFlight => {
                "its session owes its first input (the handover package) and has an attempt in flight"
            }
        }
    }
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
    /// For a move of the run, what an end of it waits for (docs/adr/ADR-031.md decision 4).
    pub quiet: Quiet,
    /// For a record_assumption of an unattended run, the decision classes its configuration snapshot lists under
    /// `[assumptions]` (REQ-030 behaviour 4); `None` for an interactive run, whose basis is not checked.
    pub assumptions: Option<BTreeSet<String>>,
    /// For a transition_run to READY, the run's readiness under its specification revision (REQ-031, C5:
    /// docs/adr/ADR-035.md decision 4); `None` when none is judged (no specification adopted).
    pub readiness: Option<crate::readiness::Readiness>,
    /// For a dispatch, the dispatch admission's refusal of its task, if it refused it (docs/adr/ADR-035.md decision 12:
    /// its revision, who may dispatch it, its readiness under the run's current revision).
    pub admission: Option<Rejection>,
}

/// What an end of a run waits for (docs/adr/ADR-031.md decision 4): it is ended only when quiet, so nothing of it is
/// written after its run.result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Quiet {
    /// Its attempts not ended (PREPARED, STARTING, RUNNING), its work's and its supervision's.
    pub attempts_unfinished: u64,
    /// Its verification rounds not closed.
    pub open_rounds: u64,
    /// Its wake-kind events its supervisor has not taken in yet, counted up to `unread_limit + 1` at most.
    pub unread_wakes: u64,
    /// How many of those an end may take in in its own commit (one step's batch).
    pub unread_limit: u64,
}

/// A refused proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub reason: RejectionReason,
    pub detail: String,
    /// What the rejection names, machine-readable (`decision.rejected.details`), when it has more than its detail.
    pub details: Option<serde_json::Value>,
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
    /// The plan's tasks, dependencies and requirements, imported in the decision's transaction with the run's one
    /// revision move (docs/adr/ADR-031.md decision 2); its plan was checked against the run before.
    CreateTasks,
    /// A kind that is not applied yet ([`Proposal::applicable`]).
    Unsupported(&'static str),
}

fn reject<T>(reason: RejectionReason, detail: String) -> Result<T, Rejection> {
    Err(Rejection {
        reason,
        detail,
        details: None,
    })
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
        DecisionPayload::RecordAssumption { basis, .. } => record_assumption(snapshot, basis),
        DecisionPayload::CreateTasks { .. } => Ok(Effect::CreateTasks),
        DecisionPayload::RequestUserDecision { .. } => Ok(Effect::Unsupported(proposal.kind())),
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

/// The prefix of a basis that cites a configured default (`assumptions.<class>`, REQ-030 behaviour 4).
pub const ASSUMPTION_BASIS: &str = "assumptions.";

/// A record_assumption: in an unattended run its basis must be `assumptions.<class>` with the class listed in the run's
/// configuration snapshot (REQ-030 behaviour 4, AC-030-02); anything else is ASSUMPTION_NOT_LISTED, its details naming
/// the key. An interactive run's basis is not checked.
fn record_assumption(snapshot: &Snapshot, basis: &str) -> Result<Effect, Rejection> {
    let Some(listed) = &snapshot.assumptions else {
        return Ok(Effect::RecordAssumption);
    };
    let key = basis.strip_prefix(ASSUMPTION_BASIS);
    if key.is_some_and(|key| listed.contains(key)) {
        return Ok(Effect::RecordAssumption);
    }
    let shown = if listed.is_empty() {
        "none".to_owned()
    } else {
        listed.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    Err(Rejection {
        reason: RejectionReason::AssumptionNotListed,
        detail: format!(
            "basis \"{basis}\" is not a key listed in [assumptions] (listed: {shown}); an unattended run assumes only \
             those, cited as basis \"{ASSUMPTION_BASIS}<key>\""
        ),
        details: Some(serde_json::json!({"basis": basis, "key": key, "listed": listed})),
    })
}

fn table<S: Lifecycle>(from: S, to: S) -> Result<(), Rejection> {
    lifecycle::check(from, to).map_err(|e| Rejection {
        reason: e.reason(),
        detail: e.to_string(),
        details: None,
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
    if let Some(refused) = &snapshot.admission {
        return Err(refused.clone());
    }
    if let Some(next) = &task.retry_at {
        return reject(
            RetryNotDue,
            format!("{task_id} waits for its retry at {next}"),
        );
    }
    // The binding, in CR-025's order: no binding, BLOCKED and no role contract are ROLE_CAPABILITY_MISSING whatever is
    // pending; a change whose serving session is not settled is ROLE_BINDING_PENDING before any capability is judged; a
    // capability the session lacks is ROLE_CAPABILITY_MISSING before a handover of that session is pending. Each case
    // was rejected before that code; only the reason names it now.
    let Some(binding) = &snapshot.target else {
        return reject(RoleCapabilityMissing, format!("role {role} has no binding"));
    };
    if binding.blocked {
        return reject(RoleCapabilityMissing, format!("role {role} is blocked"));
    }
    if !binding.active && binding.pending.is_empty() {
        return reject(
            RoleCapabilityMissing,
            format!("role {role} has no active binding"),
        );
    }
    let Some(required) = &snapshot.required_capabilities else {
        return reject(
            RoleCapabilityMissing,
            format!(
                "no role contract states the capabilities {task_id} (role {}) needs",
                task.role
            ),
        );
    };
    let pending_detail = |pending: &[Pending]| {
        let why: Vec<&str> = pending.iter().map(|p| p.describe()).collect();
        format!(
            "role {role} is not dispatched while it changes: {}; {task_id} stays READY and is dispatched once that completed",
            why.join("; ")
        )
    };
    let unsettled: Vec<Pending> = binding
        .pending
        .iter()
        .copied()
        .filter(|p| !p.session_settled())
        .collect();
    if !unsettled.is_empty() {
        return reject(RoleBindingPending, pending_detail(&unsettled));
    }
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
    if !binding.pending.is_empty() {
        return reject(RoleBindingPending, pending_detail(&binding.pending));
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
    // An end only when quiet (docs/adr/ADR-031.md decision 4): nothing of the run still runs or is allocated, and a
    // COMPLETED or FAILED one also has no round left open and no more unread wakes than its commit takes in.
    let quiet = &snapshot.quiet;
    let refusal = |why: String| {
        reject(
            RejectionReason::IllegalTransition,
            format!("run {} -> {}: {why}", from.as_str(), to.as_str()),
        )
    };
    if matches!(
        to,
        RunState::Incomplete | RunState::Failed | RunState::Completed
    ) && quiet.attempts_unfinished > 0
    {
        return refusal(format!(
            "{} of its attempts have not ended",
            quiet.attempts_unfinished
        ));
    }
    if matches!(to, RunState::Failed | RunState::Completed) {
        if quiet.open_rounds > 0 {
            return refusal(format!(
                "{} of its verification rounds are open",
                quiet.open_rounds
            ));
        }
        if quiet.unread_wakes > quiet.unread_limit {
            return refusal(format!(
                "more than {} of its events are not taken in by its supervisor",
                quiet.unread_limit
            ));
        }
    }
    // Only readiness lets a run into READY (REQ-031 behaviour 1 and 7, C5): the Commander cannot overrule it.
    if to == RunState::Ready
        && let Some(crate::readiness::Readiness::NotReady(reasons)) = &snapshot.readiness
    {
        let named: Vec<String> = reasons
            .iter()
            .map(|r| format!("{} {}", r.code.as_str(), r.ids.join(",")))
            .collect();
        return Err(Rejection {
            reason: RejectionReason::IllegalTransition,
            detail: format!(
                "run {} -> READY: readiness is not met ({})",
                from.as_str(),
                named.join("; ")
            ),
            details: Some(serde_json::json!({
                "reasons": reasons.iter().map(crate::readiness::Reason::json).collect::<Vec<_>>()
            })),
        });
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
