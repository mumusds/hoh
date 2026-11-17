//! The Codex adapter (REQ-024, docs/13-adapters.md "Codex"). One process per attempt.
//!
//! **App Server first**: `codex app-server [--listen stdio://]` speaks JSON-RPC on stdio. The client sends
//! initialize, waits for its response, sends initialized, then thread/start (or thread/resume for a session's
//! thread), then turn/start; requests and responses are matched by id. The YOLO profile is thread/start's
//! `approvalPolicy=never`, `sandbox=danger-full-access` (CR-002). The thread id is the session's native id; the
//! thread response's `model` is the resolved model; every item/started and item/completed becomes an `adapter.item`
//! event with its thread, turn and item ids; a notification this version's schema does not name is kept as it came in
//! an `adapter.unknown_event`. Server requests (approvals, user input, ...) are answered at once by the headless
//! policy — declined or refused, never a made-up human choice — and recorded. A steer is `turn/steer` with
//! `expectedTurnId`: the input moves QUEUED -> DELIVERED on the response and -> APPLIED when its user message item
//! appears; a refusal moves it to REJECTED. turn/completed decides the end: completed -> SETTLED, interrupted ->
//! CANCELLED, failed -> FAILED (unauthorized -> BLOCKED_AUTH, no retry; rateLimitExceeded/serverOverloaded ->
//! TRANSIENT_EXTERNAL for the retry policy); an `error` notification with willRetry=true is no end. A stream that
//! ends before turn/completed is FAILED (PROTOCOL_INCOMPLETE), or UNKNOWN_EFFECT once a tool item had started. A
//! cancel sends turn/interrupt for the active thread and turn and waits the native grace for the interrupted turn,
//! then SIGTERM and SIGKILL go to the process group; the end is committed through the attempt's [`CancelLatch`].
//!
//! **exec when App Server is not there**: when `codex app-server --help` was not offered, or initialize answers
//! "method not found" (-32601) or does not answer in time, the attempt runs `codex exec --json` instead, recording one
//! `adapter.degraded` event; `mid_turn_steer` is then false and a steer is answered CAPABILITY_UNSUPPORTED with the
//! input left QUEUED. The YOLO flag is used only if `codex exec --help` offers it; otherwise nothing starts
//! (BLOCKED_CAPABILITY). exec's turn.completed (with exit code 0) SETTLES; turn.failed fails, BLOCKED_AUTH when it or
//! an earlier `error` event says `unexpected status 401` (CR-003).

use crate::adapter::{
    AdapterError, AttemptRef, Observer, Recorder, Stop, Watch, finish, now, offers, sha256_file,
    step,
};
pub use crate::adapter::{Facts, GateCommand, Held, Observe, Recorded, Refused, Replayed};
use crate::process::{Escalation, Signal, Step, Termination};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{AttemptFacts, CancelLatch, Session, Store};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub mod app_server;
mod exec;

/// exec's YOLO flag; used only when `codex exec --help` offers it.
pub const YOLO_FLAG: &str = "--dangerously-bypass-approvals-and-sandbox";

/// JSON-RPC "method not found": initialize answered this way means no usable App Server.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// How to start one turn.
#[derive(Debug, Clone)]
pub struct Launch {
    pub program: PathBuf,
    /// The probed `codex app-server --help`; `None` when codex has no App Server (exec is used).
    pub app_server_help: Option<String>,
    /// The probed `codex exec --help`.
    pub exec_help: String,
    /// The probed `codex exec resume --help`, for a resumed turn in exec mode.
    pub exec_resume_help: Option<String>,
    pub cwd: PathBuf,
    /// The launch profile (docs/adr/ADR-032.md decision 2); `Yolo` is HoH's YOLO runtime profile.
    pub profile: crate::Profile,
    /// The thread to continue (the session's native id).
    pub resume: Option<String>,
    /// The model asked for; `None` is codex's own default.
    pub model: Option<String>,
    /// Server notification methods of this version (its app-server schema, [`known_notifications`]).
    pub known: BTreeSet<String>,
    /// Client request methods of this version (its app-server schema's `ClientRequest.json`, [`listed_methods`]): the
    /// App Server is used only if they and `known` have what every turn needs (M1T07R-15).
    pub requests: BTreeSet<String>,
    /// Client notification methods of this version (its app-server schema's `ClientNotification.json`); the App
    /// Server is used only if they have what every turn sends (M1R-04).
    pub client_notifications: BTreeSet<String>,
    /// What the probe found for this codex (its compat manifest entry); the session row records them, narrowed by
    /// the transport the turn used (REQ-027).
    pub capabilities: crate::capabilities::Probed,
}

/// Why a turn was not started (`BLOCKED_CAPABILITY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked(pub String);

/// The notification methods `ServerNotification.json` of `codex app-server generate-json-schema` names.
pub fn known_notifications(server_notification_schema: &Value) -> BTreeSet<String> {
    listed_methods(server_notification_schema)
}

/// The methods an App Server schema file lists (its `oneOf` branches' `method` values): `ClientRequest.json`'s
/// requests, `ServerNotification.json`'s notifications. It does not validate the file: what decides whether to use
/// the App Server reads the schema through `probe::codex_schema` (M1T07R-31).
pub fn listed_methods(schema: &Value) -> BTreeSet<String> {
    schema["oneOf"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|variant| {
            let method = &variant["properties"]["method"];
            let listed = method["enum"].as_array().cloned().unwrap_or_default();
            listed.into_iter().chain(method.get("const").cloned())
        })
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect()
}

/// The App Server's transport: stdio, given as `--listen stdio://` where its help declares `--listen`.
pub const LISTEN_STDIO: &str = "stdio://";

/// Why this `codex app-server --help` gives no way to start the App Server on stdio, or `None`: a `--listen` whose
/// text does not list `stdio://` (without `--listen`, stdio is the App Server's only transport). The probe and
/// `codex::run` apply it before choosing the App Server (M1T07R-27).
pub fn app_server_launch_missing(help: &str) -> Option<String> {
    (offers(help, "--listen")
        && !crate::adapter::option_text(help, "--listen")
            .is_some_and(|text| crate::adapter::has_word(&text, LISTEN_STDIO)))
    .then(|| format!("codex app-server --help offers --listen without {LISTEN_STDIO}"))
}

/// The arguments of `codex app-server` on stdio, or why it cannot be started so.
pub fn app_server_args(help: &str) -> Result<Vec<String>, Blocked> {
    if let Some(why) = app_server_launch_missing(help) {
        return Err(Blocked(why));
    }
    let mut args = vec!["app-server".to_owned()];
    if offers(help, "--listen") {
        args.extend(["--listen".to_owned(), LISTEN_STDIO.to_owned()]);
    }
    Ok(args)
}

/// Whether a resumed exec turn can do whatever a new one can: `codex exec resume --help` was probed and offers
/// `--json`, and the YOLO flag and `--model` wherever `codex exec --help` offers them. It does not depend on one
/// launch's profile or model, since a later turn of the session may use others (M1T04R-11, -14).
pub fn exec_resumable(launch: &Launch) -> bool {
    let Some(help) = launch.exec_resume_help.as_deref() else {
        return false;
    };
    offers(help, "--json")
        && [YOLO_FLAG, "--model"]
            .iter()
            .all(|flag| !offers(&launch.exec_help, flag) || offers(help, flag))
}

/// What the App Server adapter sends on every turn: the handshake (`initialize`, then the `initialized`
/// notification), the thread, the turn and its interrupt (a cancel); and what it needs to hear back: the turn's end,
/// its items (the reply, a tool that started, a steer's message). An App Server whose schema lacks one cannot run a
/// turn (M1T07R-15, M1R-04); the adapter sends nothing else but [`APP_SERVER_GATED`].
pub const APP_SERVER_REQUESTS: &[&str] =
    &["initialize", "thread/start", "turn/start", "turn/interrupt"];
pub const APP_SERVER_CLIENT_NOTIFICATIONS: &[&str] = &["initialized"];
pub const APP_SERVER_NOTIFICATIONS: &[&str] = &["turn/completed", "item/started", "item/completed"];
/// Requests the adapter sends only on a turn that uses the capability, and only where the schema has them: (method,
/// capability). Without `thread/resume` a resumed turn does not use the App Server; without `turn/steer` a steer
/// waits for a later turn (M1T07R-28).
pub const RESUME_REQUEST: &str = "thread/resume";
pub const STEER_REQUEST: &str = "turn/steer";
pub const APP_SERVER_GATED: &[(&str, &str)] = &[
    (RESUME_REQUEST, "resume"),
    (STEER_REQUEST, "mid_turn_steer"),
];

/// Why an App Server with these methods (its schema's `ClientRequest.json`, `ClientNotification.json` and
/// `ServerNotification.json`) cannot run a turn, or `None`. The probe and doctor --live apply it before choosing the
/// App Server over exec (M1T07R-15, M1R-04).
pub fn app_server_interface_missing(
    requests: &BTreeSet<String>,
    client_notifications: &BTreeSet<String>,
    notifications: &BTreeSet<String>,
) -> Option<String> {
    let missing: Vec<&str> = [
        (APP_SERVER_REQUESTS, requests),
        (APP_SERVER_CLIENT_NOTIFICATIONS, client_notifications),
        (APP_SERVER_NOTIFICATIONS, notifications),
    ]
    .into_iter()
    .flat_map(|(needed, listed)| needed.iter().filter(|m| !listed.contains(**m)))
    .copied()
    .collect();
    (!missing.is_empty()).then(|| format!("the App Server schema has no {}", missing.join(", ")))
}

/// Why this `codex exec --help` gives the exec fallback no way to run a turn (no `--json`), or `None`; the probe applies
/// the same rule (M1R-03).
pub fn exec_interface_missing(help: &str) -> Option<String> {
    (!offers(help, "--json")).then(|| "codex exec --help offers no --json".to_owned())
}

/// The arguments of `codex exec --json` (or `codex exec resume --json ... <thread>`), the prompt on stdin. No read-only
/// turn runs on exec (docs/adr/ADR-032.md decision 10 (A)): its patch refusals leave no trace there.
pub fn exec_args(launch: &Launch) -> Result<Vec<String>, Blocked> {
    if launch.profile == crate::Profile::ReadOnly {
        return Err(Blocked(
            "codex exec runs no read-only turn: the read-only profile needs the App Server (ADR-032 decision 10)"
                .to_owned(),
        ));
    }
    let help = match &launch.resume {
        None => launch.exec_help.as_str(),
        Some(_) => launch.exec_resume_help.as_deref().ok_or_else(|| {
            Blocked("no probed `codex exec resume --help` to resume with".to_owned())
        })?,
    };
    if !offers(help, "--json") {
        return Err(Blocked("codex exec --help offers no --json".to_owned()));
    }
    let mut args = vec!["exec".to_owned()];
    if launch.resume.is_some() {
        args.push("resume".to_owned());
    }
    args.push("--json".to_owned());
    if launch.profile == crate::Profile::Yolo {
        if !offers(help, YOLO_FLAG) {
            return Err(Blocked(format!(
                "codex exec --help offers no {YOLO_FLAG} for the YOLO profile"
            )));
        }
        args.push(YOLO_FLAG.to_owned());
    }
    if let Some(model) = &launch.model {
        if !offers(help, "--model") {
            return Err(Blocked("codex exec --help offers no --model".to_owned()));
        }
        args.extend(["--model".to_owned(), model.clone()]);
    }
    if let Some(thread) = &launch.resume {
        args.push(thread.clone());
    }
    args.push("-".to_owned());
    Ok(args)
}

/// The headless answer to a server request: a result, or a JSON-RPC error (code, message) refusing it. Approvals are
/// declined; anything that would need a person's choice or a credential is refused.
pub fn headless_answer(method: &str) -> Result<Value, (i64, String)> {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            Ok(json!({"decision": "decline"}))
        }
        "execCommandApproval" | "applyPatchApproval" => Ok(json!({"decision": "denied"})),
        "mcpServer/elicitation/request" => Ok(json!({"action": "decline"})),
        _ => Err((
            -32000,
            format!("HoH runs codex headless: {method} needs a person and is refused"),
        )),
    }
}

/// A turn to run.
#[derive(Debug, Clone)]
pub struct Turn {
    pub launch: Launch,
    /// Who starts the agent (docs/adr/ADR-031.md, 设计修正 2): the host's direct spawner, or doctor's gate.
    pub spawner: std::sync::Arc<dyn hoh_core::spawn::Spawner>,
    pub prompt: String,
    /// Directory for the raw logs (created if needed): `app-server.stdout.jsonl`/`.stderr.log`, `exec.…`.
    pub logs: PathBuf,
    /// `native_grace` is how long an interrupted turn may take to end (App Server: turn/interrupt; exec: SIGINT);
    /// then SIGTERM and SIGKILL go to the group (docs/13: 10 s / 5 s initially).
    pub escalation: Escalation,
    /// How long initialize may take before exec is used instead.
    pub handshake_timeout: Duration,
    pub deadline: Option<Duration>,
    /// A test's look at the diagnostics a turn of the runner's mode collects; the product has none.
    pub observe: Option<Observe>,
}

impl Turn {
    /// The initial graces; the App Server's native cancel is a protocol message, not a signal.
    pub fn default_escalation() -> Escalation {
        Escalation {
            native: None,
            native_grace: Duration::from_secs(10),
            term_grace: Duration::from_secs(5),
            ..Escalation::default()
        }
    }
}

/// Which protocol ran the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    AppServer,
    Exec,
}

/// A steer for the running turn: input `input_id` (QUEUED in the store) with `text`, aimed at `expected_turn` (the
/// active turn if `None`). The receipt says what happened to it.
#[derive(Debug)]
pub struct Steer {
    pub input_id: String,
    pub text: String,
    pub expected_turn: Option<String>,
    pub reply: oneshot::Sender<SteerReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteerReceipt {
    /// codex took it for this turn (the input is DELIVERED, APPLIED once its item appears).
    Delivered { turn_id: String },
    /// codex refused it (the input is REJECTED).
    Rejected { reason: String },
    /// exec cannot take input mid-turn; the input stays QUEUED for a later turn.
    CapabilityUnsupported,
    /// No turn could take it now; the input stays QUEUED.
    NotNow,
}

/// What the end of a turn is judged on (see [`verdict`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Ending<'a> {
    pub cancelled: bool,
    pub too_large: bool,
    pub deadline: bool,
    pub logs_incomplete: bool,
    /// A protocol failure that ended the turn early (a refused thread/start, ...).
    pub failure: Option<&'a str>,
    /// turn.status (App Server) or `completed`/`failed` (exec's turn.completed/turn.failed).
    pub status: Option<&'a str>,
    /// turn.error (App Server) or turn.failed's error (exec).
    pub error: Option<&'a Value>,
    /// exec said `unexpected status 401` before failing.
    pub auth_hint: bool,
    /// An item that may have changed something (a command, a file change, a tool call) had started.
    pub tool_started: bool,
    /// The exit code counts (exec: a one-shot process).
    pub exit_matters: bool,
    pub exit: Option<Termination>,
}

/// The failure reason of a failed turn's error.
pub fn failure_of(error: Option<&Value>, auth_hint: bool) -> String {
    let info = error.and_then(|e| e["codexErrorInfo"].as_str());
    let message = error
        .and_then(|e| e["message"].as_str())
        .unwrap_or_default();
    match info {
        Some("unauthorized") => "BLOCKED_AUTH".to_owned(),
        Some("rateLimitExceeded" | "serverOverloaded") => "TRANSIENT_EXTERNAL".to_owned(),
        _ if auth_hint || message.contains("unexpected status 401") => "BLOCKED_AUTH".to_owned(),
        // Codex's own text: never one of HoH's reason codes (docs/adr/ADR-030.md decision 6).
        Some(other) => crate::adapter::native_code(other),
        None => "turn failed".to_owned(),
    }
}

/// The end state and failure reason of a turn (pure; module docs).
pub fn verdict(e: &Ending<'_>) -> (AttemptState, Option<String>) {
    let failed = |why: &str| (AttemptState::Failed, Some(why.to_owned()));
    if e.cancelled {
        return (AttemptState::Cancelled, None);
    }
    if e.too_large {
        return failed("PROTOCOL_FRAME_TOO_LARGE");
    }
    if e.deadline {
        return failed("DEADLINE_EXCEEDED");
    }
    if e.logs_incomplete {
        return failed("RAW_LOG_INCOMPLETE");
    }
    if let Some(why) = e.failure {
        return failed(why);
    }
    match e.status {
        Some("completed") => match (e.exit_matters, e.exit) {
            (false, _) | (true, Some(Termination::Code(0))) => (AttemptState::Settled, None),
            (true, Some(Termination::Code(code))) => failed(&format!("exit code {code}")),
            (true, Some(Termination::Signal(signal))) => failed(&format!("signal {signal}")),
            (true, None) => failed("no exit status"),
        },
        Some("interrupted") => (AttemptState::Cancelled, None),
        Some("failed") => failed(&failure_of(e.error, e.auth_hint)),
        _ if e.tool_started => (
            AttemptState::UnknownEffect,
            Some("PROTOCOL_INCOMPLETE".to_owned()),
        ),
        _ => failed("PROTOCOL_INCOMPLETE"),
    }
}

/// Item types that only carry text: their start changes nothing outside the conversation (the read-only judge reads
/// the same list, docs/adr/ADR-032.md decision 8 row C).
pub fn is_text_item(kind: &str) -> bool {
    matches!(
        kind,
        "userMessage" | "agentMessage" | "reasoning" | "plan" | "agent_message" | "error"
    )
}

/// What a turn did.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub state: AttemptState,
    pub failure: Option<String>,
    pub mode: Mode,
    /// The arguments of the process that ran the turn.
    pub args: Vec<String>,
    /// The thread id.
    pub native_session: Option<String>,
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
    /// The text of the last completed agent message.
    pub reply: Option<String>,
    pub exit: Option<Termination>,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
    pub items: usize,
    pub unknown_events: usize,
    /// Why exec ran instead of the App Server.
    pub degraded: Option<String>,
    pub pid: Option<u32>,
    pub steps: Vec<Step>,
    /// The most stdout bytes the frame reader held at once (AC-027-03's probe; at most the frame limit and one read
    /// chunk); `None` when no process was read.
    pub max_buffered: Option<usize>,
}

/// Run one turn as `attempt` (created PREPARED by the caller); a cancel accepted by `cancel` cancels it, and `steers`
/// carries input for the running turn.
pub async fn run(
    store: &Store,
    attempt: &AttemptRef,
    turn: &Turn,
    cancel: &CancelLatch,
    mut steers: mpsc::Receiver<Steer>,
) -> Result<Outcome, AdapterError> {
    use AttemptState::*;
    let blocked = |detail: String| async move {
        let state = finish(
            store,
            attempt,
            Prepared,
            Failed,
            AttemptFacts {
                ended_at: now(),
                failure_reason: Some("BLOCKED_CAPABILITY".to_owned()),
                ..AttemptFacts::default()
            },
            cancel,
        )
        .await?;
        store
            .attempt_event(
                &attempt.attempt_id,
                "blocked",
                "adapter.blocked",
                json!({"attempt_id": attempt.attempt_id, "reason": "BLOCKED_CAPABILITY", "detail": detail}),
            )
            .await?;
        Ok::<Outcome, AdapterError>(Outcome {
            state,
            failure: (state == Failed).then(|| "BLOCKED_CAPABILITY".to_owned()),
            mode: Mode::Exec,
            args: Vec::new(),
            native_session: None,
            turn_id: None,
            model: None,
            status: None,
            reply: None,
            exit: None,
            stdout: turn.logs.join("exec.stdout.jsonl"),
            stderr: turn.logs.join("exec.stderr.log"),
            stdout_sha256: String::new(),
            stderr_sha256: String::new(),
            items: 0,
            unknown_events: 0,
            degraded: None,
            pid: None,
            steps: Vec::new(),
            max_buffered: None,
        })
    };
    // The App Server only if it starts on stdio and its schema has what this turn uses (the probe's choice too, but for
    // the resume this turn may need; M1T07R-15, -27, -28); without one, exec must be possible before anything starts.
    let unusable = app_server_unusable(turn);
    let app_server_help = turn
        .launch
        .app_server_help
        .as_deref()
        .filter(|_| unusable.is_none());
    if app_server_help.is_none()
        && let Err(Blocked(detail)) = exec_args(&turn.launch)
    {
        return blocked(detail).await;
    }
    if let Err(e) = std::fs::create_dir_all(&turn.logs) {
        finish(
            store,
            attempt,
            Prepared,
            Failed,
            AttemptFacts {
                ended_at: now(),
                failure_reason: Some(format!("LOGS: {e}")),
                ..AttemptFacts::default()
            },
            cancel,
        )
        .await?;
        return Err(e.into());
    }
    let app_server = app_server_help.is_some();
    store
        .upsert_session(session(
            attempt,
            turn,
            app_server,
            turn.launch.resume.clone(),
        ))
        .await?;
    step(
        store,
        attempt,
        Prepared,
        Starting,
        AttemptFacts {
            session_id: Some(attempt.session_id.clone()),
            started_at: now(),
            raw_log_path: Some(turn.logs.to_string_lossy().into_owned()),
            ..AttemptFacts::default()
        },
    )
    .await?;
    let mut held = Vec::new();
    let reason = match app_server_help {
        Some(help) => {
            match app_server::run(store, attempt, turn, help, cancel, &mut steers, &mut held)
                .await?
            {
                app_server::End::Ended(outcome) => return Ok(*outcome),
                app_server::End::Degrade(reason) => reason,
            }
        }
        None => unusable.unwrap_or_else(|| "codex --help offers no app-server".to_owned()),
    };
    store
        .attempt_event(
            &attempt.attempt_id,
            "degraded",
            "adapter.degraded",
            json!({"attempt_id": attempt.attempt_id, "reason": reason, "mode": "exec"}),
        )
        .await?;
    store
        .upsert_session(session(attempt, turn, false, turn.launch.resume.clone()))
        .await?;
    for steer in held {
        let _ = steer.reply.send(SteerReceipt::CapabilityUnsupported);
    }
    let args = match exec_args(&turn.launch) {
        Ok(args) => args,
        Err(Blocked(detail)) => {
            let state = finish(
                store,
                attempt,
                Starting,
                Failed,
                AttemptFacts {
                    ended_at: now(),
                    failure_reason: Some("BLOCKED_CAPABILITY".to_owned()),
                    ..AttemptFacts::default()
                },
                cancel,
            )
            .await?;
            store
                .attempt_event(
                    &attempt.attempt_id,
                    "blocked",
                    "adapter.blocked",
                    json!({"attempt_id": attempt.attempt_id, "reason": "BLOCKED_CAPABILITY", "detail": detail}),
                )
                .await?;
            return Ok(Outcome {
                state,
                failure: (state == Failed).then(|| "BLOCKED_CAPABILITY".to_owned()),
                mode: Mode::Exec,
                args: Vec::new(),
                native_session: turn.launch.resume.clone(),
                turn_id: None,
                model: None,
                status: None,
                reply: None,
                exit: None,
                stdout: turn.logs.join("exec.stdout.jsonl"),
                stderr: turn.logs.join("exec.stderr.log"),
                stdout_sha256: String::new(),
                stderr_sha256: String::new(),
                items: 0,
                unknown_events: 0,
                degraded: Some(reason),
                pid: None,
                steps: Vec::new(),
                max_buffered: None,
            });
        }
    };
    exec::run(store, attempt, turn, args, reason, cancel, &mut steers).await
}

/// Replay `bytes` (the App Server's stdout, with the host's cancel where [`crate::adapter::REPLAY_CANCEL`] stands)
/// through the App Server adapter's own judging, no process started (docs/adr/ADR-030.md decision 10): see
/// [`Replayed`]. Every codex version is judged alike: its notifications are those of the frame registry built in
/// ([`crate::known::codex_app_server`]); `_version` is the tested one's.
pub fn replay(bytes: &[u8], _version: &str) -> Replayed {
    app_server::replay(bytes, &replay_turn())
}

/// The prompt a replay's turn/start carries: the recorded one is not an input of the replay, and the request log's
/// prompt text is not compared (docs/adr/ADR-032.md design correction 7 item 2, the comparison contract).
pub const REPLAY_PROMPT: &str = "<the prompt>";

/// Replay a recorded App Server turn (codex's stdout as it was read) through the App Server adapter's own judging with
/// the observations recorded with it, no process started (docs/adr/ADR-032.md design correction 7): the launch they
/// record (profile, resumed thread, model) is the only source of what the adapter sends, the recorded exit status and
/// log completion end it. Observations the contract does not accept give NOT_OBSERVED ([`crate::observed::parse`]).
/// Every codex version is judged alike (the frame registry built in).
pub fn replay_facts(stdout: &[u8], observations: &Value) -> crate::observed::ReplayFacts {
    use crate::observed::{Agent, ReplayFacts};
    let observed = match crate::observed::parse(Agent::Codex, observations) {
        Ok(observed) => observed,
        Err(why) => return ReplayFacts::failed(why),
    };
    let Some(launch) = observed.codex.clone() else {
        return ReplayFacts::failed(format!("{} (launch)", crate::observed::NOT_OBSERVED));
    };
    let mut turn = replay_turn();
    turn.launch.profile = launch.profile;
    turn.launch.resume = launch.resume;
    turn.launch.model = launch.model;
    turn.prompt = REPLAY_PROMPT.to_owned();
    app_server::replay_facts(stdout, &turn, &observed)
}

/// Replay `bytes` (`codex exec --json`'s stdout, with the host's cancel where [`crate::adapter::REPLAY_CANCEL`] stands)
/// through the exec transport's own judging, no process started (docs/adr/ADR-030.md decision 10): see [`Replayed`].
/// Every codex version is judged alike (the exec registry built in, [`crate::known::codex_exec`]); `_version` is the
/// tested one's.
pub fn replay_exec(bytes: &[u8], _version: &str) -> Replayed {
    exec::replay(bytes)
}

/// A codex turn of the runner's mode driven on `process` (its gate open) with `watch`, its diagnostics shown to
/// `observer`, for the adapters' unit tests: over the App Server (`exec` false) or exec; its verdict and what it
/// recorded.
#[cfg(test)]
pub(crate) async fn drive_watched(
    exec: bool,
    process: &mut crate::process::Process,
    watch: &mut Watch,
    observer: Observer,
) -> Result<(AttemptState, Option<String>, Vec<Recorded>), AdapterError> {
    let attempt = AttemptRef {
        attempt_id: "attempt".to_owned(),
        session_id: "session".to_owned(),
        role: "developer".to_owned(),
        agent_version: String::new(),
    };
    let turn = replay_turn();
    let native = tokio::sync::Notify::new();
    let (_, mut steers) = mpsc::channel::<Steer>(1);
    if exec {
        let mut recorder = Recorder::Collect(Vec::new(), observer);
        let driven = exec::drive(
            &mut recorder,
            &attempt,
            &turn,
            process,
            b"prompt".to_vec(),
            Stop::Native(&native),
            &mut steers,
            watch,
        )
        .await?;
        let recorded = match recorder {
            Recorder::Collect(recorded, _) => recorded,
            Recorder::Store { .. } => Vec::new(),
        };
        return Ok((driven.state, driven.failure, recorded));
    }
    match app_server::drive(
        None,
        &attempt,
        &turn,
        process,
        Stop::Native(&native),
        &mut steers,
        &mut Vec::new(),
        None,
        watch,
        observer,
    )
    .await?
    {
        app_server::Drove::Ended(driven) => Ok((driven.state, driven.failure, driven.recorded)),
        app_server::Drove::Degrade(degraded) => Ok((
            AttemptState::Failed,
            Some(format!("degraded: {}", degraded.reason)),
            degraded.recorded,
        )),
    }
}

/// A codex App Server turn of the runner's mode driven on `process` (its gate `gate` open) with `watch` and the
/// handshake timeout `handshake`, its diagnostics shown to `observer` and its native cancel `native`, for the adapters'
/// unit tests: its facts as [`start`] gives them.
#[cfg(test)]
pub(crate) async fn app_server_facts_watched(
    process: &mut crate::process::Process,
    watch: &mut Watch,
    observer: Observer,
    gate: &GateCommand,
    handshake: Duration,
    native: &tokio::sync::Notify,
) -> Facts {
    let attempt = AttemptRef {
        attempt_id: "attempt".to_owned(),
        session_id: "session".to_owned(),
        role: "developer".to_owned(),
        agent_version: String::new(),
    };
    let turn = Turn {
        handshake_timeout: handshake,
        ..replay_turn()
    };
    let (_, mut steers) = mpsc::channel::<Steer>(1);
    let drove = app_server::drive(
        None,
        &attempt,
        &turn,
        process,
        Stop::Native(native),
        &mut steers,
        &mut Vec::new(),
        Some(gate.go()),
        watch,
        observer.clone(),
    )
    .await;
    app_server_facts(
        drove,
        &attempt.attempt_id,
        gate,
        "/logs".to_owned(),
        &observer,
    )
}

/// The turn a replay judges: the product's YOLO launch of a new thread with codex's own model and the suite's prompt;
/// the App Server serves every request the adapter sends.
fn replay_turn() -> Turn {
    let known = crate::known::codex_app_server().keys().cloned().collect();
    let requests = APP_SERVER_REQUESTS
        .iter()
        .chain(APP_SERVER_GATED.iter().map(|(method, _)| method))
        .map(|m| (*m).to_owned())
        .collect();
    let client_notifications = APP_SERVER_CLIENT_NOTIFICATIONS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    Turn {
        launch: Launch {
            program: PathBuf::from("codex"),
            app_server_help: None,
            exec_help: String::new(),
            exec_resume_help: None,
            cwd: PathBuf::from(crate::compat::SUITE_CWD),
            profile: crate::Profile::Yolo,
            resume: None,
            model: None,
            known,
            requests,
            client_notifications,
            capabilities: crate::capabilities::Probed::new(),
        },
        prompt: crate::compat::SUITE_PROMPT.to_owned(),
        logs: PathBuf::from(crate::compat::SUITE_CWD),
        escalation: Turn::default_escalation(),
        handshake_timeout: Duration::from_secs(60),
        deadline: None,
        observe: None,
        spawner: hoh_core::spawn::Direct::shared(),
    }
}

/// The capabilities a session of this turn's launch has (docs/adr/ADR-022.md decision 9): those of the transport
/// [`start`] would take (the App Server when it serves the turn, else exec), as [`run`] records them; or why no turn can
/// start.
pub fn session_capabilities(turn: &Turn) -> Result<Value, Blocked> {
    let app_server = app_server_unusable(turn).is_none()
        && turn
            .launch
            .app_server_help
            .as_deref()
            .is_some_and(|help| app_server_args(help).is_ok());
    if !app_server {
        exec_args(&turn.launch)?;
    }
    let attempt = AttemptRef {
        attempt_id: String::new(),
        session_id: String::new(),
        role: String::new(),
        agent_version: String::new(),
    };
    Ok(session(&attempt, turn, app_server, None).capabilities)
}

/// Why the App Server cannot serve `turn` though its help is there, or `None`.
fn app_server_unusable(turn: &Turn) -> Option<String> {
    turn.launch.app_server_help.as_deref().and_then(|help| {
        app_server_launch_missing(help)
            .or_else(|| {
                app_server_interface_missing(
                    &turn.launch.requests,
                    &turn.launch.client_notifications,
                    &turn.launch.known,
                )
            })
            .or_else(|| {
                (turn.launch.resume.is_some() && !turn.launch.requests.contains(RESUME_REQUEST))
                    .then(|| {
                        format!("the App Server schema has no {RESUME_REQUEST} to resume with")
                    })
            })
    })
}

/// Start one turn in the runner's mode (docs/adr/ADR-022.md decisions 12 to 15). Its transport is fixed before
/// anything starts: the App Server when its help and schema serve this turn, else exec (the runner's registry passes
/// no App Server help once one failed). It runs through `gate`, held until [`Held::open`] is sent; it moves no attempt
/// state and writes nothing to the store, its facts come back. A cancel is native only: turn/interrupt on the App
/// Server (stdin closed before a turn is active), SIGINT on exec; the host's ladder does the rest. No deadline. An App
/// Server whose initialize is refused as unknown or not answered ends the turn FAILED (CAPABILITY_UNSUPPORTED), the
/// reason in [`Facts::degraded`]: no second process is started within the attempt.
pub fn start(attempt: &AttemptRef, turn: &Turn, gate: &GateCommand) -> Result<Held, Refused> {
    let blocked = |detail: String| Refused {
        code: "BLOCKED_CAPABILITY".to_owned(),
        detail,
    };
    let unusable = app_server_unusable(turn);
    let app_server = match turn
        .launch
        .app_server_help
        .as_deref()
        .filter(|_| unusable.is_none())
    {
        Some(help) => app_server_args(help).ok(),
        None => None,
    };
    let (args, config) = match &app_server {
        Some(args) => (args.clone(), app_server::config(turn)),
        None => (
            exec_args(&turn.launch).map_err(|Blocked(detail)| blocked(detail))?,
            crate::process::Config {
                deadline: None,
                ..exec::config(turn)
            },
        ),
    };
    std::fs::create_dir_all(&turn.logs).map_err(|e| Refused {
        code: "LOGS".to_owned(),
        detail: e.to_string(),
    })?;
    let mut command = std::process::Command::new(&gate.program);
    command
        .args(gate.argv())
        .arg(&turn.launch.program)
        .args(&args)
        .envs(gate.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&turn.launch.cwd);
    let mut process =
        crate::process::Process::spawn(command, config, &*turn.spawner).map_err(|e| Refused {
            code: "SPAWN".to_owned(),
            detail: e.to_string(),
        })?;
    let pid = process.pid();
    let (open, opened) = oneshot::channel::<()>();
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let native = cancel.clone();
    let (attempt, turn) = (attempt.clone(), turn.clone());
    let raw_log_path = turn.logs.to_string_lossy().into_owned();
    let observer = Observer::new(&attempt.attempt_id, turn.observe.clone());
    let gate = gate.clone();
    let ended = tokio::spawn(async move {
        if opened.await.is_err() {
            let report = crate::adapter::close_gate(&mut process).await;
            return Facts::gate_closed(raw_log_path, report.as_ref());
        }
        // No steering in the runner's mode yet: the channel is closed.
        let (_, mut steers) = mpsc::channel::<Steer>(1);
        // The agent's files are re-checked while the turn runs, from the gate's opening, and on the App Server before
        // its first input (docs/adr/ADR-030.md decision 9); exec's prompt goes with the go line.
        let mut watch = Watch::new(gate.expect.clone());
        if app_server.is_some() {
            let mut held = Vec::new();
            let drove = app_server::drive(
                None,
                &attempt,
                &turn,
                &mut process,
                Stop::Native(&native),
                &mut steers,
                &mut held,
                Some(gate.go()),
                &mut watch,
                observer.clone(),
            )
            .await;
            return app_server_facts(drove, &attempt.attempt_id, &gate, raw_log_path, &observer);
        }
        let mut input = gate.go();
        input.extend_from_slice(turn.prompt.as_bytes());
        let mut recorder = Recorder::Collect(Vec::new(), observer);
        let driven = exec::drive(
            &mut recorder,
            &attempt,
            &turn,
            &mut process,
            input,
            Stop::Native(&native),
            &mut steers,
            &mut watch,
        )
        .await;
        let (recorded, observer) = match recorder {
            Recorder::Collect(recorded, observer) => (recorded, observer),
            Recorder::Store { .. } => (Vec::new(), Observer::default()),
        };
        match driven {
            // The agent ended without a frame: its gate may have refused it (docs/adr/ADR-030.md decision 9).
            Ok(driven)
                if driven.state != AttemptState::Cancelled
                    && let Some(refused) = crate::adapter::refused_at_gate(
                        &gate,
                        &raw_log_path,
                        driven.report.as_ref(),
                        &recorded,
                    ) =>
            {
                observer.refused(refused)
            }
            Ok(driven) => Facts {
                state: driven.state,
                failure: driven.failure,
                native_session: driven.thread_id,
                resolved_model: None,
                usage: driven.usage,
                raw_log_path,
                max_buffered: driven.report.as_ref().map(|r| r.stdout.max_buffered),
                group_reaped: driven.report.as_ref().is_some_and(|r| r.group_reaped),
                recorded,
                degraded: None,
                final_text: driven.reply,
                compat: None,
                retry_at: None,
            },
            Err(_) => Facts::incomplete(raw_log_path, recorded),
        }
    });
    Ok(Held {
        pid,
        open,
        cancel,
        ended,
    })
}

/// The facts of an App Server turn of the runner's mode (`attempt_id`'s, its gate `gate`) from how its drive ended.
fn app_server_facts(
    drove: Result<app_server::Drove, AdapterError>,
    attempt_id: &str,
    gate: &GateCommand,
    raw_log_path: String,
    observer: &Observer,
) -> Facts {
    match drove {
        // The agent ended without a frame: its gate may have refused it (docs/adr/ADR-030.md decision 9).
        Ok(app_server::Drove::Ended(driven))
            if driven.state != AttemptState::Cancelled
                && let Some(refused) = crate::adapter::refused_at_gate(
                    gate,
                    &raw_log_path,
                    driven.report.as_ref(),
                    &driven.recorded,
                ) =>
        {
            observer.refused(refused)
        }
        Ok(app_server::Drove::Ended(driven)) => Facts {
            state: driven.state,
            failure: driven.failure,
            native_session: driven.thread_id,
            resolved_model: driven.model,
            usage: driven.usage,
            raw_log_path,
            max_buffered: driven.report.as_ref().map(|r| r.stdout.max_buffered),
            group_reaped: driven.report.as_ref().is_some_and(|r| r.group_reaped),
            recorded: driven.recorded,
            degraded: None,
            final_text: driven.reply,
            compat: None,
            retry_at: driven.retry_at,
        }
        .before_input(driven.changed.as_ref()),
        // Moved to exec before the agent wrote a frame: its gate, slower than the handshake timeout, may have refused it
        // (M2T15R-02); its keyed refusal decides.
        Ok(app_server::Drove::Degrade(degraded))
            if let Some(refused) = crate::adapter::refused_at_gate(
                gate,
                &raw_log_path,
                degraded.report.as_ref(),
                &degraded.recorded,
            ) =>
        {
            observer.refused(refused)
        }
        // What the turn recorded until it moved to exec is kept with its degraded record (an in-turn re-check's
        // diagnostic among it: docs/adr/ADR-030.md decision 9, M2T15R-03).
        Ok(app_server::Drove::Degrade(degraded)) => {
            let mut recorded = degraded.recorded;
            recorded.push(Recorded::Event {
                key: "degraded".to_owned(),
                kind: "adapter.degraded".to_owned(),
                payload: json!({"attempt_id": attempt_id, "reason": degraded.reason, "mode": "exec",
                                "next_attempt": true}),
            });
            Facts {
                failure: Some("CAPABILITY_UNSUPPORTED".to_owned()),
                recorded,
                degraded: Some(degraded.reason),
                ..Facts::gate_closed(raw_log_path, degraded.report.as_ref())
            }
        }
        Err(_) => Facts::incomplete(raw_log_path, Vec::new()),
    }
}

/// The session row of a turn: the probed capabilities, only those this adapter implements on the transport the turn
/// used (M1R-01, M1T04R-15). On App Server: events, resume (`thread/resume`), steer, deltas and token usage; on exec:
/// events, token usage (`turn.completed`), and resume if a resumed exec turn can do whatever a new one can
/// (M1T04R-11, -14, -17); on both, commands under the YOLO profile (docs/adr/ADR-030.md decision 12); the read-only
/// workspace profile on the App Server only (ADR-032 decision 10). The row says which profiles the session can start
/// in, whatever this turn's (ADR-032 decision 4). Server requests are answered headless (no dialogs, no custom tools),
/// prompts are text only, there is no browser.
fn session(
    attempt: &AttemptRef,
    turn: &Turn,
    app_server: bool,
    native_id: Option<String>,
) -> Session {
    let supported: Vec<(&str, bool)> = if app_server {
        vec![
            ("structured_events", true),
            ("resume", turn.launch.requests.contains(RESUME_REQUEST)),
            (
                "mid_turn_steer",
                turn.launch.requests.contains(STEER_REQUEST),
            ),
            ("partial_messages", true),
            ("token_usage", true),
            (
                "command_execution",
                turn.launch.profile != crate::Profile::Plain,
            ),
            ("read_only_workspace", true),
        ]
    } else {
        vec![
            ("structured_events", true),
            ("token_usage", true),
            (
                "resume",
                crate::adapter::lists_command(&turn.launch.exec_help, "resume")
                    && exec_resumable(&turn.launch),
            ),
            (
                "command_execution",
                turn.launch.profile != crate::Profile::Plain,
            ),
            ("read_only_workspace", false),
        ]
    };
    Session {
        session_id: attempt.session_id.clone(),
        agent: "codex".to_owned(),
        role: attempt.role.clone(),
        native_id,
        native_path: None,
        agent_version: attempt.agent_version.clone(),
        capabilities: crate::capabilities::of_session(&turn.launch.capabilities, &supported),
    }
}

/// The escalation once the native cancel had its grace: SIGTERM, then SIGKILL, to the group.
fn hard_stop(escalation: Escalation) -> Escalation {
    Escalation {
        native: None,
        native_grace: Duration::ZERO,
        ..escalation
    }
}

/// The exec escalation: SIGINT first (codex's own interrupt), with the turn's graces.
fn interrupt_first(escalation: Escalation) -> Escalation {
    Escalation {
        native: Some(Signal::Interrupt),
        ..escalation
    }
}

/// The raw log hashes of an ended turn.
fn hashes(stdout: &std::path::Path, stderr: &std::path::Path) -> std::io::Result<(String, String)> {
    Ok((sha256_file(stdout)?, sha256_file(stderr)?))
}

#[cfg(test)]
mod tests;
