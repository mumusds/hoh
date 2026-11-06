//! HCP wire types (`contracts/protocol.schema.json`). The schema stays authoritative: [`decode`] validates a value
//! against the type's definition before deserializing it and [`encode`] validates what it produced, so a value that
//! the schema refuses is an error here, never a panic. Free-form members (message and event `payload`, task result
//! `issues`) are kept as JSON objects, so fields HoH does not know survive a round trip, and event kinds are open
//! strings, so unknown events are kept as well. Enum values are checked against the schema by the test suite.

use crate::nullable;
use crate::schema::{self, SchemaError, Violation};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

/// The `schema_version` every HCP document carries.
pub const SCHEMA_VERSION: &str = "1.0";

/// Free-form JSON object kept verbatim.
pub type Object = Map<String, Value>;

/// The protocol schema definition a type corresponds to.
pub trait SchemaDef {
    const DEF: &'static str;
}

/// Every value of a schema enum, in schema order.
pub trait AllVariants: Sized + 'static {
    const ALL: &'static [Self];
}

/// Why a document could not be decoded or encoded.
#[derive(Debug)]
pub enum DecodeError {
    /// The value violates the schema definition.
    Schema(Vec<Violation>),
    /// The value is schema-valid but does not fit the Rust type.
    Json(serde_json::Error),
    /// The embedded schema has no such definition.
    Definition(SchemaError),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Schema(violations) => {
                write!(f, "schema violation")?;
                for v in violations.iter().take(5) {
                    write!(
                        f,
                        "; {} {} ({})",
                        if v.instance_path.is_empty() {
                            "/"
                        } else {
                            &v.instance_path
                        },
                        v.message,
                        v.schema_path
                    )?;
                }
                Ok(())
            }
            Self::Json(e) => write!(f, "does not fit the type: {e}"),
            Self::Definition(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn check(def: &str, value: &Value) -> Result<(), DecodeError> {
    schema::protocol()
        .validate_def(def, value)
        .map_err(DecodeError::Definition)?
        .map_err(DecodeError::Schema)
}

/// Validate `value` against `T`'s schema definition, then deserialize it.
pub fn decode<T: SchemaDef + DeserializeOwned>(value: &Value) -> Result<T, DecodeError> {
    check(T::DEF, value)?;
    serde_json::from_value(value.clone()).map_err(DecodeError::Json)
}

/// Serialize `value` and validate the result against `T`'s schema definition.
pub fn encode<T: SchemaDef + Serialize>(value: &T) -> Result<Value, DecodeError> {
    let json = serde_json::to_value(value).map_err(DecodeError::Json)?;
    check(T::DEF, &json)?;
    Ok(json)
}

macro_rules! schema_enum {
    ($(#[$meta:meta])* $name:ident, $rename:literal, [$($variant:ident),+ $(,)?]) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = $rename)]
        pub enum $name { $($variant),+ }
        impl AllVariants for $name {
            const ALL: &'static [Self] = &[$(Self::$variant),+];
        }
    };
}

schema_enum!(
    /// `Message.kind`.
    MessageKind, "snake_case", [Dispatch, Query, Handoff, Result, Decision, Steer, Cancel, Ack, Rejected]
);
schema_enum!(
    /// `ContextHandoff.handoff_kind`.
    HandoffKind, "snake_case", [Verification, RoleChange, Task]
);
schema_enum!(
    /// `TaskResult.status`.
    TaskStatus, "SCREAMING_SNAKE_CASE", [Completed, Failed, Blocked, Incomplete]
);
schema_enum!(
    /// `Command.kind`.
    CommandKind, "PascalCase",
    [StartRun, SetMode, SendInput, ConfigureTeam, ProposeSpecChange, PauseRun, ResumeRun, CancelRun, ApproveSpec, AnswerQuestion]
);
schema_enum!(
    /// `BlockedReason`: runs/tasks/role_bindings.blocked_reason.
    BlockedReason, "SCREAMING_SNAKE_CASE",
    [NeedDecision, BlockedAuth, BlockedCapability, CommanderUnavailable, BudgetUnenforceable, AgentUnavailable, ModelUnavailable,
     CompatUnverified]
);
schema_enum!(
    /// `RejectionReason`: decisions.reason_code and rejected messages.
    RejectionReason, "SCREAMING_SNAKE_CASE",
    [StaleRoleEpoch, StaleRevision, StaleLeaseEpoch, SpecMismatch, CandidateMismatch, IllegalTransition, DependencyNotAccepted,
     BudgetNotReservable, RoleCapabilityMissing, EvidenceGateUnmet, SessionNotIndependent, KeyReuse]
);
schema_enum!(
    /// `ErrorCode`: API errors, CLI stderr codes and attempt failure reasons.
    ErrorCode, "SCREAMING_SNAKE_CASE",
    [CapabilityUnsupported, RunNotFound, DbBusy, DbOnNetworkFs, DbWalUnsupported, DbFull, InvalidTeamConfig, ContextBudgetTooSmall,
     ProtocolFrameTooLarge, ProtocolIncomplete, DagCycle, DagMissingDependency, ScoreOutOfRange, CursorExpired]
);
schema_enum!(
    /// Task states (`transition_task.to_state`).
    TaskState, "SCREAMING_SNAKE_CASE", [Pending, Ready, Leased, Running, Implemented, Verifying, Accepted, Blocked, Cancelled]
);
schema_enum!(
    /// Run states (`transition_run.to_state`).
    RunState, "SCREAMING_SNAKE_CASE",
    [Created, Discovering, Specifying, Ready, Running, Verifying, PauseRequested, Paused, WaitingExternal, Blocked, CancelRequested,
     Cancelled, Incomplete, Failed, Completed]
);
schema_enum!(
    /// Run mode (`StartRun` / `SetMode`).
    Mode, "SCREAMING_SNAKE_CASE", [Plan, Build, Auto]
);
schema_enum!(
    /// `SendInput.input_kind`.
    InputKind, "SCREAMING_SNAKE_CASE", [Ask, Steer, Change, Control, Reply]
);
schema_enum!(
    /// `ApproveSpec.decision`.
    SpecDecision, "SCREAMING_SNAKE_CASE", [ApproveAndBuild, ApproveOnly, Reject]
);

/// `ReasonCode`: every reason code HoH emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReasonCode {
    Blocked(BlockedReason),
    Rejection(RejectionReason),
    Error(ErrorCode),
}

/// `ArtifactRef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub path: String,
    pub sha256: String,
}

/// `Candidate`: the frozen identity a verification refers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub git_commit: String,
    pub git_tree: String,
    pub spec_sha256: String,
    pub environment_sha256: String,
    pub test_plan_sha256: String,
}

/// `Message`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub schema_version: String,
    pub message_id: String,
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub sender_role: String,
    pub recipient_role: String,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub role_epoch: u64,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub idempotency_key: String,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub expected_revision: u64,
    pub spec_sha256: String,
    pub candidate_id: Option<String>,
    pub kind: MessageKind,
    pub payload: Object,
    /// Absent, null, or a reason; required (non-null) when `kind` is `rejected`.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nullable")]
    pub reason_code: Option<Option<RejectionReason>>,
}

/// `ContextHandoff.state`: the checkpoint package of a role change; ids reference rows, never transcripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffState {
    pub applied_decision_ids: Vec<String>,
    pub open_issue_ids: Vec<String>,
    pub unacked_message_ids: Vec<String>,
    pub queued_input_ids: Vec<String>,
}

/// `ContextHandoff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextHandoff {
    pub schema_version: String,
    pub run_id: String,
    pub task_id: Option<String>,
    pub goal: String,
    pub role: String,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub role_epoch: u64,
    pub spec_sha256: String,
    pub candidate: Option<Candidate>,
    pub acceptance_ids: Vec<String>,
    pub references: Vec<ArtifactRef>,
    pub output_contract: String,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub budget_tokens: u64,
    pub handoff_kind: HandoffKind,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub spec_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<HandoffState>,
}

/// `Event`. `kind` is open: kinds HoH does not know are kept as they are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: String,
    pub project_id: String,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub seq: u64,
    pub event_id: String,
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
    pub at: String,
    pub kind: String,
    pub payload: Object,
}

/// `TaskResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskResult {
    pub schema_version: String,
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role: String,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub role_epoch: u64,
    pub status: TaskStatus,
    pub summary: String,
    pub candidate_id: Option<String>,
    pub evidence: Vec<ArtifactRef>,
    pub issues: Vec<Object>,
    pub next_action: String,
}

/// `RunResult`: the result object of `--output json` and the payload of the final `run.result` event of
/// `--output stream-json` (REQ-004). `accepted`/`total` count the requirements of the current specification revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResult {
    pub run_id: String,
    pub state: RunState,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub exit_code: u8,
    pub blocked_reason: Option<BlockedReason>,
    #[serde(deserialize_with = "crate::integer::option")]
    pub spec_revision: Option<u64>,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub accepted: u64,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub total: u64,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub last_event_seq: u64,
}

impl RunResult {
    /// The `kind` of the event that carries a run result.
    pub const EVENT_KIND: &'static str = "run.result";
}

/// `Command`; the payload shape depends on the kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub schema_version: String,
    pub command_id: String,
    pub idempotency_key: String,
    #[serde(deserialize_with = "crate::integer::deserialize")]
    pub expected_revision: u64,
    pub run_id: Option<String>,
    #[serde(flatten)]
    pub body: CommandBody,
}

impl Command {
    pub fn kind(&self) -> CommandKind {
        match self.body {
            CommandBody::StartRun(_) => CommandKind::StartRun,
            CommandBody::SetMode(_) => CommandKind::SetMode,
            CommandBody::SendInput(_) => CommandKind::SendInput,
            CommandBody::ConfigureTeam(_) => CommandKind::ConfigureTeam,
            CommandBody::ProposeSpecChange(_) => CommandKind::ProposeSpecChange,
            CommandBody::PauseRun(_) => CommandKind::PauseRun,
            CommandBody::ResumeRun(_) => CommandKind::ResumeRun,
            CommandBody::CancelRun(_) => CommandKind::CancelRun,
            CommandBody::ApproveSpec(_) => CommandKind::ApproveSpec,
            CommandBody::AnswerQuestion(_) => CommandKind::AnswerQuestion,
        }
    }
}

/// `Command.kind` with its `payload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum CommandBody {
    StartRun(StartRun),
    SetMode(SetMode),
    SendInput(SendInput),
    ConfigureTeam(ConfigureTeam),
    ProposeSpecChange(ProposeSpecChange),
    PauseRun(Reason),
    ResumeRun(Reason),
    CancelRun(Reason),
    ApproveSpec(ApproveSpec),
    AnswerQuestion(AnswerQuestion),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRun {
    pub goal: String,
    pub mode: Mode,
    pub interactive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMode {
    pub mode: Mode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendInput {
    pub text: String,
    pub target_role: Option<String>,
    pub input_kind: InputKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigureTeam {
    pub bindings: Object,
    /// Always `checkpoint`.
    pub apply_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposeSpecChange {
    pub reason: String,
    pub spec_sha256: String,
}

/// Payload of `PauseRun`, `ResumeRun` and `CancelRun`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproveSpec {
    pub spec_sha256: String,
    pub decision: SpecDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerQuestion {
    pub question_id: String,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nullable")]
    pub choice_id: Option<Option<String>>,
}

/// `DecisionPayload`: a Commander proposal, validated by the Runtime before any state change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionPayload {
    Dispatch {
        task_id: String,
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    TransitionTask {
        task_id: String,
        to_state: TaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    TransitionRun {
        to_state: RunState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accepted_requirements: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    RequestUserDecision {
        question_id: String,
        question: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choices: Option<Vec<Choice>>,
        requires_user: bool,
    },
    CreateTasks {
        plan: PlanProposal,
    },
    RecordAssumption {
        statement: String,
        basis: String,
    },
}

impl DecisionPayload {
    /// The `kind` values, in schema order.
    pub const KINDS: &'static [&'static str] = &[
        "dispatch",
        "transition_task",
        "transition_run",
        "request_user_decision",
        "create_tasks",
        "record_assumption",
    ];
}

/// A choice offered by `request_user_decision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub choice_id: String,
    pub label: String,
}

/// `PlanProposal`: a task plan submitted for validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanProposal {
    pub tasks: Vec<PlannedTask>,
}

/// One task of a [`PlanProposal`]; the scores are 0..=5 (cost 1..=5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTask {
    pub task_id: String,
    pub title: String,
    pub role: String,
    pub writer: bool,
    pub depends_on: Vec<String>,
    pub requirement_ids: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::integer::option"
    )]
    pub user_value: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::integer::option"
    )]
    pub unblock_value: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::integer::option"
    )]
    pub risk_reduction: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::integer::option"
    )]
    pub information_gain: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::integer::option"
    )]
    pub cost_estimate: Option<u8>,
}

macro_rules! schema_def {
    ($($name:ident),+) => { $(impl SchemaDef for $name { const DEF: &'static str = stringify!($name); })+ };
}
schema_def!(
    ArtifactRef,
    Candidate,
    Message,
    ContextHandoff,
    Event,
    TaskResult,
    Command,
    DecisionPayload,
    PlanProposal,
    RunResult
);
