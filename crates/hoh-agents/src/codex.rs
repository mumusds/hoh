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

use crate::adapter::{AdapterError, AttemptRef, finish, now, offers, sha256_file, step};
use crate::process::{Escalation, Signal, Step, Termination};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{AttemptFacts, CancelLatch, Session, Store};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

mod app_server;
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
    /// HoH's YOLO runtime profile.
    pub yolo: bool,
    /// The thread to continue (the session's native id).
    pub resume: Option<String>,
    /// The model asked for; `None` is codex's own default.
    pub model: Option<String>,
    /// Server notification methods of this version (its app-server schema, [`known_notifications`]).
    pub known: BTreeSet<String>,
    /// What the probe found for this codex (its compat manifest entry); the session row records them, narrowed by
    /// the transport the turn used (REQ-027).
    pub capabilities: crate::capabilities::Probed,
}

/// Why a turn was not started (`BLOCKED_CAPABILITY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked(pub String);

/// The notification methods `ServerNotification.json` of `codex app-server generate-json-schema` names.
pub fn known_notifications(server_notification_schema: &Value) -> BTreeSet<String> {
    server_notification_schema["oneOf"]
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

/// The arguments of `codex app-server` on stdio.
pub fn app_server_args(help: &str) -> Vec<String> {
    let mut args = vec!["app-server".to_owned()];
    if offers(help, "--listen") {
        args.extend(["--listen".to_owned(), "stdio://".to_owned()]);
    }
    args
}

/// The arguments of `codex exec --json` (or `codex exec resume --json ... <thread>`), the prompt on stdin.
pub fn exec_args(launch: &Launch) -> Result<Vec<String>, Blocked> {
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
    if launch.yolo {
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
    pub prompt: String,
    /// Directory for the raw logs (created if needed): `app-server.stdout.jsonl`/`.stderr.log`, `exec.…`.
    pub logs: PathBuf,
    /// `native_grace` is how long an interrupted turn may take to end (App Server: turn/interrupt; exec: SIGINT);
    /// then SIGTERM and SIGKILL go to the group (docs/13: 10 s / 5 s initially).
    pub escalation: Escalation,
    /// How long initialize may take before exec is used instead.
    pub handshake_timeout: Duration,
    pub deadline: Option<Duration>,
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
        Some(other) => other.to_owned(),
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

/// Item types that only carry text: their start changes nothing outside the conversation.
fn is_text_item(kind: &str) -> bool {
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
        })
    };
    // Without an App Server, exec must be possible before anything starts.
    if turn.launch.app_server_help.is_none()
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
    let app_server = turn.launch.app_server_help.is_some();
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
    let reason = match &turn.launch.app_server_help {
        Some(help) => {
            match app_server::run(store, attempt, turn, help, cancel, &mut steers, &mut held)
                .await?
            {
                app_server::End::Ended(outcome) => return Ok(*outcome),
                app_server::End::Degrade(reason) => reason,
            }
        }
        None => "codex --help offers no app-server".to_owned(),
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
            });
        }
    };
    exec::run(store, attempt, turn, args, reason, cancel, &mut steers).await
}

/// The session row of a turn: the probed capabilities (M1R-01). Server requests are answered headless, so there are
/// no dialogs; on exec there is no steer and no delta, and resume only if a resumed exec turn of this launch could
/// start, by the same rule as that turn (M1T04R-11).
fn session(
    attempt: &AttemptRef,
    turn: &Turn,
    app_server: bool,
    native_id: Option<String>,
) -> Session {
    let mut limits = vec![("ui_dialogs", false)];
    if !app_server {
        let resumed = Launch {
            resume: Some(String::new()),
            ..turn.launch.clone()
        };
        limits.extend([
            ("mid_turn_steer", false),
            ("partial_messages", false),
            (
                "resume",
                offers(&turn.launch.exec_help, "resume") && exec_args(&resumed).is_ok(),
            ),
        ]);
    }
    Session {
        session_id: attempt.session_id.clone(),
        agent: "codex".to_owned(),
        role: attempt.role.clone(),
        native_id,
        native_path: None,
        agent_version: attempt.agent_version.clone(),
        capabilities: crate::capabilities::of_session(&turn.launch.capabilities, &limits),
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
