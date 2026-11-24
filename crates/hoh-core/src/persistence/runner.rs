//! The host runner's writes (docs/adr/ADR-019.md decision 8): what is waiting for it (dispatch and run.started
//! notices, the working runs) and each step of an attempt it runs, every step one transaction:
//!
//! 1. received: the dispatch's receipt, its notice ACKED, the attempt STARTING and its work effect PREPARED;
//! 2. running: the process started and held at its start gate; the attempt RUNNING with its process identity, its
//!    work effect STARTED (write-ahead) and its task RUNNING, before the gate opens;
//! 3. the work effect OBSERVED with its outcome when the process ended, then COMMITTED;
//! 4. finished: the attempt SETTLED (its task IMPLEMENTED) or FAILED (its task READY).

use super::attempts::{self, AttemptFacts, AttemptMove};
use super::writer::{self, Change, Commit, Ctx, Intent, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::{AttemptState, Lifecycle};
use crate::protocol::TaskState;
use crate::reconcile::EffectState;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// The consumer named in the receipts of the runner's notices.
pub const RUNNER: &str = "runner";

/// A dispatch notice waiting for the runner: its attempt is still PREPARED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchNotice {
    pub message_id: String,
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role: String,
    /// What the attempt runs as (docs/adr/ADR-022.md decisions 9 to 12).
    pub assignment: super::team::Assignment,
}

/// What is waiting for the runner in a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Waiting {
    pub dispatches: Vec<DispatchNotice>,
    /// run.started notices not yet taken: (message id, run id).
    pub started: Vec<(String, String)>,
    /// The runs the runner schedules and reconciles the verification of (RUNNING, VERIFYING, and WAITING_EXTERNAL to
    /// be woken).
    pub runs: Vec<String>,
    /// The runs the runner paces: those of `runs` and the Commander's stages (CREATED, DISCOVERING, SPECIFYING, READY),
    /// for their budget only (docs/adr/ADR-031.md correction C, M2T13D-301).
    pub paced: Vec<String>,
    /// The runs whose pause waits for its safe point (PAUSE_REQUESTED) and those being cancelled (CANCEL_REQUESTED),
    /// which the host loop moves on with or without an executor (docs/adr/ADR-021.md decision 7).
    pub pausing: Vec<String>,
    /// Runs with an unfulfilled SetMode command.
    pub mode_changes: Vec<String>,
    /// Runs in DISCOVERING whose project discovery has not written `run.baseline` yet (docs/adr/ADR-039.md decision 4).
    pub discovering: Vec<String>,
    /// Runs whose user confirmed the baseline's test run and whose tests have not been recorded, each with its test
    /// commands (docs/adr/ADR-039.md decision 8).
    pub baseline_tests: Vec<(String, Vec<String>)>,
    pub cancelling: Vec<String>,
    /// Nonterminal runs behind the team revision, or with an idle role behind the host's resource bundle. A resource
    /// upgrade that only waits for in-flight attempts needs no checkpoint transaction yet (CR-026 C14).
    pub team_behind: Vec<String>,
    /// Every binding of every run not ended that staffing judges (docs/adr/ADR-030.md decision 1): a removed role, and a
    /// role whose configuration or resource change waits for its checkpoint, left out.
    pub staffing: Vec<crate::staffing::Staffed>,
    /// The project's current team: each role and the built-in role whose contract it follows (its own, a custom role's
    /// `builtin:<role>`, or none), for the capabilities it needs (decision 10).
    pub roles: Vec<(String, Option<String>)>,
    /// The agents the runner needs looked at: the project's current team's and those of the bindings in runs not ended
    /// (decision 8, third version; decision 10).
    pub agents: std::collections::BTreeSet<String>,
    /// The session.start notices waiting (docs/adr/ADR-024.md decision 4).
    pub session_starts: Vec<super::handover::SessionStart>,
    /// The runs whose supervisor steps (docs/adr/ADR-025.md decision 2): those not ended, and an ended one while it
    /// still has a PENDING trigger, or a woken or an output since its supervision was last closed (superseded and
    /// closed by its next step; v13, M2T08R-06).
    pub supervised: Vec<String>,
    /// The runs the stall detector looks at, last in each tick (docs/adr/ADR-031.md design correction C): their
    /// Commander bound and their specification adopted, in a working state or in one the tick's pace may put back in
    /// one (WAITING_EXTERNAL, BLOCKED; M2T13D-250): the list is read at the tick's start, and the detector judges the
    /// state in its own transaction.
    pub stall: Vec<String>,
}

/// One step of the runner.
#[derive(Debug, Clone)]
pub(super) enum RunnerJob {
    Waiting(String),
    /// A run's project baseline from the host's discovery (docs/adr/ADR-039.md decision 4).
    Baseline {
        run_id: String,
        payload: Box<Value>,
    },
    /// A run's baseline test run, once the user confirmed it (docs/adr/ADR-039.md decision 8).
    BaselineTests {
        run_id: String,
        payload: Box<Value>,
    },
    /// A baseline test command's process, recorded before its gate opens (docs/adr/ADR-039.md decision 8, DC-T07).
    BaselineProcess {
        run_id: String,
        payload: Box<Value>,
    },
    /// A recorded baseline test process that ended.
    BaselineProcessEnded {
        run_id: String,
        process: String,
        payload: Box<Value>,
    },
    /// Only the session.start notices waiting in a project (docs/adr/ADR-024.md decision 4), its runner's second look
    /// after a checkpoint or a staffing write (M2T17R-156).
    SessionStarts(String),
    /// A dispatch taken at this time (the runner's clock: its attempt's start, M2T19R-53).
    Received(Box<DispatchNotice>, std::time::SystemTime),
    Effect {
        attempt_id: String,
        key: String,
        from: EffectState,
        to: EffectState,
        detail: Value,
    },
    Running {
        attempt_id: String,
        facts: Box<AttemptFacts>,
        /// How its processes are tracked (`attempt.tracking`, docs/adr/ADR-021.md decision 8d v5), recorded with it.
        tracking: serde_json::Value,
        /// The prompt it was sent, recorded with it (docs/adr/ADR-026.md decision 7).
        prompt: Option<Box<PromptRecord>>,
    },
    Finished {
        attempt_id: String,
        ending: Box<Ending>,
    },
    /// A specification review attempt's end and, for a COMPLETED result, its report's intake: one transaction, so
    /// neither a kill nor another write comes between them (docs/adr/ADR-035.md decision 7).
    FinishedReview {
        attempt_id: String,
        ending: Box<Ending>,
        intake: Option<Box<super::spec_review::SpecReviewIntake>>,
    },
    /// Open the review due in a run at its revision's commit (REQ-031 behaviour 3).
    OpenReview {
        run_id: String,
        spec_id: String,
        git_commit: String,
    },
    /// A due review that waits for its revision's commit, once per revision and HEAD.
    ReviewWaiting {
        run_id: String,
        spec_id: String,
        head: String,
    },
    /// One more repair, or its refusal for a decision (ADR-020 decision 10).
    Repair(Box<super::retry::RepairRequest>),
    /// One step of a run's supervisor (docs/adr/ADR-025.md decision 2).
    Supervise {
        run_id: String,
        context: Box<crate::decision::Context>,
    },
    /// One owed wake of a project given a Commander turn, if one can be (docs/adr/ADR-025.md decision 3).
    Allocate {
        project_id: String,
        context: Box<crate::decision::Context>,
    },
    /// The supervision turns of a project to stop now (docs/adr/ADR-025.md decision 3).
    Stops {
        project_id: String,
        context: Box<crate::decision::Context>,
    },
    /// Withdraw a PREPARED supervision turn.
    WithdrawTurn(String, std::time::SystemTime),
    /// A run's supervisor step failed with this error (docs/adr/ADR-025.md v15 decision 5).
    StepFailed(String, String),
    /// One job of a run's output disposal (docs/adr/ADR-025.md v16 decision 2).
    Dispose {
        run_id: String,
        /// The cursor of the step whose verdict saw the turns end (v17).
        upto: i64,
        context: Box<crate::decision::Context>,
    },
    /// A run's readiness step (docs/adr/ADR-035.md decisions 4 and 8).
    Readiness(String),
    DraftStep(String, bool),
    DraftDispatch(String, Box<crate::decision::Context>),
    DraftInit(String),
    /// One look at a run as a whole (ADR-020 decisions 4, 5 and 7).
    Pace {
        run_id: String,
        context: Box<crate::decision::Context>,
    },
    Mode(String),
    Ack(String),
    /// A session.start notice settled (docs/adr/ADR-024.md decision 4): delivered to this session, or superseded.
    SessionStart(String, Option<String>),
    /// A session.start notice the executor did not take, with what it said (decision 4, M2T17R-150).
    SessionStartUndelivered(String, String),
    /// A session granted to a binding (docs/adr/ADR-022.md decision 9).
    BindSession(Box<super::team::SessionGrant>),
    /// A staffing judgment's write (docs/adr/ADR-030.md decision 5).
    Staff(Box<super::staffing::StaffChange>),
    /// A run's team checkpoint (docs/adr/ADR-022.md decision 8), with the agents the registry found unavailable.
    Checkpoint(String, super::team::Availability),
    /// Renew the unexpired leases these attempts hold, at `now` (REQ-041: every 30 s, for 120 s).
    Renew(Vec<String>, std::time::SystemTime),
    /// An inactivity watchdog's event of an attempt (docs/adr/ADR-022.md decision 19).
    Watched {
        attempt_id: String,
        event: Box<NewEvent>,
    },
    /// One look at a run for a stall (docs/adr/ADR-031.md design correction C), with the tick's pending candidates.
    Stall {
        run_id: String,
        pending: std::collections::BTreeSet<String>,
        context: Box<crate::decision::Context>,
    },
}

/// What a step gave.
#[derive(Debug)]
pub(super) enum RunnerDone {
    Waiting(Box<Waiting>),
    SessionStarts(Vec<super::handover::SessionStart>),
    /// Whether the step applied (a dispatch already taken or an attempt no longer PREPARED is not).
    Applied(bool),
    /// How many leases were renewed.
    Renewed(usize),
    Paced(super::retry::Paced),
    /// One batch of a supervisor step: none while it has not caught up (docs/adr/ADR-025.md v13 decision 2).
    Supervised(Option<super::supervise::Supervised>),
    Allocated(Option<super::wakes::Allocated>),
    Stops(Vec<super::wakes::Stop>),
    Repair(super::retry::RepairVerdict),
    Stall(super::stall::Detected),
    /// A baseline test process's record: its event id, none when the run no longer owns its tests.
    Process(Option<String>),
}

/// How an attempt ended, as the runner reports it (ADR-020 decisions 1 and 3).
#[derive(Debug, Clone, PartialEq)]
pub struct Ending {
    pub success: bool,
    pub detail: String,
    /// How it failed (unused when it succeeded).
    pub class: crate::retry::FailureClass,
    /// When it ended, by the runner's clock.
    pub at: std::time::SystemTime,
    /// A uniform 32-bit draw for the retry's jitter.
    pub draw: u32,
    /// What the agent's turn showed beyond its verdict, recorded with its end.
    pub observed: Observed,
    /// The turn ended after starting an effect whose outcome it never reported: the attempt ends UNKNOWN_EFFECT, its
    /// task untouched and nothing run again (ADR-019 decision 5, docs/adr/ADR-022.md decision 12, M2T07R-200).
    pub unknown_effect: bool,
}

/// What an agent's turn showed beyond its verdict (docs/adr/ADR-022.md decision 12): written in the commit of its end,
/// so a full writer queue cannot lose it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// The host's Task-level draft boundary observation, applied atomically with the ending.
    pub draft_boundary: Option<(String, Vec<String>)>,
    /// The model the agent ran (REQ-018: recorded after the attempt).
    pub resolved_model: Option<String>,
    pub usage: Option<serde_json::Value>,
    /// The agent's own session id, for the session row (resumed by later turns).
    pub native_session: Option<String>,
    /// The attempt's events, in order: (key, kind, payload); each event's id is the attempt's id and its key.
    pub events: Vec<(String, String, serde_json::Value)>,
    /// The prompt it was sent (docs/adr/ADR-022.md decision 11).
    pub prompt: Option<PromptRecord>,
    /// The directory of its raw logs.
    pub raw_log_path: Option<String>,
    /// The workspace's record, taken once its processes were proven gone: its end needs it when it is an interrupted end
    /// of the writer lease's holder, and the store asks for it then (REQ-019 behaviour 6, docs/adr/ADR-024.md decision
    /// 9).
    pub workspace: Option<serde_json::Value>,
    /// Its file-bridge lines: the runner carries them into its work's OBSERVED, where a supervision turn's are recorded
    /// as its outputs when it succeeded (docs/adr/ADR-025.md decision 8) and a work attempt's as `attempt.outputs`
    /// whatever its end (docs/adr/ADR-028.md decision 2).
    pub messages: Vec<String>,
    /// The bridge was read only up to its limit (`BRIDGE_LINES`, `BRIDGE_BYTES`): its last lines may be lost, so its
    /// result is not trusted (docs/adr/ADR-028.md decision 2, M2T16D-456).
    pub messages_truncated: bool,
    /// Its raw logs kept as objects (docs/adr/ADR-026.md decision 10): the runner carries them into its work's OBSERVED,
    /// where they are registered with their producer.
    pub artifacts: Vec<ArtifactRow>,
    /// Its raw logs that could not be kept or even read (`{name, error}`), recorded with the others so a missing
    /// reference is never silent (M2T09R-01).
    pub artifacts_failed: Vec<Value>,
    /// Its final assistant message, where a verifier's report block is read (docs/adr/ADR-028.md decision 15).
    pub final_text: Option<String>,
}

/// Where a turn's prompt came from and what was sent (decision 11, AC-018-05): a `prompt_revisions` row (the role
/// contract's layer, path and content sha256) and the attempt's `prompt_sha256` of the bytes sent; and how it was packed
/// (docs/adr/ADR-026.md decision 7). Written in the commit that marks the attempt RUNNING, before its start gate opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRecord {
    /// Ordered override sources; empty only for older/custom executors that record one source.
    pub chain: Vec<crate::prompt::Revision>,
    /// The effective override layer: `builtin`, `global`, `project` or `run`.
    pub layer: String,
    pub role: String,
    pub path: String,
    pub content_sha256: String,
    pub sent_sha256: String,
    /// What the context packer recorded; none from an executor that does not pack (a test's).
    pub packed: Option<Packing>,
}

/// A packed prompt's record (docs/adr/ADR-026.md decision 7): its resource bundle version, its `context.packed`
/// event's payload (the attempt id is added), and the objects it wrote, registered as artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packing {
    pub resource_bundle_version: String,
    pub event: serde_json::Value,
    pub artifacts: Vec<ArtifactRow>,
}

/// An object to register in `artifacts` (docs/adr/ADR-026.md decision 6): its sha256, its path relative to the
/// project (`.hoh/objects/sha256/<2>/<64>`), its size and type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactRow {
    pub sha256: String,
    pub path: String,
    pub bytes: u64,
    pub mime: String,
    /// What it is (`raw_log`, or a packed input's kind: `template`, `entry`, `manifest`, ...) and the name it stands
    /// for (a raw log's file name, an entry's path).
    pub kind: String,
    pub name: String,
}

impl ArtifactRow {
    /// As `attempt.artifacts` and the work effect's OBSERVED detail carry it.
    pub fn json(&self) -> Value {
        json!({"artifact_id": artifact_id(&self.sha256, &self.path), "sha256": self.sha256, "path": self.path,
            "bytes": self.bytes, "mime": self.mime, "kind": self.kind, "name": self.name})
    }

    /// One read back from [`ArtifactRow::json`].
    pub fn from_json(v: &Value) -> Option<ArtifactRow> {
        Some(ArtifactRow {
            sha256: v.get("sha256")?.as_str()?.to_owned(),
            path: v.get("path")?.as_str()?.to_owned(),
            bytes: v.get("bytes")?.as_u64()?,
            mime: v.get("mime")?.as_str()?.to_owned(),
            kind: v.get("kind")?.as_str()?.to_owned(),
            name: v.get("name")?.as_str()?.to_owned(),
        })
    }
}

/// An `artifacts` row's id: its content and its path.
pub(super) fn artifact_id(sha256: &str, path: &str) -> String {
    format!("{sha256}:{path}")
}

/// Register `rows` in `artifacts` in the caller's transaction, each read back (docs/adr/ADR-026.md decision 6): a row
/// of the same digest and path that says another size is no registration of it.
pub(super) fn register(tx: &Transaction<'_>, rows: &[ArtifactRow]) -> Result<(), PersistError> {
    for a in rows {
        tx.execute(
            "INSERT OR IGNORE INTO artifacts(artifact_id, sha256, path, bytes, mime) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                artifact_id(&a.sha256, &a.path),
                a.sha256,
                a.path,
                i64::try_from(a.bytes).unwrap_or(i64::MAX),
                a.mime
            ],
        )
        .map_err(sqlite_error)?;
        let bytes: i64 = tx
            .query_row(
                "SELECT bytes FROM artifacts WHERE sha256 = ?1 AND path = ?2",
                params![a.sha256, a.path],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if u64::try_from(bytes).ok() != Some(a.bytes) {
            return Err(PersistError::IntegrityViolation(format!(
                "artifact {} at {} is registered with {bytes} bytes, not {}",
                a.sha256, a.path, a.bytes
            )));
        }
    }
    Ok(())
}

impl PromptRecord {
    /// Its `prompt_revisions` id: the contract's layer, role, path and content, so two files of the same content are
    /// two sources (M2T07R-205).
    pub fn revision_id(&self) -> String {
        if let Some(leaf) = self.chain.last() {
            return leaf.revision_id.clone();
        }
        format!(
            "{}/{}/{}/{}",
            self.layer, self.role, self.content_sha256, self.path
        )
    }
}

pub(super) fn run(
    conn: &mut Connection,
    job: RunnerJob,
    ctx: &Ctx<'_>,
) -> Result<RunnerDone, PersistError> {
    match job {
        RunnerJob::Waiting(project_id) => {
            waiting(conn, &project_id, &ctx.resource_bundle.version())
                .map(|w| RunnerDone::Waiting(Box::new(w)))
        }
        RunnerJob::SessionStarts(project_id) => {
            super::handover::waiting(conn, &project_id).map(RunnerDone::SessionStarts)
        }
        RunnerJob::Received(notice, at) => {
            received(conn, &notice, at, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Effect {
            attempt_id,
            key,
            from,
            to,
            detail,
        } => effect(conn, &attempt_id, &key, from, to, detail, ctx).map(RunnerDone::Applied),
        RunnerJob::Running {
            attempt_id,
            facts,
            tracking,
            prompt,
        } => running(conn, &attempt_id, *facts, tracking, prompt.as_deref(), ctx)
            .map(RunnerDone::Applied),
        RunnerJob::Finished { attempt_id, ending } => {
            finished(conn, &attempt_id, &ending, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::FinishedReview {
            attempt_id,
            ending,
            intake,
        } => super::spec_review::finish(conn, &attempt_id, &ending, intake.as_deref(), ctx)
            .map(RunnerDone::Applied),
        RunnerJob::OpenReview {
            run_id,
            spec_id,
            git_commit,
        } => super::spec_review::open(conn, &run_id, &spec_id, &git_commit, ctx)
            .map(|opened| RunnerDone::Applied(opened.is_some())),
        RunnerJob::ReviewWaiting {
            run_id,
            spec_id,
            head,
        } => super::spec_review::waiting(conn, &run_id, &spec_id, &head, ctx)
            .map(RunnerDone::Applied),
        RunnerJob::Repair(request) => {
            super::retry::request_repair(conn, &request, ctx).map(RunnerDone::Repair)
        }
        RunnerJob::Allocate {
            project_id,
            context,
        } => super::wakes::allocate(conn, &project_id, &context, ctx).map(RunnerDone::Allocated),
        RunnerJob::Stops {
            project_id,
            context,
        } => super::wakes::stops(conn, &project_id, &context, ctx).map(RunnerDone::Stops),
        RunnerJob::WithdrawTurn(attempt_id, at) => {
            super::wakes::withdraw_turn(conn, &attempt_id, at, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Dispose {
            run_id,
            upto,
            context,
        } => super::supervise::dispose(conn, &run_id, upto, &context, ctx).map(RunnerDone::Applied),
        RunnerJob::StepFailed(run_id, error) => {
            super::supervise::step_failed(conn, &run_id, &error, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Supervise { run_id, context } => {
            super::supervise::step(conn, &run_id, &context, ctx).map(RunnerDone::Supervised)
        }
        RunnerJob::SessionStart(message_id, delivered_to) => {
            super::handover::settle(conn, &message_id, delivered_to.as_deref(), ctx)
                .map(RunnerDone::Applied)
        }
        RunnerJob::SessionStartUndelivered(message_id, detail) => {
            super::handover::undelivered(conn, &message_id, &detail, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::BindSession(grant) => {
            super::team::bind_session(conn, &grant, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Staff(change) => {
            super::staffing::staff(conn, &change, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Checkpoint(run_id, availability) => {
            super::team::checkpoint(conn, &run_id, &availability, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Mode(run) => super::modes::step(conn, &run, ctx).map(RunnerDone::Applied),
        RunnerJob::DraftStep(run, discovered) => {
            super::drafting::step(conn, &run, discovered, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::DraftDispatch(run, context) => {
            super::drafting::dispatch(conn, &run, &context, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::DraftInit(attempt) => {
            super::drafting::prepare_init(conn, &attempt, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Readiness(run_id) => {
            super::readiness::settle(conn, &run_id, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Baseline { run_id, payload } => {
            super::baseline::record(conn, &run_id, *payload, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::BaselineTests { run_id, payload } => {
            super::baseline::record_tests(conn, &run_id, *payload, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::BaselineProcess { run_id, payload } => {
            super::baseline::process_started(conn, &run_id, *payload, ctx).map(RunnerDone::Process)
        }
        RunnerJob::BaselineProcessEnded {
            run_id,
            process,
            payload,
        } => super::baseline::process_ended(conn, &run_id, &process, *payload, ctx)
            .map(RunnerDone::Applied),
        RunnerJob::Pace { run_id, context } => {
            super::retry::pace(conn, &run_id, &context, ctx).map(RunnerDone::Paced)
        }
        RunnerJob::Stall {
            run_id,
            pending,
            context,
        } => super::stall::detect(conn, &run_id, &pending, &context, ctx).map(RunnerDone::Stall),
        RunnerJob::Watched { attempt_id, event } => {
            watched(conn, &attempt_id, &event, ctx).map(RunnerDone::Applied)
        }
        RunnerJob::Renew(attempts, now) => {
            let tx = writer::begin(conn, ctx)?;
            let at = crate::time::rfc3339_utc(now);
            let until = crate::time::rfc3339_utc(now + super::LEASE_TTL);
            let mut renewed = 0;
            for attempt_id in &attempts {
                // Only a lease still unexpired: an expired one is the runner's to settle, never to revive.
                renewed += tx
                    .execute(
                        "UPDATE leases SET expires_at = ?1 WHERE attempt_id = ?2 AND expires_at > ?3",
                        params![until, attempt_id, at],
                    )
                    .map_err(sqlite_error)?;
            }
            super::writer::commit_unhooked(tx, ctx)?;
            Ok(RunnerDone::Renewed(renewed))
        }
        RunnerJob::Ack(message_id) => {
            let tx = writer::begin(conn, ctx)?;
            let changed = tx
                .execute(
                    "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE message_id = ?1 \
                     AND state IN ('PENDING', 'DELIVERING') AND json_extract(payload_json, '$.notice') IS 'runtime'",
                    params![message_id],
                )
                .map_err(sqlite_error)?;
            super::writer::commit_unhooked(tx, ctx)?;
            Ok(RunnerDone::Applied(changed == 1))
        }
    }
}

fn waiting(
    conn: &Connection,
    project_id: &str,
    resource_version: &str,
) -> Result<Waiting, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT o.message_id, o.run_id, json_extract(o.payload_json, '$.task_id'), \
             json_extract(o.payload_json, '$.attempt_id'), json_extract(o.payload_json, '$.role') \
             FROM outbox o JOIN runs r ON r.run_id = o.run_id \
             JOIN attempts a ON a.attempt_id = json_extract(o.payload_json, '$.attempt_id') \
             WHERE r.project_id = ?1 AND o.state = 'PENDING' AND json_extract(o.payload_json, '$.notice') IS 'runtime' \
             AND json_extract(o.payload_json, '$.kind') IS 'dispatch' AND a.state = 'PREPARED' \
             AND (r.state IN ('CREATED', 'DISCOVERING', 'SPECIFYING', 'READY', 'RUNNING', 'VERIFYING') \
                  OR (r.state IN ('BLOCKED', 'WAITING_EXTERNAL') AND json_extract(o.payload_json, '$.origin') IS NOT NULL \
                      AND json_extract(o.payload_json, '$.origin') NOT IN ('spec_review','spec_draft'))) \
             ORDER BY o.event_seq",
        )
        .map_err(sqlite_error)?;
    let dispatches = statement
        .query_map(params![project_id], |r| {
            Ok(DispatchNotice {
                message_id: r.get(0)?,
                run_id: r.get(1)?,
                task_id: r.get(2)?,
                attempt_id: r.get(3)?,
                role: r.get(4)?,
                assignment: super::team::Assignment::default(),
            })
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    let dispatches = dispatches
        .into_iter()
        .map(|mut notice| {
            notice.assignment = super::team::assignment(
                conn,
                project_id,
                &notice.run_id,
                &notice.task_id,
                &notice.attempt_id,
                &notice.role,
            )?;
            Ok(notice)
        })
        .collect::<Result<Vec<_>, PersistError>>()?;
    let mut statement = conn
        .prepare(
            "SELECT o.message_id, o.run_id FROM outbox o JOIN runs r ON r.run_id = o.run_id \
             WHERE r.project_id = ?1 AND o.state = 'PENDING' AND json_extract(o.payload_json, '$.notice') IS 'runtime' \
             AND json_extract(o.payload_json, '$.kind') IS 'run.started' ORDER BY o.event_seq",
        )
        .map_err(sqlite_error)?;
    let started = statement
        .query_map(params![project_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut statement = conn
        .prepare(
            "SELECT run_id FROM runs WHERE project_id = ?1 \
             AND state IN ('RUNNING', 'VERIFYING', 'WAITING_EXTERNAL', 'BLOCKED') ORDER BY created_at, run_id",
        )
        .map_err(sqlite_error)?;
    let runs = statement
        .query_map(params![project_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let in_state = |state: &str| -> Result<Vec<String>, PersistError> {
        let mut statement = conn
            .prepare("SELECT run_id FROM runs WHERE project_id = ?1 AND state = ?2 ORDER BY created_at, run_id")
            .map_err(sqlite_error)?;
        statement
            .query_map(params![project_id, state], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)
    };
    // A stopped run is not picked (docs/adr/ADR-031.md correction D): no step of the supervisor may write for it.
    let supervised = {
        let mut statement = conn
            .prepare(&format!(
                "SELECT r.run_id FROM runs r WHERE r.project_id = ?1 AND (r.state NOT IN ('CANCELLED', 'COMPLETED', 'FAILED') \
                 OR EXISTS (SELECT 1 FROM supervisor_triggers t WHERE t.run_id = r.run_id AND t.state = 'PENDING') \
                 OR EXISTS (SELECT 1 FROM events e WHERE e.run_id = r.run_id AND e.kind IN (?2, ?3) AND e.seq > \
                   (SELECT coalesce(max(c.seq), 0) FROM events c WHERE c.run_id = r.run_id AND c.kind = ?4))) \
                 AND {} ORDER BY r.created_at, r.run_id",
                super::hold::not_held("r.run_id")
            ))
            .map_err(sqlite_error)?;
        statement
            .query_map(
                params![
                    project_id,
                    super::wakes::WOKEN,
                    super::outputs::OUTPUT,
                    super::supervise::CLOSED
                ],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    let paced = {
        let mut statement = conn
            .prepare(
                "SELECT run_id FROM runs WHERE project_id = ?1 AND state IN ('CREATED', 'DISCOVERING', 'SPECIFYING', \
                 'READY', 'RUNNING', 'VERIFYING', 'WAITING_EXTERNAL', 'BLOCKED') ORDER BY created_at, run_id",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![project_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    let stall = {
        let mut statement = conn
            .prepare(
                "SELECT r.run_id FROM runs r WHERE r.project_id = ?1 \
                 AND r.state IN ('CREATED', 'DISCOVERING', 'SPECIFYING', 'READY', 'RUNNING', 'VERIFYING', \
                   'WAITING_EXTERNAL', 'BLOCKED') AND r.spec_id IS NOT NULL \
                 AND EXISTS(SELECT 1 FROM role_bindings b WHERE b.run_id = r.run_id AND b.role = 'commander') \
                 ORDER BY r.created_at, r.run_id",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![project_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    Ok(Waiting {
        paced,
        stall,
        supervised,
        session_starts: super::handover::waiting(conn, project_id)?,
        dispatches,
        started,
        runs,
        pausing: in_state("PAUSE_REQUESTED")?,
        mode_changes: super::modes::waiting(conn, project_id)?,
        discovering: super::baseline::waiting(conn, project_id)?,
        baseline_tests: super::baseline::tests_waiting(conn, project_id)?,
        cancelling: in_state("CANCEL_REQUESTED")?,
        team_behind: {
            let mut statement = conn
                .prepare(
                    "SELECT run_id FROM runs WHERE project_id = ?1 \
                     AND state NOT IN ('CANCELLED', 'COMPLETED', 'FAILED', 'INCOMPLETE') AND (config_revision < \
                     (SELECT coalesce(max(revision), 0) FROM team_revisions WHERE project_id = ?1 AND state = 'APPLIED') OR EXISTS ( \
                     SELECT 1 FROM role_bindings b WHERE b.run_id = runs.run_id \
                     AND NOT EXISTS (SELECT 1 FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                       WHERE t.run_id = b.run_id AND t.role = b.role AND a.state IN ('PREPARED', 'STARTING', 'RUNNING')) \
                     AND coalesce((SELECT json_extract(e.payload_json, '$.resource_bundle_version') \
                     FROM events e WHERE e.run_id = b.run_id AND e.kind = 'resource.pinned' AND json_extract(e.payload_json, '$.role') = b.role ORDER BY e.seq DESC LIMIT 1), '') <> ?2)) \
                     ORDER BY created_at, run_id",
                )
                .map_err(sqlite_error)?;
            statement
                .query_map(params![project_id, resource_version], |r| r.get(0))
                .map_err(sqlite_error)?
                .collect::<Result<_, _>>()
                .map_err(sqlite_error)?
        },
        staffing: super::staffing::staffed(conn, project_id, resource_version)?,
        roles: super::team::roles(conn, project_id)?,
        agents: super::team::agents(conn, project_id)?,
    })
}

/// The task of `attempt_id`, its state and revision, its run and project.
fn task_of(
    tx: &Transaction<'_>,
    attempt_id: &str,
) -> Result<(String, TaskState, i64, String, String), PersistError> {
    let row: Option<(String, String, i64, String, String)> = tx
        .query_row(
            "SELECT t.task_id, t.state, t.revision, t.run_id, r.project_id FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id JOIN runs r ON r.run_id = t.run_id WHERE a.attempt_id = ?1",
            params![attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let (task_id, state, revision, run_id, project_id) =
        row.ok_or_else(|| PersistError::IntegrityViolation(format!("no attempt {attempt_id}")))?;
    Ok((
        task_id,
        writer::parse::<TaskState>(&state)?,
        revision,
        run_id,
        project_id,
    ))
}

fn event(project_id: &str, run_id: &str, id: String, kind: &str, payload: Value) -> NewEvent {
    NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id: id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    }
}

fn received(
    conn: &mut Connection,
    notice: &DispatchNotice,
    at: std::time::SystemTime,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let taken = tx
        .execute(
            "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE message_id = ?1 AND state = 'PENDING'",
            params![notice.message_id],
        )
        .map_err(sqlite_error)?;
    let prepared: Option<String> = tx
        .query_row(
            "SELECT state FROM attempts WHERE attempt_id = ?1 AND cancel_requested = 0",
            params![notice.attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    // Nothing starts for a run that does not work (paused or cancelled meanwhile, docs/adr/ADR-021.md decision 7); a
    // runtime-class dispatch (its notice carries an origin) starts on a BLOCKED or WAITING_EXTERNAL run too
    // (docs/adr/ADR-025.md decision 3, CR-003), but a specification review's (REQ-031 behaviour 3).
    let working: bool = tx
        .query_row(
            "SELECT r.state IN ('CREATED', 'DISCOVERING', 'SPECIFYING', 'READY', 'RUNNING', 'VERIFYING') \
             OR (r.state IN ('BLOCKED', 'WAITING_EXTERNAL') AND EXISTS (SELECT 1 FROM outbox o WHERE o.message_id = ?2 \
                 AND json_extract(o.payload_json, '$.origin') IS NOT NULL \
                 AND json_extract(o.payload_json, '$.origin') NOT IN ('spec_review','spec_draft'))) FROM runs r WHERE r.run_id = ?1",
            params![notice.run_id, notice.message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .unwrap_or(false);
    if taken != 1 || prepared.as_deref() != Some(AttemptState::Prepared.as_str()) {
        return Ok(false);
    }
    if super::drafting::is_draft(&tx, &notice.task_id)?
        && let Err(refusal) = super::admission::admit_one(
            &tx,
            &notice.run_id,
            &notice.task_id,
            super::admission::By::Draft,
        )?
    {
        // PREPARED is proof that no process gate opened. End the leased attempt here, atomically with its receipt,
        // instead of retaining a notice that the pending mode and writer safe point would wait on forever.
        tx.execute(
            "INSERT INTO receipts(consumer,message_id,applied_at,result_json) VALUES(?1,?2,?3,?4)",
            params![
                RUNNER,
                notice.message_id,
                crate::time::rfc3339_utc(at),
                json!({"refused":notice.attempt_id}).to_string()
            ],
        )
        .map_err(sqlite_error)?;
        finish_in(
            &tx,
            &notice.attempt_id,
            &Ending {
                success: false,
                detail: format!("draft start admission refused: {refusal:?}"),
                class: crate::retry::FailureClass::Transient { retry_after: None },
                at,
                draw: 0,
                observed: Observed {
                    events: vec![(
                        "draft/start-refused".into(),
                        "spec.draft_start_refused".into(),
                        json!({"attempt_id":notice.attempt_id,"proof":"not_started","refusal":format!("{refusal:?}")}),
                    )],
                    ..Observed::default()
                },
                unknown_effect: false,
            },
            true,
            ctx,
        )?;
        writer::commit(tx, ctx)?;
        return Ok(false);
    }
    if !working {
        return Ok(false);
    }
    let now = crate::time::rfc3339_utc(at);
    // A refused repair standing holds its task whatever dispatched it (docs/adr/ADR-028.md decision 12, M2T16R-02):
    // nothing starts; the attempt ends CANCELLED (nothing counted), its lease with it, and the task waits BLOCKED for
    // the decision.
    if super::verification::refused(&tx, &notice.run_id)?.contains(&notice.task_id) {
        tx.execute(
            "INSERT INTO receipts(consumer, message_id, applied_at, result_json) VALUES (?1, ?2, ?3, ?4)",
            params![
                RUNNER,
                notice.message_id,
                now,
                json!({"refused": notice.attempt_id, "reason": "repair_refused"}).to_string()
            ],
        )
        .map_err(sqlite_error)?;
        attempts::move_attempt(
            &tx,
            &AttemptMove {
                attempt_id: notice.attempt_id.clone(),
                from: AttemptState::Prepared,
                to: AttemptState::Cancelled,
                facts: AttemptFacts {
                    ended_at: Some(now.clone()),
                    failure_reason: Some("REPAIR_REFUSED".to_owned()),
                    ..AttemptFacts::default()
                },
            },
            ctx,
        )?;
        let (task_id, task_state, revision, run_id, project_id) = task_of(&tx, &notice.attempt_id)?;
        let mut commit = Commit {
            events: vec![event(
                &project_id,
                &run_id,
                child_id(&notice.attempt_id, "refused"),
                "runner.refused",
                json!({"attempt_id": notice.attempt_id, "task_id": task_id, "reason": "repair_refused",
                    "detail": "a refused repair of this task waits for a decision"}),
            )],
            ..Commit::default()
        };
        if matches!(task_state, TaskState::Leased | TaskState::Running) {
            tx.execute(
                "UPDATE tasks SET blocked_reason = 'NEED_DECISION', next_retry_at = NULL WHERE task_id = ?1",
                params![task_id],
            )
            .map_err(sqlite_error)?;
            commit.cas.push(StateCas {
                id: task_id,
                expected_revision: revision,
                change: Change::Task(TaskState::Blocked),
            });
        }
        writer::write(&tx, &commit, ctx)?;
        super::writer::commit(tx, ctx)?;
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO receipts(consumer, message_id, applied_at, result_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            RUNNER,
            notice.message_id,
            now,
            json!({"received": notice.attempt_id}).to_string()
        ],
    )
    .map_err(sqlite_error)?;
    attempts::move_attempt(
        &tx,
        &AttemptMove {
            attempt_id: notice.attempt_id.clone(),
            from: AttemptState::Prepared,
            to: AttemptState::Starting,
            facts: AttemptFacts {
                started_at: Some(now),
                ..AttemptFacts::default()
            },
        },
        ctx,
    )?;
    let key = crate::reconcile::work_key(&notice.attempt_id);
    tx.execute(
        "INSERT INTO effects(effect_key, attempt_id, state) VALUES (?1, ?2, 'PREPARED')",
        params![key, notice.attempt_id],
    )
    .map_err(sqlite_error)?;
    let (_, _, _, run_id, project_id) = task_of(&tx, &notice.attempt_id)?;
    writer::write(
        &tx,
        &Commit {
            events: vec![event(
                &project_id,
                &run_id,
                child_id(&notice.attempt_id, "received"),
                "runner.received",
                json!({"attempt_id": notice.attempt_id, "task_id": notice.task_id, "message_id": notice.message_id,
                       "effect_key": key}),
            )],
            ..Commit::default()
        },
        ctx,
    )?;
    super::writer::commit(tx, ctx)?;
    Ok(true)
}

/// Attempt `attempt_id`'s inactivity watchdog event (docs/adr/ADR-022.md decision 19), written only while the attempt
/// runs and no cancel has begun, of the attempt or of its run (items 6 and 11, WD-01, WD-52): a look taken before the
/// attempt ended or a cancel began records nothing after. Whether it was written.
fn watched(
    conn: &mut Connection,
    attempt_id: &str,
    event: &NewEvent,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let running: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM attempts a JOIN tasks t ON t.task_id = a.task_id JOIN runs r ON r.run_id = t.run_id \
             WHERE a.attempt_id = ?1 AND a.state = 'RUNNING' AND a.cancel_requested = 0 \
             AND r.state NOT IN ('CANCEL_REQUESTED', 'CANCELLED')",
            params![attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if running.is_none() {
        return Ok(false);
    }
    writer::insert_event(&tx, event)?;
    writer::commit(tx, ctx)?;
    Ok(true)
}

fn effect(
    conn: &mut Connection,
    attempt_id: &str,
    key: &str,
    from: EffectState,
    to: EffectState,
    detail: Value,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let changed = tx
        .execute(
            "UPDATE effects SET state = ?1 WHERE effect_key = ?2 AND attempt_id = ?3 AND state = ?4",
            params![to.as_str(), key, attempt_id, from.as_str()],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return Ok(false);
    }
    let (task_id, _, _, run_id, project_id) = task_of(&tx, attempt_id)?;
    let kind = format!("effect.{}", to.as_str().to_ascii_lowercase());
    let mut detail = detail;
    // Its bridge lines: a supervision turn's recorded as its outputs in the transaction of its work's OBSERVED when it
    // succeeded, else discarded (docs/adr/ADR-025.md decision 8); a work attempt's kept as `attempt.outputs` whatever
    // its end (docs/adr/ADR-028.md decision 2); never kept in the effect's own event.
    let truncated = detail
        .as_object_mut()
        .and_then(|d| d.remove("messages_truncated"))
        .is_some_and(|t| t == json!(true));
    let messages = detail
        .as_object_mut()
        .and_then(|d| d.remove("messages"))
        .and_then(|m| match m {
            Value::Array(lines) => Some(lines),
            _ => None,
        })
        .unwrap_or_default();
    // Its raw logs kept as objects (docs/adr/ADR-026.md decision 10): registered with their producer in this
    // OBSERVED, so a crash before its end's commit keeps them; never kept in the effect's own event.
    let artifacts: Vec<ArtifactRow> = detail
        .as_object_mut()
        .and_then(|d| d.remove("artifacts"))
        .and_then(|a| {
            a.as_array()
                .map(|a| a.iter().filter_map(ArtifactRow::from_json).collect())
        })
        .unwrap_or_default();
    let failed: Vec<Value> = detail
        .as_object_mut()
        .and_then(|d| d.remove("artifacts_failed"))
        .and_then(|a| a.as_array().cloned())
        .unwrap_or_default();
    let mut events = vec![event(
        &project_id,
        &run_id,
        child_id(key, &kind),
        &kind,
        json!({"effect_key": key, "attempt_id": attempt_id, "from": from.as_str(), "detail": detail}),
    )];
    if to == EffectState::Observed
        && key == crate::reconcile::work_key(attempt_id)
        && (!artifacts.is_empty() || !failed.is_empty())
    {
        register(&tx, &artifacts)?;
        let (role, session): (Option<String>, Option<String>) = tx
            .query_row(
                "SELECT t.role, a.session_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE a.attempt_id = ?1",
                params![attempt_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sqlite_error)?;
        events.push(event(
            &project_id,
            &run_id,
            child_id(attempt_id, "artifacts"),
            "attempt.artifacts",
            json!({"attempt_id": attempt_id, "run_id": run_id, "role": role, "session_id": session,
                "items": artifacts.iter().map(ArtifactRow::json).collect::<Vec<_>>(), "failed": failed}),
        ));
    }
    if to == EffectState::Observed && key == crate::reconcile::work_key(attempt_id) {
        if super::wakes::is_supervision(&tx, &task_id)? {
            if super::inputs::for_attempt(&tx, attempt_id)?.is_some_and(|i| i.kind == "ASK") {
                if !messages.is_empty() {
                    events.push(event(&project_id,&run_id,child_id(attempt_id,"outputs-discarded"),"supervisor.output_discarded",json!({"attempt_id":attempt_id,"count":messages.len(),"detail":"read_only_ask"})));
                }
            } else {
                events.extend(super::outputs::recorded(
                    &project_id,
                    &run_id,
                    attempt_id,
                    detail["success"] == json!(true),
                    &messages,
                ));
            }
        } else if let Some(kept) = outputs(
            &tx,
            &project_id,
            &run_id,
            attempt_id,
            detail["success"] == json!(true),
            truncated,
            &messages,
        )? {
            events.push(kept);
        }
    }
    writer::write(
        &tx,
        &Commit {
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    super::writer::commit(tx, ctx)?;
    Ok(true)
}

/// Handoff `message_id` delivered to verifier attempt `attempt_id` as it starts (docs/adr/ADR-028.md decision 9): its
/// package object registered, the message ACKED, a `handoffs` row DELIVERED (one per message: a later attempt of the
/// same verifier task finds it there); its `handoff.delivered` event.
fn deliver_handoff(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    attempt_id: &str,
    message_id: &str,
    package: &ArtifactRow,
    at: &str,
) -> Result<NewEvent, PersistError> {
    register(tx, std::slice::from_ref(package))?;
    let (role, role_epoch): (String, i64) = tx
        .query_row(
            "SELECT recipient_role, role_epoch FROM outbox WHERE message_id = ?1",
            params![message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    tx.execute(
        "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE message_id = ?1 \
         AND state IN ('PENDING', 'DELIVERING')",
        params![message_id],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "INSERT OR IGNORE INTO handoffs(handoff_id, run_id, from_role, to_role, role_epoch, package_artifact_id, state, \
         created_at) VALUES (?1, ?2, 'runtime', ?3, ?4, ?5, 'DELIVERED', ?6)",
        params![
            message_id,
            run_id,
            role,
            role_epoch,
            artifact_id(&package.sha256, &package.path),
            at
        ],
    )
    .map_err(sqlite_error)?;
    Ok(event(
        project_id,
        run_id,
        child_id(attempt_id, "handoff"),
        "handoff.delivered",
        json!({"handoff_id": message_id, "attempt_id": attempt_id, "to_role": role, "role_epoch": role_epoch,
            "package": package.json()}),
    ))
}

/// A work attempt's bridge lines as one `attempt.outputs` event (docs/adr/ADR-028.md decision 2, M2T16D-404): its
/// completion and a reconcile read them through hoh_core::bridge::parse. Registered once, by whichever of its work's
/// OBSERVED and its end comes first (an UNKNOWN_EFFECT end has no OBSERVED); none when there is nothing to keep.
fn outputs(
    conn: &Connection,
    project_id: &str,
    run_id: &str,
    attempt_id: &str,
    success: bool,
    truncated: bool,
    lines: &[Value],
) -> Result<Option<NewEvent>, PersistError> {
    if lines.is_empty() && !truncated {
        return Ok(None);
    }
    let id = child_id(attempt_id, "outputs");
    let kept: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
            params![id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if kept {
        return Ok(None);
    }
    Ok(Some(event(
        project_id,
        run_id,
        id,
        "attempt.outputs",
        json!({"attempt_id": attempt_id, "success": success, "truncated": truncated, "lines": lines}),
    )))
}

/// The attempt's process started, held at its start gate: in one transaction the attempt RUNNING with the process's
/// identity, its work effect STARTED (write-ahead: before the gate opens) and its task RUNNING. The runner opens the
/// gate only after this commits, so an agent never runs unless its identity and its STARTED effect are durable
/// (ADR-019 decision 8; M2T05R-01, -52). False when the attempt is no longer STARTING with its work PREPARED.
fn running(
    conn: &mut Connection,
    attempt_id: &str,
    facts: AttemptFacts,
    tracking: serde_json::Value,
    prompt: Option<&PromptRecord>,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let (task_id, task_state, revision, run_id, project_id) = task_of(&tx, attempt_id)?;
    // Approval can cancel a STARTING attempt while its process waits at the gate (ADR-033, M3B-150).
    // Recheck in the start transaction, before recording any effect or allowing the host to open that gate.
    let can_start: bool = tx
        .query_row(
            "SELECT state = 'STARTING' AND cancel_requested = 0 FROM attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let draft = super::drafting::is_draft(&tx, &task_id)?;
    let refused = !can_start
        || task_state == TaskState::Cancelled
        || (draft
            && super::admission::admit_one(&tx, &run_id, &task_id, super::admission::By::Draft)?
                .is_err());
    let key = crate::reconcile::work_key(attempt_id);
    let started = if refused {
        0
    } else {
        tx.execute(
            "UPDATE effects SET state = 'STARTED' WHERE effect_key = ?1 AND attempt_id = ?2 AND state = 'PREPARED'",
            params![key, attempt_id],
        ).map_err(sqlite_error)?
    };
    if started != 1 {
        // A closed gate still has a process, including when the work effect changed (M3L-303).
        // Preserve its identity before any termination attempt (M3L-252),
        // without marking work STARTED or changing the Attempt state. Reconcile must never infer NoPid here.
        if draft
            && let (Some(pid), Some(boot), Some(start)) =
                (facts.pid, &facts.boot_id, &facts.process_start_id)
        {
            let changed = tx.execute(
                "UPDATE attempts SET pid=?2,boot_id=?3,process_start_id=?4 WHERE attempt_id=?1 AND state='STARTING' \
                 AND (pid IS NULL OR (pid=?2 AND boot_id=?3 AND process_start_id=?4))",
                params![attempt_id,pid,boot,start]).map_err(sqlite_error)?;
            if changed == 1 {
                writer::write(
                    &tx,
                    &Commit {
                        events: vec![event(
                            &project_id,
                            &run_id,
                            child_id(attempt_id, "tracking"),
                            "attempt.tracking",
                            tracking,
                        )],
                        ..Commit::default()
                    },
                    ctx,
                )?;
                writer::commit(tx, ctx)?;
            }
        }
        return Ok(false);
    }
    let at = facts
        .started_at
        .clone()
        .unwrap_or_else(|| crate::time::rfc3339_utc(std::time::SystemTime::now()));
    // A verifier's handoff, delivered in this commit (docs/adr/ADR-028.md decision 9).
    let delivered = match facts.handoff_package.as_deref().cloned() {
        Some((message_id, package)) => Some(deliver_handoff(
            &tx,
            &project_id,
            &run_id,
            attempt_id,
            &message_id,
            &package,
            &at,
        )?),
        None => None,
    };
    // A verifier's run directory, in this commit (docs/adr/ADR-028.md decision 6).
    let verification_start = facts.verification_start.as_deref().map(|start| {
        let mut payload = json!({"attempt_id": attempt_id});
        if let (Some(payload), Some(start)) = (payload.as_object_mut(), start.as_object()) {
            payload.extend(start.clone());
        }
        event(
            &project_id,
            &run_id,
            child_id(attempt_id, "verification_started"),
            "verification.attempt_started",
            payload,
        )
    });
    // A writer's workspace at its start, in this commit (docs/adr/ADR-028.md decisions 4 and 14).
    let workspace_start = facts.workspace_start.clone().map(|start| {
        let mut payload = json!({"attempt_id": attempt_id});
        if let (Some(payload), Some(start)) = (payload.as_object_mut(), start.as_object()) {
            payload.extend(start.clone());
        }
        event(
            &project_id,
            &run_id,
            child_id(attempt_id, "workspace_start"),
            "attempt.workspace_start",
            payload,
        )
    });
    attempts::move_attempt(
        &tx,
        &AttemptMove {
            attempt_id: attempt_id.to_owned(),
            from: AttemptState::Starting,
            to: AttemptState::Running,
            facts,
        },
        ctx,
    )?;
    let cas = if task_state == TaskState::Leased {
        vec![StateCas {
            id: task_id,
            expected_revision: revision,
            change: Change::Task(TaskState::Running),
        }]
    } else {
        Vec::new()
    };
    // The prompt it is about to be sent, before its gate opens (docs/adr/ADR-026.md decision 7).
    let packed = match prompt {
        Some(prompt) => record_prompt(&tx, &project_id, &run_id, attempt_id, prompt, &at)?,
        None => Vec::new(),
    };
    writer::write(
        &tx,
        &Commit {
            cas,
            events: packed
                .into_iter()
                .chain(workspace_start)
                .chain(delivered)
                .chain(verification_start)
                .chain([
                event(
                    &project_id,
                    &run_id,
                    child_id(&key, "effect.started"),
                    "effect.started",
                    json!({"effect_key": key, "attempt_id": attempt_id, "from": "PREPARED", "detail": {}}),
                ),
                event(
                    &project_id,
                    &run_id,
                    child_id(attempt_id, "tracking"),
                    "attempt.tracking",
                    tracking,
                ),
            ])
            .collect(),
            ..Commit::default()
        },
        ctx,
    )?;
    super::writer::commit(tx, ctx)?;
    Ok(true)
}

/// Write `prompt`, the record of what attempt `attempt_id` is sent, in the caller's transaction: its source's
/// `prompt_revisions` row, the attempt's `prompt_revision`, `prompt_sha256` and `resource_bundle_version`, the objects
/// it packed as `artifacts` rows (each read back), and its `context.packed` event, returned for the caller's commit
/// (decision 11; docs/adr/ADR-026.md decisions 6 and 7).
pub(super) fn record_prompt(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    attempt_id: &str,
    prompt: &PromptRecord,
    at: &str,
) -> Result<Vec<NewEvent>, PersistError> {
    let revision = prompt.revision_id();
    if prompt.chain.is_empty() {
        tx.execute(
        "INSERT OR IGNORE INTO prompt_revisions(revision_id, layer, role, content_sha256, path, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![revision, prompt.layer, prompt.role, prompt.content_sha256, prompt.path, at],
    )
        .map_err(sqlite_error)?;
    } else {
        let mut parent: Option<&str> = None;
        for source in &prompt.chain {
            if source.parent_revision.as_deref() != parent {
                return Err(PersistError::IntegrityViolation(
                    "prompt chain parent mismatch".to_owned(),
                ));
            }
            tx.execute("INSERT OR IGNORE INTO prompt_revisions(revision_id, layer, role, content_sha256, path, parent_revision, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![source.revision_id, source.layer, prompt.role, source.sha256, source.path, source.parent_revision, at]).map_err(sqlite_error)?;
            let stored: (String, String, String, String, Option<String>) = tx.query_row(
                "SELECT layer, role, content_sha256, path, parent_revision FROM prompt_revisions WHERE revision_id = ?1",
                [&source.revision_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map_err(sqlite_error)?;
            if stored
                != (
                    source.layer.clone(),
                    prompt.role.clone(),
                    source.sha256.clone(),
                    source.path.clone(),
                    source.parent_revision.clone(),
                )
            {
                return Err(PersistError::IntegrityViolation(
                    "stored prompt revision differs from its source chain".to_owned(),
                ));
            }
            parent = Some(&source.revision_id);
        }
        let leaf = prompt
            .chain
            .last()
            .ok_or_else(|| PersistError::IntegrityViolation("empty prompt chain".to_owned()))?;
        if (
            leaf.layer.as_str(),
            leaf.path.as_str(),
            leaf.sha256.as_str(),
        ) != (
            prompt.layer.as_str(),
            prompt.path.as_str(),
            prompt.content_sha256.as_str(),
        ) {
            return Err(PersistError::IntegrityViolation(
                "prompt leaf differs from effective contract".to_owned(),
            ));
        }
    }
    if let Some(packed) = &prompt.packed {
        let expected: Option<String> = tx
            .query_row(
                "SELECT resource_bundle_version FROM attempts WHERE attempt_id = ?1",
                [attempt_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if expected.is_some_and(|v| v != packed.resource_bundle_version) {
            return Err(PersistError::IntegrityViolation(
                "packed prompt differs from the prepared resource pin".to_owned(),
            ));
        }
    }
    tx.execute(
        "UPDATE attempts SET prompt_revision = ?2, prompt_sha256 = ?3, \
         resource_bundle_version = coalesce(?4, resource_bundle_version) WHERE attempt_id = ?1",
        params![
            attempt_id,
            revision,
            prompt.sent_sha256,
            prompt
                .packed
                .as_ref()
                .map(|p| p.resource_bundle_version.as_str())
        ],
    )
    .map_err(sqlite_error)?;
    let Some(packed) = &prompt.packed else {
        return Ok(Vec::new());
    };
    register(tx, &packed.artifacts)?;
    if let Some(skills) = packed.event.get("skills") {
        let sources: Vec<crate::skills::Source> =
            serde_json::from_value(skills.clone()).map_err(|e| {
                PersistError::IntegrityViolation(format!("invalid Skill provenance: {e}"))
            })?;
        for source in sources {
            if source.revision_id != source.identity(&prompt.role)
                || [&source.sha256, &source.registry_sha256]
                    .iter()
                    .any(|sha| !packed.artifacts.iter().any(|a| a.sha256 == **sha))
            {
                return Err(PersistError::IntegrityViolation(
                    "Skill identity or source object mismatch".to_owned(),
                ));
            }
            tx.execute("INSERT OR IGNORE INTO prompt_revisions(revision_id, layer, role, content_sha256, path, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![source.revision_id,source.layer,prompt.role,source.sha256,source.path,at]).map_err(sqlite_error)?;
            let stored: (String,String,String,String,Option<String>) = tx.query_row("SELECT layer,role,content_sha256,path,parent_revision FROM prompt_revisions WHERE revision_id = ?1",[&source.revision_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(sqlite_error)?;
            if stored
                != (
                    source.layer,
                    prompt.role.clone(),
                    source.sha256,
                    source.path,
                    None,
                )
            {
                return Err(PersistError::IntegrityViolation(
                    "stored Skill revision differs from its source".to_owned(),
                ));
            }
        }
    }
    let mut payload = packed.event.clone();
    if let Some(object) = payload.as_object_mut() {
        object.insert("attempt_id".to_owned(), json!(attempt_id));
    }
    let mut events = vec![event(
        project_id,
        run_id,
        child_id(attempt_id, "context.packed"),
        "context.packed",
        payload,
    )];
    if let Some(reports) = packed.event["resource_drift"]
        .as_array()
        .filter(|r| !r.is_empty())
    {
        events.insert(
            0,
            event(
                project_id,
                run_id,
                child_id(attempt_id, "resource.drift"),
                "resource.drift",
                json!({"attempt_id":attempt_id,"role":prompt.role,"reports":reports}),
            ),
        );
    }
    Ok(events)
}

fn finished(
    conn: &mut Connection,
    attempt_id: &str,
    ending: &Ending,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let applied = finished_in(&tx, attempt_id, ending, ctx)?;
    super::writer::commit(tx, ctx)?;
    Ok(applied)
}

/// [`finished`] inside the caller's transaction (docs/adr/ADR-028.md decision 4: a writer attempt's completion freezes,
/// accepts and settles in one commit). The caller commits.
pub(super) fn finished_in(
    tx: &Transaction<'_>,
    attempt_id: &str,
    ending: &Ending,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    finish_in(tx, attempt_id, ending, false, ctx)
}

fn finish_in(
    tx: &Transaction<'_>,
    attempt_id: &str,
    ending: &Ending,
    refused_draft_gate: bool,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let (task_id, mut task_state, mut revision, run_id, project_id) = task_of(tx, attempt_id)?;
    let (from, cancelled): (String, bool) = tx
        .query_row(
            "SELECT state, cancel_requested FROM attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let from = writer::parse::<AttemptState>(&from)?;
    // Every normal ending has a termination proof. STARTING failures still carry a class:
    // only an explicit admission refusal without a failure gets a free retry (M3L-500).
    let draft_end = super::drafting::is_draft(tx, &task_id)?.then(|| {
        crate::reconcile::draft_end(
            from == AttemptState::Running,
            &crate::reconcile::Process::Gone {
                basis: "runner termination proof".into(),
            },
        )
    });
    let retry_draft = refused_draft_gate && draft_end == Some(crate::reconcile::DraftEnd::Retry);
    if !cancelled
        && task_state == TaskState::Running
        && let Some((detail, paths)) = &ending.observed.draft_boundary
        && draft_end == Some(crate::reconcile::DraftEnd::CheckWrites)
    {
        super::drafting::blocked(tx, attempt_id, detail, Some(paths), ctx)?;
        (_, task_state, revision, _, _) = task_of(tx, attempt_id)?;
    }
    // A process that never started leaves its attempt STARTING: it can only fail. A cancel takes precedence over how
    // the work ended: CANCELLED, not counted, no retry; the run's cancel moves its task (ADR-021 decision 8e).
    // Its effects still STARTED: none was observed (the runner observes and commits the work it saw end first).
    let started = super::attempts::started_effects(tx, attempt_id)?;
    // A turn that started an effect it never reported, or an effect left STARTED at all: what it did is unknown, so
    // nothing runs it again and its task stays, by the rule a reconcile applies too (ADR-019 decision 5; ADR-022
    // decision 12 v3, M2T07R-200, -451). Only a RUNNING attempt can have started one.
    let unknown = (ending.unknown_effect || !started.is_empty())
        && from == AttemptState::Running
        && !cancelled;
    let success = ending.success && from == AttemptState::Running && !cancelled && !unknown;
    let to = if cancelled || retry_draft {
        AttemptState::Cancelled
    } else if unknown {
        AttemptState::UnknownEffect
    } else if success {
        AttemptState::Settled
    } else {
        AttemptState::Failed
    };
    attempts::move_attempt(
        tx,
        &AttemptMove {
            attempt_id: attempt_id.to_owned(),
            from,
            to,
            facts: AttemptFacts {
                ended_at: Some(crate::time::rfc3339_utc(ending.at)),
                failure_reason: (!success).then(|| ending.detail.clone()),
                workspace: ending.observed.workspace.clone(),
                resolved_model: ending.observed.resolved_model.clone(),
                usage: ending.observed.usage.clone(),
                raw_log_path: ending.observed.raw_log_path.clone(),
                ..AttemptFacts::default()
            },
        },
        ctx,
    )?;
    super::retry::accrue_wall(tx, &run_id, attempt_id, ending.at, None)?;
    // The prompt's source and what was sent (decision 11), from an executor that hands it with its end (a test's); the
    // product's is recorded when the attempt is marked RUNNING (docs/adr/ADR-026.md decision 7).
    let prompt_events = match &ending.observed.prompt {
        Some(prompt) => record_prompt(
            tx,
            &project_id,
            &run_id,
            attempt_id,
            prompt,
            &crate::time::rfc3339_utc(ending.at),
        )?,
        None => Vec::new(),
    };
    // The agent's own session id, from a turn that completed only: an id given before any input (Pi's get_state, Codex's
    // thread/start) names a session that may never have been persisted, and a failed turn must not replace a completed
    // one's (M2T17R-500). The completed turn records it as a session.native event in its own settle's transaction: that
    // record, not the session row (which older versions and direct adapters fill), is what a later turn resumes
    // (docs/adr/ADR-024.md decision 4, M2T17R-600, -601).
    let mut commit = Commit {
        events: prompt_events,
        ..Commit::default()
    };
    // The session's first completed turn pays its first input: the handover package's row ACCEPTED
    // (docs/adr/ADR-028.md decision 9).
    if to == AttemptState::Settled {
        tx.execute(
            "UPDATE handoffs SET state = 'ACCEPTED' WHERE state = 'DELIVERED' AND handoff_id IN (\
               SELECT r.message_id FROM receipts r JOIN outbox o ON o.message_id = r.message_id \
               WHERE json_extract(o.payload_json, '$.kind') = 'session.start' \
               AND json_extract(r.result_json, '$.session_id') = \
                 (SELECT session_id FROM attempts WHERE attempt_id = ?1))",
            params![attempt_id],
        )
        .map_err(sqlite_error)?;
    }
    if to == AttemptState::Settled
        && let Some(native) = &ending.observed.native_session
    {
        let session: Option<String> = tx
            .query_row(
                "SELECT session_id FROM attempts WHERE attempt_id = ?1",
                params![attempt_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if let Some(session) = session {
            tx.execute(
                "UPDATE sessions SET native_id = ?2 WHERE session_id = ?1",
                params![session, native],
            )
            .map_err(sqlite_error)?;
            writer::write_session_native(tx, &project_id, &run_id, attempt_id, &session, native)?;
        }
    }
    // A work attempt's bridge lines, when its work's OBSERVED did not keep them (an UNKNOWN_EFFECT end, a failure that
    // never reached OBSERVED): kept by its end (docs/adr/ADR-028.md decision 2, M2T16D-404).
    if !super::wakes::is_supervision(tx, &task_id)? {
        let lines: Vec<Value> = ending
            .observed
            .messages
            .iter()
            .map(|l| Value::String(l.clone()))
            .collect();
        if let Some(kept) = outputs(
            tx,
            &project_id,
            &run_id,
            attempt_id,
            ending.success,
            ending.observed.messages_truncated,
            &lines,
        )? {
            commit.events.push(kept);
        }
    }
    for (key, kind, payload) in &ending.observed.events {
        commit.events.push(NewEvent {
            intent: Intent::Progress,
            project_id: project_id.clone(),
            event_id: crate::ids::child_id(attempt_id, key),
            run_id: Some(run_id.clone()),
            kind: kind.clone(),
            payload: payload.clone(),
        });
    }
    // The effects it left STARTED are UNKNOWN, in its ending's transaction, and named for whoever settles them, as the
    // cancel's own pass and a reconcile record them (ADR-021 decision 8e; ADR-022 decision 12 v3, M2T07R-301, -451).
    if unknown || cancelled {
        super::attempts::effects_unknown(tx, attempt_id)?;
    }
    if !started.is_empty() && (unknown || cancelled) {
        commit.events.push(NewEvent {
            intent: Intent::Record,
            project_id: project_id.clone(),
            event_id: crate::ids::child_id(attempt_id, "runner/unknown-effect"),
            run_id: Some(run_id.clone()),
            kind: "runner.diagnostic".to_owned(),
            payload: serde_json::json!({
                "code": "UNKNOWN_EFFECT", "attempt_id": attempt_id, "effect_keys": started,
                "detail": ending.detail,
            }),
        });
    }
    // A verifier whose attempt ended with an effect not confirmed is never run again by itself (REQ-041 behaviour 2):
    // its task BLOCKED for a decision that confirms the effect (docs/adr/ADR-028.md decision 11, M2T16D-500).
    if unknown
        && !super::consult::is_child(tx, &task_id)?
        && let Some((cas, decision)) = super::verification::unknown_effect(
            tx,
            (&project_id, &run_id, &task_id),
            (task_state, revision),
            attempt_id,
        )?
    {
        commit.cas.push(cas);
        commit.events.push(decision);
    }
    // An unknown effect leaves its task as it is; a cancelled attempt's task is the cancel's to move.
    if success && !unknown {
        if task_state == TaskState::Running && !super::consult::is_child(tx, &task_id)? {
            commit.cas.push(StateCas {
                id: task_id,
                expected_revision: revision,
                change: Change::Task(TaskState::Implemented),
            });
        }
    } else if retry_draft && !cancelled {
        commit.cas.push(StateCas {
            id: task_id,
            expected_revision: revision,
            change: Change::Task(TaskState::Ready),
        });
    } else if !unknown && !cancelled {
        // Its task by the retry policy, in this transaction (ADR-020 decision 3).
        let failed = super::retry::fail_task(
            tx,
            &super::retry::TaskRef {
                project_id: &project_id,
                run_id: &run_id,
                task_id: &task_id,
                state: task_state,
                revision,
            },
            attempt_id,
            &super::retry::Failure {
                class: ending.class,
                at: ending.at,
                draw: ending.draw,
            },
        )?;
        commit.cas.extend(failed.cas);
        commit.events.extend(failed.events);
    }
    writer::write(tx, &commit, ctx)?;
    super::consult::finish(tx, attempt_id, ctx)?;
    super::inputs::finish(
        tx,
        attempt_id,
        success && !unknown,
        ending.observed.final_text.as_deref(),
        ctx,
    )?;
    Ok(true)
}
