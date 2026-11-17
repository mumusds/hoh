//! The Pi adapter (REQ-026, docs/13-adapters.md "Pi"): `pi --mode rpc` on a persistent session, one process per
//! attempt.
//!
//! The session is HoH's: `--session-dir` (under `.hoh/`, fixed per session) and `--session-id` are the same on every
//! turn of a session, and `--no-session` is never used; the directory is the session's native path. Which file is the
//! session is pi's own decision, not predicted by HoH: before a resumed turn starts, the directory's `*.jsonl` files
//! are noted ([`session_files`]; none: SESSION_NOT_FOUND, nothing starts), and pi must then report, in `get_state`,
//! this session id and one of exactly those files (same device and inode) — a file it made anew, one that vanished, or
//! one not written yet means it did not resume the session (SESSION_MISMATCH, no prompt sent; M1T05R-03/06/10/11). A
//! `*.jsonl` entry that is not a regular file would block pi's own lookup, so nothing starts in such a directory
//! (SESSION_DIR_UNSAFE; M1T05R-08/12). stdout is framed by the process substrate on LF bytes only (one CR before it
//! dropped, U+2028/U+2029 kept inside frames, 1 MiB per frame) and read from the start, stderr is drained alongside;
//! every record goes to the turn's subscriber, if any.
//!
//! The turn: `get_state` (to check the session), then `prompt` with its own id. Its response only says the prompt was
//! taken: `success=false` fails the turn at once (`No API key found` -> BLOCKED_AUTH, `Model not found` ->
//! MODEL_UNAVAILABLE, else PERMANENT_EXTERNAL; docs/13) and REJECTS its input; disposition `handled` ends the turn at
//! once (no run started); `started`/`queued` waits for `agent_settled` — `agent_end` is not the end, retries and
//! compaction may follow. A version without `agent_settled` checks with `get_state` after each `agent_end` (idle: not
//! streaming, not compacting, nothing pending). No timer ever decides that Pi is idle: the turn's own timers (the
//! deadline, the abort grace) run on its [`Clock`]. Dialog UI requests (select/confirm/input/editor) are answered
//! `cancelled` at once and recorded; other UI requests are only data. A cancel (or the deadline) sends `abort`; Pi's
//! `agent_settled` ends the turn, and an abort not answered within the native grace is followed by SIGTERM and SIGKILL
//! to the process group. A stream that ends before Pi settled is FAILED (PROTOCOL_INCOMPLETE), or UNKNOWN_EFFECT once
//! a tool had started. Every end is committed through the attempt's cancel latch.

use crate::adapter::{
    AdapterError, AttemptRef, Changed, Expect, Observer, Replay, Replayed, Sink, Stop, Watch,
    finish, now, offers, sha256_file, step,
};
pub use crate::adapter::{Facts, GateCommand, Held, Observe, Recorded, Refused};
use crate::clock::{Clock, Timer};
use crate::frame::Frame;
use crate::known;
use crate::process::{Config, Escalation, Event, Process, Step, Termination};
use hoh_core::lifecycle::{AttemptState, InputState};
use hoh_core::persistence::{AttemptFacts, CancelLatch, PersistError, Session, Store};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// How to start one turn.
#[derive(Debug, Clone)]
pub struct Launch {
    pub program: PathBuf,
    /// The probed `pi --help`.
    pub help: String,
    pub cwd: PathBuf,
    /// The session's persistent directory (its native path).
    pub session_dir: PathBuf,
    /// The session's id, chosen by HoH and kept for every turn of it.
    pub session_id: String,
    /// The session existed before: its file must be in `session_dir`.
    pub resume: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// This version emits `agent_settled` (pi 1.5.0 does); otherwise `get_state` checks for idle.
    pub settled_event: bool,
    /// The launch profile (docs/adr/ADR-032.md decision 2); pi has no bypass, so `Yolo` and `Plain` start alike.
    pub profile: crate::Profile,
    /// What the probe found for this pi (its compat manifest entry); the session row records them, without what this
    /// adapter does not offer (REQ-027).
    pub capabilities: crate::capabilities::Probed,
}

/// Why a turn was not started (`BLOCKED_CAPABILITY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked(pub String);

/// Why this pi's help gives the adapter no way to run a turn at all (no `--mode` with `rpc`, no `--session-dir` or
/// `--session-id`), or `None`. The probe applies the same rule, so doctor never calls a pi compatible that the adapter
/// cannot drive (M1R-03).
pub fn interface_missing(help: &str) -> Option<String> {
    if let Some(flag) = ["--mode", "--session-dir", "--session-id"]
        .into_iter()
        .find(|flag| !offers(help, flag))
    {
        return Some(format!("pi --help offers no {flag}"));
    }
    // rpc among --mode's own values.
    (!crate::adapter::option_text(help, "--mode")
        .is_some_and(|t| crate::adapter::has_word(&t, "rpc")))
    .then(|| "pi --help offers no --mode rpc".to_owned())
}

/// The built-in tools `help` lists, by name with their description, or `None` when it has no such list: the lines
/// after the one that is `Built-in Tool Names:` once trimmed, up to an empty line or the first line not indented, each
/// split at its first ` - ` into a name and a description, both trimmed (the recorded help pads the names:
/// `bash       - Execute bash commands`, M2T15D-200). A line without ` - ` continues the description before it, and a
/// name listed again adds its description to the first, so a note such as `off by default` is never dropped
/// (docs/adr/ADR-030.md decision 12).
pub fn builtin_tools(help: &str) -> Option<std::collections::BTreeMap<String, String>> {
    let mut lines = help
        .lines()
        .skip_while(|line| line.trim() != "Built-in Tool Names:");
    lines.next()?;
    let mut tools = std::collections::BTreeMap::<String, String>::new();
    let mut last: Option<String> = None;
    for line in lines.take_while(|line| !line.trim().is_empty() && line.starts_with([' ', '\t'])) {
        let (name, about) = match (line.split_once(" - "), &last) {
            (Some((name, about)), _) => (name.trim(), about.trim()),
            (None, Some(previous)) => (previous.as_str(), line.trim()),
            (None, None) => (line.trim(), ""),
        };
        let name = name.to_owned();
        tools
            .entry(name.clone())
            .and_modify(|before| {
                before.push(' ');
                before.push_str(about);
            })
            .or_insert_with(|| about.to_owned());
        last = Some(name);
    }
    Some(tools)
}

/// The read tools of the read-only workspace profile (docs/adr/ADR-032.md decision 3).
pub const READ_TOOLS: &[&str] = &["read", "grep", "find", "ls"];

/// Why this pi's help gives no read-only workspace profile, or `None`: `--tools`, `--no-mcp` and `--no-extensions`
/// declared, and each read tool among its built-in tools (docs/adr/ADR-032.md decisions 3, 4 and 11; the probe's rule).
pub fn read_only_missing(help: &str) -> Option<String> {
    if let Some(flag) = ["--tools", "--no-mcp", "--no-extensions"]
        .into_iter()
        .find(|flag| !offers(help, flag))
    {
        return Some(format!(
            "pi --help offers no {flag} for the read-only profile"
        ));
    }
    let builtin = builtin_tools(help).unwrap_or_default();
    READ_TOOLS
        .iter()
        .find(|tool| !builtin.contains_key(**tool))
        .map(|tool| format!("pi --help lists no built-in tool {tool} for the read-only profile"))
}

/// The arguments of `pi --mode rpc` on the session (never `--no-session`); under the read-only workspace profile the
/// allowlist of the read tools, no MCP and no extension at all (docs/adr/ADR-032.md decisions 3 and 11).
pub fn args(launch: &Launch) -> Result<Vec<String>, Blocked> {
    if let Some(why) = interface_missing(&launch.help) {
        return Err(Blocked(why));
    }
    let mut args = vec![
        "--mode".to_owned(),
        "rpc".to_owned(),
        "--session-dir".to_owned(),
        launch.session_dir.to_string_lossy().into_owned(),
        "--session-id".to_owned(),
        launch.session_id.clone(),
    ];
    if launch.profile == crate::Profile::ReadOnly {
        if let Some(why) = read_only_missing(&launch.help) {
            return Err(Blocked(why));
        }
        args.extend([
            "--tools".to_owned(),
            READ_TOOLS.join(","),
            "--no-mcp".to_owned(),
            "--no-extensions".to_owned(),
        ]);
    }
    for (flag, value) in [("--provider", &launch.provider), ("--model", &launch.model)] {
        if let Some(value) = value {
            if !offers(&launch.help, flag) {
                return Err(Blocked(format!("pi --help offers no {flag}")));
            }
            args.extend([flag.to_owned(), value.clone()]);
        }
    }
    Ok(args)
}

/// Dialog UI methods: they wait for an answer, which headless HoH gives as `cancelled`.
pub const DIALOGS: &[&str] = &["select", "confirm", "input", "editor"];

/// The pi versions whose recorded traffic has `agent_settled` (`tests/fixtures/protocol/pi/<version>/`).
const SETTLED_EVENT_VERSIONS: &[&str] = &["1.5.0"];

/// Whether a turn of the pi whose `--version` says `version` can rely on `agent_settled` ([`Launch::settled_event`]):
/// only a recorded version; any other checks for idle with `get_state` (ADR-013 decision 3), which ends a turn on
/// `agent_settled` too, so either kind of version settles (M1R-06).
pub fn emits_settled(version: Option<&str>) -> bool {
    version.is_some_and(|v| SETTLED_EVENT_VERSIONS.contains(&v.trim()))
}

/// The failure reason of a pi error text (REQ-026, CR-003; docs/13): pi 1.5.0 has no structured error codes, so only
/// the recorded patterns count — `No API key found` → BLOCKED_AUTH, `Model not found` → MODEL_UNAVAILABLE (leading
/// blanks aside) — and anything else is PERMANENT_EXTERNAL, its text kept in the event. The same rule serves a refused
/// prompt and a provider error after acceptance (the final assistant message's `errorMessage`, M1T05R-04). HoH does
/// not read statuses or phrases out of provider text (M1T05R-17..20): the formats differ by provider and SDK, and pi
/// has already retried what it judged transient before the turn ends with the error.
pub fn error_class(error: &str) -> &'static str {
    let text = error.trim_start();
    if text.starts_with("No API key found") {
        "BLOCKED_AUTH"
    } else if text.starts_with("Model not found") {
        "MODEL_UNAVAILABLE"
    } else {
        "PERMANENT_EXTERNAL"
    }
}

/// A turn to run.
#[derive(Debug, Clone)]
pub struct Turn {
    pub launch: Launch,
    /// Who starts the agent (docs/adr/ADR-031.md, 设计修正 2): the host's direct spawner, or doctor's gate.
    pub spawner: std::sync::Arc<dyn hoh_core::spawn::Spawner>,
    pub prompt: String,
    /// The input the prompt delivers (QUEUED in the store): DELIVERED when Pi takes it, APPLIED when its run starts
    /// (or it was handled), REJECTED when Pi refuses it.
    pub input_id: Option<String>,
    /// Directory for stdout.jsonl and stderr.log (created if needed).
    pub logs: PathBuf,
    /// `native_grace` is how long an `abort` may take; then SIGTERM and SIGKILL go to the group.
    pub escalation: Escalation,
    pub deadline: Option<Duration>,
    /// The turn's own timers (the deadline, the abort grace).
    pub clock: Arc<dyn Clock>,
    /// Every record Pi writes, in order (the adapter's subscribe()).
    pub events: Option<mpsc::UnboundedSender<Value>>,
    /// A test's look at the diagnostics a turn of the runner's mode collects; the product has none.
    pub observe: Option<Observe>,
}

impl Turn {
    /// The initial graces (docs/13: 10 s / 5 s); the native cancel is the `abort` command, not a signal.
    pub fn default_escalation() -> Escalation {
        Escalation {
            native: None,
            native_grace: Duration::from_secs(10),
            term_grace: Duration::from_secs(5),
            ..Escalation::default()
        }
    }
}

/// What ended a settled turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettledBy {
    /// The prompt's disposition was `handled`: no run started.
    Handled,
    AgentSettled,
    /// `get_state` said idle (a version without `agent_settled`).
    State,
}

/// What the end of a turn is judged on (see [`verdict`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Ending<'a> {
    pub cancelled: bool,
    pub too_large: bool,
    pub deadline: bool,
    pub logs_incomplete: bool,
    /// The prompt was refused, or the session was not the one asked for.
    pub failure: Option<&'a str>,
    pub settled: Option<SettledBy>,
    /// Pi's `agent_settled` said the run was aborted.
    pub aborted: bool,
    pub tool_started: bool,
    /// The class of the provider error the final assistant message stopped with, if it did.
    pub provider_error: Option<&'a str>,
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
    match e.settled {
        Some(_) if e.aborted => (AttemptState::Cancelled, None),
        Some(_) if e.provider_error.is_some() => failed(e.provider_error.unwrap_or_default()),
        Some(_) => (AttemptState::Settled, None),
        None if e.tool_started => (
            AttemptState::UnknownEffect,
            Some("PROTOCOL_INCOMPLETE".to_owned()),
        ),
        None => failed("PROTOCOL_INCOMPLETE"),
    }
}

/// What a turn did.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub state: AttemptState,
    pub failure: Option<String>,
    pub args: Vec<String>,
    /// The session id `get_state` reported.
    pub native_session: Option<String>,
    pub native_path: PathBuf,
    pub model: Option<String>,
    pub disposition: Option<String>,
    pub settled_by: Option<SettledBy>,
    /// The text of the last assistant message.
    pub reply: Option<String>,
    /// stdout lines that were not JSON objects.
    pub parse_errors: usize,
    /// `get_state` queries made to check for idle.
    pub state_queries: usize,
    pub exit: Option<Termination>,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
    pub pid: Option<u32>,
    pub steps: Vec<Step>,
    /// The most stdout bytes the frame reader held at once (AC-027-03's probe; at most the frame limit and one read
    /// chunk); `None` when no process was read.
    pub max_buffered: Option<usize>,
}

/// What a command of ours was.
enum Pending {
    /// The first `get_state`, checking the session.
    Session,
    /// A `get_state` after `agent_end`, checking for idle.
    Idle,
    Prompt,
    Abort,
}

/// The text of a message's text blocks.
fn text_of(message: &Value) -> String {
    message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

/// A timer of the turn's clock, or never.
async fn fire(timer: &mut Option<Timer>) {
    match timer {
        Some(t) => t.await,
        None => std::future::pending().await,
    }
}

/// Run one turn as `attempt` (created PREPARED by the caller); a cancel accepted by `cancel` cancels it.
pub async fn run(
    store: &Store,
    attempt: &AttemptRef,
    turn: &Turn,
    cancel: &CancelLatch,
) -> Result<Outcome, AdapterError> {
    use AttemptState::*;
    let launch = &turn.launch;
    let id = &attempt.attempt_id;
    let (stdout, stderr) = (turn.logs.join("stdout.jsonl"), turn.logs.join("stderr.log"));
    let early_end = |state: AttemptState, failure: Option<String>| Outcome {
        state,
        failure,
        args: Vec::new(),
        native_session: None,
        native_path: launch.session_dir.clone(),
        model: None,
        disposition: None,
        settled_by: None,
        reply: None,
        parse_errors: 0,
        state_queries: 0,
        exit: None,
        stdout: stdout.clone(),
        stderr: stderr.clone(),
        stdout_sha256: String::new(),
        stderr_sha256: String::new(),
        pid: None,
        steps: Vec::new(),
        max_buffered: None,
    };
    // Nothing starts without its capability, in a directory that would block pi, or to resume a session that has no
    // file at all.
    let noted = session_files(&launch.session_dir);
    let refused = match (args(launch), &noted) {
        (Err(Blocked(detail)), _) => Some(("BLOCKED_CAPABILITY", detail)),
        (Ok(_), Err(entry)) => Some((
            "SESSION_DIR_UNSAFE",
            format!("{entry} is not a regular file: pi would block opening it"),
        )),
        (Ok(_), Ok(files)) if launch.resume && files.is_empty() => Some((
            "SESSION_NOT_FOUND",
            format!(
                "no session file in {} to resume {}",
                launch.session_dir.display(),
                launch.session_id
            ),
        )),
        _ => None,
    };
    let noted = noted.unwrap_or_default();
    if let Some((reason, detail)) = refused {
        let state = finish(
            store,
            attempt,
            Prepared,
            Failed,
            AttemptFacts {
                ended_at: now(),
                failure_reason: Some(reason.to_owned()),
                ..AttemptFacts::default()
            },
            cancel,
        )
        .await?;
        store
            .attempt_event(
                id,
                "blocked",
                "adapter.blocked",
                json!({"attempt_id": id, "reason": reason, "detail": detail}),
            )
            .await?;
        return Ok(early_end(
            state,
            (state == Failed).then(|| reason.to_owned()),
        ));
    }
    let args = args(launch).map_err(|Blocked(d)| AdapterError::Io(std::io::Error::other(d)))?;
    for dir in [&turn.logs, &launch.session_dir] {
        if let Err(e) = std::fs::create_dir_all(dir) {
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
    }
    store
        .upsert_session(Session {
            session_id: attempt.session_id.clone(),
            agent: "pi".to_owned(),
            role: attempt.role.clone(),
            native_id: Some(launch.session_id.clone()),
            native_path: Some(launch.session_dir.to_string_lossy().into_owned()),
            agent_version: attempt.agent_version.clone(),
            // The probed capabilities this adapter implements: events, resume, commands (M1R-01, docs/adr/ADR-030.md
            // decision 12) and the read-only workspace profile (docs/adr/ADR-032.md decision 4). It takes no steer,
            // answers no dialog and records no token usage (M1T04R-16); whether pi emits agent_settled is the
            // adapter's setting, not a capability of the session (M1T04R-13).
            capabilities: session_row(launch),
        })
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
    let mut command = Command::new(&launch.program);
    command.args(&args).current_dir(&launch.cwd);
    let config = Config {
        escalation: Escalation {
            native: None,
            native_grace: Duration::ZERO,
            ..turn.escalation
        },
        deadline: None,
        stdout_copy: Some(stdout.clone()),
        stderr_copy: Some(stderr.clone()),
        ..Config::default()
    };
    // The substrate reads stdout and stderr from the start: nothing Pi writes can be missed or block it.
    let mut process = match Process::spawn(command, config, &*turn.spawner) {
        Ok(p) => p,
        Err(e) => {
            finish(
                store,
                attempt,
                Starting,
                Failed,
                AttemptFacts {
                    ended_at: now(),
                    failure_reason: Some(format!("spawn: {e}")),
                    ..AttemptFacts::default()
                },
                cancel,
            )
            .await?;
            return Err(e.into());
        }
    };
    let pid = process.pid();
    step(
        store,
        attempt,
        Starting,
        Running,
        crate::identity::running_pid(pid),
    )
    .await?;
    let mut sink = Sink {
        store: Some(store),
        recorded: Vec::new(),
        observer: Observer::default(),
    };
    let driven = drive(
        &mut sink,
        attempt,
        turn,
        &mut process,
        Stop::Latch(cancel),
        None,
        &noted,
        &mut Watch::new(None),
    )
    .await?;
    let state = finish(
        store,
        attempt,
        Running,
        driven.state,
        AttemptFacts {
            ended_at: now(),
            resolved_model: driven.model.clone(),
            failure_reason: driven.failure.clone(),
            cancel_requested: Some(driven.cancelled),
            ..AttemptFacts::default()
        },
        cancel,
    )
    .await?;
    Ok(Outcome {
        failure: if state == Cancelled {
            None
        } else {
            driven.failure
        },
        state,
        args,
        native_session: driven.native_session,
        native_path: launch.session_dir.clone(),
        model: driven.model,
        disposition: driven.disposition,
        settled_by: driven.settled,
        reply: driven.reply,
        parse_errors: driven.parse_errors,
        state_queries: driven.state_queries,
        exit: driven.report.as_ref().and_then(|r| r.status),
        stdout_sha256: sha256_file(&stdout)?,
        stderr_sha256: sha256_file(&stderr)?,
        stdout,
        stderr,
        pid: Some(pid),
        max_buffered: driven.report.as_ref().map(|r| r.stdout.max_buffered),
        steps: driven.report.map(|r| r.steps).unwrap_or_default(),
    })
}

/// The capabilities a session of this launch has (docs/adr/ADR-022.md decision 9), as [`run`] records them: events,
/// resume, commands and the read-only workspace profile where its args build (no steer, no dialog, no token usage);
/// or why no turn can start. The row says which profiles the session can start in, whatever this turn's
/// (docs/adr/ADR-032.md decision 4).
pub fn session_capabilities(launch: &Launch) -> Result<Value, Blocked> {
    args(launch)?;
    Ok(session_row(launch))
}

/// [`session_capabilities`] of a launch whose args build.
fn session_row(launch: &Launch) -> Value {
    crate::capabilities::of_session(
        &launch.capabilities,
        &[
            ("structured_events", true),
            ("resume", true),
            ("command_execution", true),
            (
                "read_only_workspace",
                read_only_missing(&launch.help).is_none(),
            ),
        ],
    )
}

/// Start one turn in the runner's mode (docs/adr/ADR-022.md decisions 12 to 14): checked as [`run`] checks it, then
/// through `gate`, held until [`Held::open`] is sent; no attempt state moves, nothing written to the store, the
/// facts come back. A cancel is Pi's `abort` only (stdin closed before the prompt is sent); the host's ladder does
/// the rest. No deadline.
pub fn start(attempt: &AttemptRef, turn: &Turn, gate: &GateCommand) -> Result<Held, Refused> {
    let launch = &turn.launch;
    let refused = |code: &str, detail: String| Refused {
        code: code.to_owned(),
        detail,
    };
    let noted = session_files(&launch.session_dir);
    let args = match (args(launch), &noted) {
        (Err(Blocked(detail)), _) => return Err(refused("BLOCKED_CAPABILITY", detail)),
        (Ok(_), Err(entry)) => {
            return Err(refused(
                "SESSION_DIR_UNSAFE",
                format!("{entry} is not a regular file: pi would block opening it"),
            ));
        }
        (Ok(_), Ok(files)) if launch.resume && files.is_empty() => {
            return Err(refused(
                "SESSION_NOT_FOUND",
                format!(
                    "no session file in {} to resume {}",
                    launch.session_dir.display(),
                    launch.session_id
                ),
            ));
        }
        (Ok(args), _) => args,
    };
    let noted = noted.unwrap_or_default();
    for dir in [&turn.logs, &launch.session_dir] {
        std::fs::create_dir_all(dir).map_err(|e| refused("LOGS", e.to_string()))?;
    }
    let (stdout, stderr) = (turn.logs.join("stdout.jsonl"), turn.logs.join("stderr.log"));
    let mut command = Command::new(&gate.program);
    command
        .args(gate.argv())
        .arg(&launch.program)
        .args(&args)
        .envs(gate.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&launch.cwd);
    let config = Config {
        escalation: Escalation {
            native: None,
            native_grace: Duration::ZERO,
            ..turn.escalation
        },
        deadline: None,
        stdout_copy: Some(stdout),
        stderr_copy: Some(stderr),
        ..Config::default()
    };
    let mut process = Process::spawn(command, config, &*turn.spawner)
        .map_err(|e| refused("SPAWN", e.to_string()))?;
    let pid = process.pid();
    let (open, opened) = tokio::sync::oneshot::channel::<()>();
    let cancel = Arc::new(tokio::sync::Notify::new());
    let native = cancel.clone();
    let (attempt, turn) = (attempt.clone(), turn.clone());
    let raw_log_path = turn.logs.to_string_lossy().into_owned();
    let gate = gate.clone();
    let ended = tokio::spawn(async move {
        if opened.await.is_err() {
            let report = crate::adapter::close_gate(&mut process).await;
            return Facts::gate_closed(raw_log_path, report.as_ref());
        }
        let mut sink = Sink {
            store: None,
            recorded: Vec::new(),
            observer: Observer::new(&attempt.attempt_id, turn.observe.clone()),
        };
        // The agent's files are re-checked before the prompt, the first input, and while the turn runs, from the
        // gate's opening (docs/adr/ADR-030.md decision 9).
        let driven = drive(
            &mut sink,
            &attempt,
            &turn,
            &mut process,
            Stop::Native(&native),
            Some(gate.go()),
            &noted,
            &mut Watch::new(gate.expect.clone()),
        )
        .await;
        match driven {
            // The agent ended without a frame: its gate may have refused it (docs/adr/ADR-030.md decision 9).
            Ok(driven)
                if driven.state != AttemptState::Cancelled
                    && let Some(refused) = crate::adapter::refused_at_gate(
                        &gate,
                        &raw_log_path,
                        driven.report.as_ref(),
                        &sink.recorded,
                    ) =>
            {
                sink.observer.refused(refused)
            }
            Ok(driven) => Facts {
                state: driven.state,
                failure: driven.failure,
                native_session: driven.native_session,
                resolved_model: driven.model,
                usage: None,
                raw_log_path,
                max_buffered: driven.report.as_ref().map(|r| r.stdout.max_buffered),
                group_reaped: driven.report.as_ref().is_some_and(|r| r.group_reaped),
                recorded: sink.recorded,
                degraded: None,
                final_text: driven.reply,
                compat: None,
                retry_at: None,
            }
            .before_input(driven.changed.as_ref()),
            Err(_) => Facts::incomplete(raw_log_path, sink.recorded),
        }
    });
    Ok(Held {
        pid,
        open,
        cancel,
        ended,
    })
}

/// What a Pi turn showed, and the adapter's verdict.
struct Driven {
    state: AttemptState,
    failure: Option<String>,
    cancelled: bool,
    native_session: Option<String>,
    model: Option<String>,
    disposition: Option<String>,
    settled: Option<SettledBy>,
    reply: Option<String>,
    parse_errors: usize,
    state_queries: usize,
    report: Option<crate::process::ExitReport>,
    /// What its re-check before the prompt found changed (docs/adr/ADR-030.md decision 9).
    changed: Option<Changed>,
}

/// What a cancel or the deadline asks of the driver once [`Rpc::abort`] has done its part.
enum AbortNext {
    /// Give the `abort` its native grace (the direct mode).
    Grace,
    /// End the process now.
    HardStop,
    Nothing,
}

/// What a Pi turn knows while it runs, judged record by record ([`Rpc::on_record`]); the driver owns the process and
/// the timers, a replay has neither.
struct Rpc<'a> {
    turn: &'a Turn,
    /// The attempt's id.
    id: &'a str,
    /// The session files noted before the start.
    noted: &'a [SessionFile],
    lines: Option<mpsc::UnboundedSender<Vec<u8>>>,
    pending: HashMap<String, Pending>,
    next: usize,
    cancelled: bool,
    native_only: bool,
    deadline: bool,
    too_large: bool,
    tool_started: bool,
    aborted: bool,
    failure: Option<String>,
    settled: Option<SettledBy>,
    disposition: Option<String>,
    native_session: Option<String>,
    model: Option<String>,
    reply: Option<String>,
    unknown_records: usize,
    parse_errors: usize,
    state_queries: usize,
    agent_starts: usize,
    dialogs: usize,
    data: usize,
    prompt_sent: bool,
    stopped: bool,
    abort_sent: bool,
    /// The input's facts, applied in order whichever comes first (M1T05R-01): Pi took the prompt, its run began.
    accepted: bool,
    begun: bool,
    input_at: InputState,
    /// Once a move of it is refused (someone else moved it on), the turn leaves the input alone.
    input: Option<String>,
    /// A busy answer is checked again once work ends (M1T05R-02).
    recheck: bool,
    ended_since_query: bool,
    provider_error: Option<String>,
    /// stdin was closed since the driver last looked: Pi is given the term grace to exit.
    closed: bool,
    /// What the gate verified (the runner's mode): re-checked before the prompt, the first input (docs/adr/ADR-030.md
    /// decision 9).
    expect: Option<Expect>,
    /// What that re-check found changed: no prompt was sent.
    changed: Option<Changed>,
    /// A replay's recorded identity of the file get_state named (docs/adr/ADR-032.md design correction 7 item 2):
    /// used instead of looking at the file, which a replay never does. `None` live.
    opened: Option<Option<SessionFile>>,
}

/// The session check live and replay share (docs/adr/ADR-032.md design correction 7 item 2): get_state's session is
/// the launch's, and a resumed session reopens a file noted before the start, by the identity (canonical path,
/// device, inode) of the file get_state named as observed when its answer arrived; a file pi made anew, one gone or
/// one not written yet is not. Live passes what it just observed ([`identity`]), a replay what was recorded.
pub(crate) fn session_matches(
    launch: &Launch,
    noted: &[SessionFile],
    answered: Option<&str>,
    opened: Option<&SessionFile>,
) -> bool {
    answered == Some(launch.session_id.as_str())
        && (!launch.resume || opened.is_some_and(|o| noted.contains(o)))
}

impl<'a> Rpc<'a> {
    fn new(
        turn: &'a Turn,
        id: &'a str,
        noted: &'a [SessionFile],
        lines: mpsc::UnboundedSender<Vec<u8>>,
        native_only: bool,
        cancelled: bool,
    ) -> Rpc<'a> {
        Rpc {
            turn,
            id,
            noted,
            lines: Some(lines),
            pending: HashMap::new(),
            next: 0,
            cancelled,
            native_only,
            deadline: false,
            too_large: false,
            tool_started: false,
            aborted: false,
            failure: None,
            settled: None,
            disposition: None,
            native_session: None,
            model: None,
            reply: None,
            unknown_records: 0,
            parse_errors: 0,
            state_queries: 0,
            agent_starts: 0,
            dialogs: 0,
            data: 0,
            prompt_sent: false,
            stopped: false,
            abort_sent: false,
            accepted: false,
            begun: false,
            input_at: InputState::Queued,
            input: turn.input_id.clone(),
            recheck: false,
            ended_since_query: false,
            provider_error: None,
            closed: false,
            expect: None,
            changed: None,
            opened: None,
        }
    }

    fn send(&self, message: Value) {
        if let Some(l) = &self.lines {
            let mut line = message.to_string().into_bytes();
            line.push(b'\n');
            let _ = l.send(line);
        }
    }

    fn command_id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("hoh-{prefix}-{}", self.next)
    }

    /// The first `get_state`, checking the session.
    fn open(&mut self) {
        let state_id = self.command_id("state");
        self.pending.insert(state_id.clone(), Pending::Session);
        self.send(json!({"id": state_id, "type": "get_state"}));
    }

    /// Close stdin; the driver gives Pi the term grace to exit.
    fn close(&mut self) {
        self.lines = None;
        self.closed = true;
    }

    /// A cancel or the deadline: `abort` a prompt that runs; else close stdin (the runner's mode) or end the process.
    fn abort(&mut self) -> AbortNext {
        if self.prompt_sent && self.settled.is_none() && self.lines.is_some() && !self.abort_sent {
            let abort_id = self.command_id("abort");
            self.pending.insert(abort_id.clone(), Pending::Abort);
            self.send(json!({"id": abort_id, "type": "abort"}));
            self.abort_sent = true;
            // The runner's mode: the abort only; the host's ladder ends the process (ADR-022 decision 14).
            if self.native_only {
                AbortNext::Nothing
            } else {
                AbortNext::Grace
            }
        } else if self.native_only {
            self.lines = None;
            AbortNext::Nothing
        } else if !self.stopped {
            self.lines = None;
            self.stopped = true;
            AbortNext::HardStop
        } else {
            AbortNext::Nothing
        }
    }

    async fn sync_input(&mut self, sink: &mut Sink<'_>) -> Result<(), PersistError> {
        if self.accepted
            && self.input_at == InputState::Queued
            && let Some(i) = &self.input
        {
            if sink
                .soft_move(self.id, i, InputState::Queued, InputState::Delivered, None)
                .await?
            {
                self.input_at = InputState::Delivered;
            } else {
                self.input = None;
            }
        }
        if self.begun
            && self.input_at == InputState::Delivered
            && let Some(i) = &self.input
        {
            if sink
                .soft_move(self.id, i, InputState::Delivered, InputState::Applied, None)
                .await?
            {
                self.input_at = InputState::Applied;
            } else {
                self.input = None;
            }
        }
        Ok(())
    }

    fn check_idle(&mut self) {
        let asking = self.pending.values().any(|p| matches!(p, Pending::Idle));
        if !self.turn.launch.settled_event
            && self.settled.is_none()
            && self.lines.is_some()
            && !asking
        {
            let check = self.command_id("state");
            self.pending.insert(check.clone(), Pending::Idle);
            self.send(json!({"id": check, "type": "get_state"}));
            self.state_queries += 1;
            self.ended_since_query = false;
        }
    }

    /// Judge one stdout line and record what it shows.
    async fn on_record(&mut self, sink: &mut Sink<'_>, bytes: &[u8]) -> Result<(), PersistError> {
        let (id, turn) = (self.id, self.turn);
        let launch = &turn.launch;
        let Some(record) = serde_json::from_slice::<Value>(bytes)
            .ok()
            .filter(Value::is_object)
        else {
            self.parse_errors += 1;
            return sink
                .attempt_event(
                    id,
                    &format!("diagnostic-{}", self.parse_errors),
                    "adapter.diagnostic",
                    json!({"attempt_id": id, "reason": "stdout line is not a JSON object", "bytes": bytes.len(),
                           "head": String::from_utf8_lossy(&bytes[..bytes.len().min(256)])}),
                )
                .await;
        };
        if let Some(events) = &turn.events {
            let _ = events.send(record.clone());
        }
        let key = known::pi_key(&record);
        if !known::is_known(known::pi(), key.as_deref()) {
            self.unknown_records += 1;
            sink.attempt_event(
                id,
                &format!("unknown-{}", self.unknown_records),
                "adapter.unknown_event",
                known::unknown_event(id, key.as_deref(), bytes, &record),
            )
            .await?;
        }
        match record["type"].as_str().unwrap_or_default() {
            "response" => {
                let what = record["id"].as_str().and_then(|i| self.pending.remove(i));
                let ok = record["success"] == true;
                let error = record["error"].as_str().unwrap_or_default().to_owned();
                match what {
                    Some(Pending::Session) if ok => {
                        self.native_session =
                            record["data"]["sessionId"].as_str().map(str::to_owned);
                        self.model = record["data"]["model"]["id"].as_str().map(str::to_owned);
                        // A resumed session is one of the files noted before the start, as pi itself chose it; a
                        // file it made anew, one gone or one not written yet is not (module docs).
                        let opened = match &self.opened {
                            Some(recorded) => recorded.clone(),
                            None => record["data"]["sessionFile"].as_str().and_then(identity),
                        };
                        if !session_matches(
                            launch,
                            self.noted,
                            self.native_session.as_deref(),
                            opened.as_ref(),
                        ) {
                            self.failure = Some("SESSION_MISMATCH".to_owned());
                            sink.attempt_event(id, "session-mismatch", "adapter.diagnostic",
                                               json!({"attempt_id": id, "reason": "SESSION_MISMATCH",
                                                      "asked": launch.session_id, "got": self.native_session,
                                                      "noted": self.noted.iter().map(|(p, _, _)| p).collect::<Vec<_>>(),
                                                      "opened": record["data"]["sessionFile"]}))
                                .await?;
                            self.close();
                        } else if let Some(changed) = self
                            .expect
                            .as_ref()
                            .and_then(|e| crate::adapter::recheck(e).changed())
                        {
                            // Before the first input, the agent's files are still the ones its gate verified
                            // (docs/adr/ADR-030.md decision 9, "首个输入之前的再核对"): else no prompt is sent and stdin
                            // is closed, as for a session mismatch.
                            self.failure = Some(crate::adapter::COMPAT_UNVERIFIED.to_owned());
                            let (key, mut payload) = changed.event();
                            payload["attempt_id"] = json!(id);
                            sink.attempt_event(id, &key, "adapter.diagnostic", payload)
                                .await?;
                            self.changed = Some(changed);
                            self.close();
                        } else if self.lines.is_some() {
                            let prompt_id = self.command_id("prompt");
                            self.pending.insert(prompt_id.clone(), Pending::Prompt);
                            self.send(
                                json!({"id": prompt_id, "type": "prompt", "message": turn.prompt}),
                            );
                            self.prompt_sent = true;
                        }
                    }
                    Some(Pending::Session) => {
                        self.failure = Some(format!("get_state: {error}"));
                        self.close();
                    }
                    Some(Pending::Prompt) if ok => {
                        self.disposition =
                            record["data"]["disposition"].as_str().map(str::to_owned);
                        self.accepted = true;
                        if self.disposition.as_deref() == Some("handled") {
                            self.settled = Some(SettledBy::Handled);
                            self.begun = true;
                            self.close();
                        }
                        self.sync_input(sink).await?;
                    }
                    Some(Pending::Prompt) => {
                        let reason = error_class(&error);
                        self.failure = Some(reason.to_owned());
                        sink.attempt_event(
                            id,
                            "prompt-refused",
                            "adapter.prompt_refused",
                            json!({"attempt_id": id, "reason": reason, "error": error}),
                        )
                        .await?;
                        if self.input_at == InputState::Queued
                            && let Some(i) = self.input.take()
                            && sink
                                .soft_move(
                                    id,
                                    &i,
                                    InputState::Queued,
                                    InputState::Rejected,
                                    Some(error),
                                )
                                .await?
                        {
                            self.input_at = InputState::Rejected;
                        }
                        self.close();
                    }
                    Some(Pending::Idle) => {
                        let d = &record["data"];
                        let idle = ok
                            && d["isStreaming"] == false
                            && d["isCompacting"] == false
                            && d["pendingMessageCount"].as_u64() == Some(0);
                        if idle && self.settled.is_none() {
                            self.settled = Some(SettledBy::State);
                            self.close();
                        } else if !idle && self.ended_since_query {
                            // Work ended while this answer was on its way: it may be stale already.
                            self.check_idle();
                        } else if !idle {
                            self.recheck = true;
                        }
                    }
                    // Pi answers an abort once it is idle; its agent_settled came first.
                    Some(Pending::Abort) if self.settled.is_some() => {
                        self.close();
                    }
                    Some(Pending::Abort) | None => {}
                }
            }
            "agent_start" => {
                self.agent_starts += 1;
                sink.attempt_event(
                    id,
                    &format!("agent-start-{}", self.agent_starts),
                    "adapter.agent_start",
                    json!({"attempt_id": id}),
                )
                .await?;
                self.begun = true;
                self.sync_input(sink).await?;
            }
            "agent_end" => {
                self.ended_since_query = true;
                self.recheck = false;
                self.check_idle();
            }
            // Ends of work after a run: a busy answer is checked again.
            "compaction_end" | "auto_retry_end" | "queue_update" | "turn_end" | "message_end"
            | "tool_execution_end"
                if !launch.settled_event =>
            {
                self.ended_since_query = true;
                if self.recheck {
                    self.recheck = false;
                    self.check_idle();
                }
                if record["type"] == "message_end" && record["message"]["role"] == "assistant" {
                    note_message(
                        &record["message"],
                        &mut self.reply,
                        &mut self.provider_error,
                    );
                }
            }
            "agent_settled" => {
                if self.settled.is_none() {
                    self.settled = Some(SettledBy::AgentSettled);
                    self.aborted = record["aborted"] == true;
                }
                self.close();
            }
            "tool_execution_start" => self.tool_started = true,
            "message_end" if record["message"]["role"] == "assistant" => {
                note_message(
                    &record["message"],
                    &mut self.reply,
                    &mut self.provider_error,
                );
            }
            "extension_ui_request" => {
                let method = record["method"].as_str().unwrap_or_default();
                if DIALOGS.contains(&method) {
                    // Headless: no option is chosen for anyone.
                    self.dialogs += 1;
                    self.send(
                        json!({"type": "extension_ui_response", "id": record["id"], "cancelled": true}),
                    );
                    sink.attempt_event(
                        id,
                        &format!("ui-request-{}", self.dialogs),
                        "adapter.ui_request",
                        json!({"attempt_id": id, "method": method, "request": record,
                                              "decision": "cancelled"}),
                    )
                    .await?;
                } else {
                    self.data += 1;
                    sink.attempt_event(
                        id,
                        &format!("ui-data-{}", self.data),
                        "adapter.ui_data",
                        json!({"attempt_id": id, "method": method, "request": record}),
                    )
                    .await?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The verdict once the process ended (`report`; `None`: it was not seen to end) and stdin is closed, with what it
    /// records.
    async fn conclude(
        self,
        sink: &mut Sink<'_>,
        report: Option<crate::process::ExitReport>,
    ) -> Result<Driven, PersistError> {
        let id = self.id;
        let logs_incomplete = report.as_ref().is_none_or(|r| {
            r.stdout.error.is_some() || r.stderr.error.is_some() || !r.stdout.eof || !r.stderr.eof
        });
        if logs_incomplete && let Some(r) = &report {
            sink.attempt_event(
                id,
                "raw-log-incomplete",
                "adapter.diagnostic",
                json!({"attempt_id": id, "reason": "RAW_LOG_INCOMPLETE",
                           "stdout": {"eof": r.stdout.eof, "error": r.stdout.error},
                           "stderr": {"eof": r.stderr.eof, "error": r.stderr.error}}),
            )
            .await?;
        }
        let provider_class = self.provider_error.as_deref().map(error_class);
        let (state, failure) = verdict(&Ending {
            cancelled: self.cancelled,
            too_large: self.too_large,
            deadline: self.deadline,
            logs_incomplete,
            failure: self.failure.as_deref(),
            settled: self.settled,
            aborted: self.aborted,
            tool_started: self.tool_started,
            provider_error: provider_class,
        });
        if let (Some(class), Some(message)) = (provider_class, &self.provider_error) {
            sink.attempt_event(
                id,
                "provider-failed",
                "adapter.turn_failed",
                json!({"attempt_id": id, "reason": class, "errorMessage": message}),
            )
            .await?;
        }
        Ok(Driven {
            state,
            failure,
            cancelled: self.cancelled,
            // A prompt pi handled started no run: pi wrote no message, so no session file, and a later turn must not be
            // sent to resume it (M2T17R-551; docs/adr/ADR-024.md decision 4).
            native_session: self
                .native_session
                .filter(|_| self.settled != Some(SettledBy::Handled)),
            model: self.model,
            disposition: self.disposition,
            settled: self.settled,
            reply: self.reply,
            parse_errors: self.parse_errors,
            state_queries: self.state_queries,
            report,
            changed: self.changed,
        })
    }
}

/// Drive one Pi turn on `process` until it ended: the session query, the prompt, its records, dialogs answered
/// headless, a cancel from `stop` (an abort; in the direct mode then SIGTERM and SIGKILL after their graces).
/// `preamble` is written first (the runner's mode: the gate's `go`). `watch`'s expectation is re-checked before the
/// prompt, and while the turn runs as it is due.
#[allow(clippy::too_many_arguments)]
async fn drive(
    sink: &mut Sink<'_>,
    attempt: &AttemptRef,
    turn: &Turn,
    process: &mut Process,
    stop: Stop<'_>,
    preamble: Option<Vec<u8>>,
    noted: &[SessionFile],
    watch: &mut Watch,
) -> Result<Driven, AdapterError> {
    let id = &attempt.attempt_id;
    let (lines, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
    // The runner's mode: the gate's line goes first.
    if let Some(preamble) = preamble {
        let _ = lines.send(preamble);
    }
    let input = process.take_stdin();
    let mut writer = tokio::spawn(async move {
        let mut input = input.ok_or_else(|| std::io::Error::other("no stdin pipe"))?;
        while let Some(line) = queue.recv().await {
            input.write_all(&line).await?;
        }
        input.shutdown().await
    });
    let mut r = Rpc::new(
        turn,
        id,
        noted,
        lines,
        stop.is_native(),
        stop.requested_now(),
    );
    r.expect = watch.expect().cloned();
    r.open();
    let mut written = false;
    let mut deadline_timer: Option<Timer> = turn.deadline.map(|d| turn.clock.sleep(d));
    let mut abort_timer: Option<Timer> = None;
    // Process lifecycle, not a judgement of the turn: how long Pi may take to exit once stdin is closed.
    let mut exit_timer: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    let hard_stop = |process: &Process| {
        process.cancel(Escalation {
            native: None,
            native_grace: Duration::ZERO,
            ..turn.escalation
        });
    };
    let abort =
        |r: &mut Rpc<'_>, process: &Process, abort_timer: &mut Option<Timer>| match r.abort() {
            AbortNext::Grace => *abort_timer = Some(turn.clock.sleep(turn.escalation.native_grace)),
            AbortNext::HardStop => hard_stop(process),
            AbortNext::Nothing => {}
        };
    if r.cancelled {
        abort(&mut r, process, &mut abort_timer);
    }
    let report = loop {
        let event = tokio::select! {
            biased;
            () = stop.requested(), if !r.cancelled => {
                r.cancelled = true;
                abort(&mut r, process, &mut abort_timer);
                continue;
            }
            // A re-check that is due comes before output (the same branch wakes an idle loop for it).
            () = watch.due(), if watch.on() => {
                for (key, mut payload) in watch.check() {
                    payload["attempt_id"] = json!(id);
                    sink.attempt_event(id, &key, "adapter.diagnostic", payload).await?;
                }
                continue;
            }
            () = fire(&mut deadline_timer), if deadline_timer.is_some() && !r.deadline => {
                r.deadline = true;
                deadline_timer = None;
                abort(&mut r, process, &mut abort_timer);
                continue;
            }
            () = fire(&mut abort_timer), if abort_timer.is_some() && !r.stopped => {
                abort_timer = None;
                r.lines = None;
                hard_stop(process);
                r.stopped = true;
                continue;
            }
            () = async { if let Some(t) = exit_timer.as_mut() { t.await } }, if exit_timer.is_some() && !r.stopped => {
                exit_timer = None;
                hard_stop(process);
                r.stopped = true;
                continue;
            }
            result = &mut writer, if !written => {
                written = true;
                let failed = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(e.to_string()),
                };
                // Pi can no longer be told anything: stop it; the stream's end decides the outcome.
                if let Some(e) = failed.filter(|_| r.lines.is_some()) {
                    sink
                        .attempt_event(id, "stdin-failed", "adapter.diagnostic",
                                       json!({"attempt_id": id, "reason": "stdin failed", "error": e}))
                        .await?;
                    r.lines = None;
                    hard_stop(process);
                    r.stopped = true;
                }
                continue;
            }
            event = process.next() => event,
        };
        let bytes = match event {
            Some(Event::Frame(Frame::Line(bytes) | Frame::Partial(bytes))) => bytes,
            Some(Event::Frame(Frame::TooLarge(refused))) => {
                r.too_large = true;
                r.lines = None;
                hard_stop(process);
                r.stopped = true;
                sink
                    .attempt_event(
                        id,
                        "frame-too-large",
                        "adapter.diagnostic",
                        json!({"attempt_id": id, "reason": "PROTOCOL_FRAME_TOO_LARGE", "length": refused.length,
                               "prefix_sha256": refused.prefix_sha256,
                               "head": String::from_utf8_lossy(&refused.head)}),
                    )
                    .await?;
                continue;
            }
            Some(Event::Exit(report)) => break Some(*report),
            None => break None,
        };
        r.on_record(sink, &bytes).await?;
        // stdin was closed: Pi has the term grace to exit by itself.
        if std::mem::take(&mut r.closed) && exit_timer.is_none() {
            exit_timer = Some(Box::pin(tokio::time::sleep(turn.escalation.term_grace)));
        }
    };
    r.lines = None;
    if !written {
        writer.abort();
    }
    Ok(r.conclude(sink, report).await?)
}

/// A pi turn of the runner's mode driven on `process` (its gate open) with `watch`, its diagnostics shown to
/// `observer`, for the adapters' unit tests: its verdict and what it recorded.
#[cfg(test)]
pub(crate) async fn drive_watched(
    process: &mut Process,
    watch: &mut Watch,
    observer: Observer,
) -> Result<(AttemptState, Option<String>, Vec<Recorded>), AdapterError> {
    let attempt = AttemptRef {
        attempt_id: "attempt".to_owned(),
        session_id: "session".to_owned(),
        role: "developer".to_owned(),
        agent_version: String::new(),
    };
    let turn = replay_turn("1.5.0");
    let native = tokio::sync::Notify::new();
    let mut sink = Sink {
        store: None,
        recorded: Vec::new(),
        observer,
    };
    let driven = drive(
        &mut sink,
        &attempt,
        &turn,
        process,
        Stop::Native(&native),
        None,
        &[],
        watch,
    )
    .await?;
    Ok((driven.state, driven.failure, sink.recorded))
}

/// The turn a replay judges: the suite's launch, prompt and session; `version` (pi's `--version`) decides whether the
/// turn waits for `agent_settled` or checks for idle ([`emits_settled`]).
fn replay_turn(version: &str) -> Turn {
    Turn {
        launch: Launch {
            program: PathBuf::from("pi"),
            help: String::new(),
            cwd: PathBuf::from(crate::compat::SUITE_CWD),
            session_dir: PathBuf::from(crate::compat::SUITE_PI_SESSION_DIR),
            session_id: crate::compat::SUITE_SESSION.to_owned(),
            resume: false,
            provider: None,
            model: None,
            settled_event: emits_settled(Some(version)),
            profile: crate::Profile::Yolo,
            capabilities: crate::capabilities::Probed::new(),
        },
        prompt: crate::compat::SUITE_PROMPT.to_owned(),
        input_id: None,
        logs: PathBuf::from(crate::compat::SUITE_CWD),
        escalation: Turn::default_escalation(),
        deadline: None,
        clock: Arc::new(crate::clock::SystemClock),
        events: None,
        observe: None,
        spawner: hoh_core::spawn::Direct::shared(),
    }
}

/// Replay `bytes` (pi's stdout, with the host's cancel where [`crate::adapter::REPLAY_CANCEL`] stands) through this
/// adapter's own judging, no process started (docs/adr/ADR-030.md decision 10): see [`Replayed`]. `version` (pi's
/// `--version`) decides whether the turn waits for `agent_settled` or checks for idle ([`emits_settled`]).
pub fn replay(bytes: &[u8], version: &str) -> Replayed {
    let turn = replay_turn(version);
    let mut sink = Sink {
        store: None,
        recorded: Vec::new(),
        observer: Observer::default(),
    };
    let (lines, _written) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut r = Rpc::new(&turn, "replay", &[], lines, true, false);
    r.open();
    for step in crate::adapter::replay_steps(bytes) {
        match step {
            Replay::Cancel if !r.cancelled => {
                // The runner's cancel: the abort only (no timer, no process to stop).
                r.cancelled = true;
                let _ = r.abort();
            }
            Replay::Cancel => {}
            Replay::Frame(line) => match crate::adapter::at_once(r.on_record(&mut sink, &line)) {
                Some(Ok(())) => {}
                Some(Err(e)) => return crate::adapter::unreplayable(&e.to_string()),
                None => return crate::adapter::unreplayable("the judging waited"),
            },
        }
    }
    r.lines = None;
    match crate::adapter::at_once(r.conclude(&mut sink, Some(crate::adapter::replayed_exit()))) {
        Some(Ok(driven)) => Replayed {
            state: driven.state,
            failure: driven.failure,
            unknown_events: crate::adapter::unknown_events(&sink.recorded),
        },
        Some(Err(e)) => crate::adapter::unreplayable(&e.to_string()),
        None => crate::adapter::unreplayable("the judging waited"),
    }
}

/// The prompt a replay's prompt command carries: the recorded one is not an input of the replay, and the request log's
/// prompt text is not compared (docs/adr/ADR-032.md design correction 7 item 2, the comparison contract).
pub const REPLAY_PROMPT: &str = "<the prompt>";

/// Replay a recorded turn (pi's stdout as it was read, to the process's exit) through this adapter's own judging with
/// the observations recorded with it, no process started (docs/adr/ADR-032.md design correction 7): the launch they
/// record (session id, resume) and, resumed, the noted files and the opened file's identity feed the session check
/// live and replay share ([`session_matches`]); the version decides whether the turn waits for `agent_settled`
/// ([`emits_settled`]); the recorded exit and log completion end it. Every message the adapter writes is collected
/// from its own stdin (get_state; the prompt once the session check passes; dialogs cancelled). Observations the
/// contract does not accept give NOT_OBSERVED ([`crate::observed::parse`]).
pub fn replay_facts(stdout: &[u8], observations: &Value) -> crate::observed::ReplayFacts {
    use crate::observed::{Agent, ReplayFacts};
    let observed = match crate::observed::parse(Agent::Pi, observations) {
        Ok(observed) => observed,
        Err(why) => return ReplayFacts::failed(why),
    };
    let Some(launch) = observed.pi.clone() else {
        return ReplayFacts::failed(format!("{} (launch)", crate::observed::NOT_OBSERVED));
    };
    let mut turn = replay_turn(&observed.version);
    turn.launch.session_id = launch.session_id;
    turn.launch.resume = launch.resume;
    turn.prompt = REPLAY_PROMPT.to_owned();
    let mut sink = Sink {
        store: None,
        recorded: Vec::new(),
        observer: Observer::default(),
    };
    let (lines, mut written) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut r = Rpc::new(&turn, "replay", &launch.noted, lines, true, false);
    r.opened = Some(launch.opened.clone());
    r.open();
    for step in crate::adapter::replay_steps(stdout) {
        match step {
            Replay::Cancel if !r.cancelled => {
                r.cancelled = true;
                let _ = r.abort();
            }
            Replay::Cancel => {}
            Replay::Frame(line) => match crate::adapter::at_once(r.on_record(&mut sink, &line)) {
                Some(Ok(())) => {}
                Some(Err(e)) => return ReplayFacts::failed(format!("REPLAY_INCOMPLETE: {e}")),
                None => {
                    return ReplayFacts::failed("REPLAY_INCOMPLETE: the judging waited".to_owned());
                }
            },
        }
    }
    r.lines = None;
    let driven = match crate::adapter::at_once(
        r.conclude(&mut sink, Some(crate::observed::exit_report(&observed))),
    ) {
        Some(Ok(driven)) => driven,
        Some(Err(e)) => return ReplayFacts::failed(format!("REPLAY_INCOMPLETE: {e}")),
        None => return ReplayFacts::failed("REPLAY_INCOMPLETE: the judging waited".to_owned()),
    };
    let mut sent = Vec::new();
    while let Ok(line) = written.try_recv() {
        sent.push(serde_json::from_slice(&line).unwrap_or(Value::Null));
    }
    ReplayFacts {
        state: driven.state,
        failure: driven.failure,
        final_text: driven.reply,
        native_session: driven.native_session,
        thread: None,
        turn: None,
        thread_result: None,
        recorded: sink.recorded,
        sent: Some(sent),
    }
}

/// Whether the session's file (`<timestamp>_<id>.jsonl`, as pi names it) is in its directory.
/// A session file as noted before a start: canonical path, device, inode.
pub type SessionFile = (PathBuf, u64, u64);

/// The identity of an existing file: its canonical path, device and inode (`None` if it does not resolve).
fn identity(path: &str) -> Option<SessionFile> {
    use std::os::unix::fs::MetadataExt;
    let canonical = std::path::Path::new(path).canonicalize().ok()?;
    let meta = std::fs::metadata(&canonical).ok()?;
    meta.is_file().then(|| (canonical, meta.dev(), meta.ino()))
}

/// The `*.jsonl` files of a session directory, as pi lists them (names ending in `.jsonl`; a missing directory has
/// none); `Err(name)` for an entry, symlinks followed, that would block pi's lookup or may (a FIFO, a device).
pub fn session_files(dir: &std::path::Path) -> Result<Vec<SessionFile>, String> {
    use std::os::unix::fs::FileTypeExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.as_encoded_bytes().ends_with(b".jsonl") {
            continue;
        }
        let path = entry.path();
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => files.extend(identity(&path.to_string_lossy())),
            // pi's open or read fails at once and it goes on: a directory (searchable or not), a socket (ENXIO), a
            // dangling or looping symlink, an entry it may not reach (M1T05R-15/21).
            Ok(meta) if meta.is_dir() || meta.file_type().is_socket() => {}
            Err(_) => {}
            // A FIFO blocks pi's open; a device may block or never end (HoH cannot tell which without reading it).
            Ok(_) => return Err(name.to_string_lossy().into_owned()),
        }
    }
    files.sort();
    Ok(files)
}

/// The reply and any provider error of a final assistant message; a later message without an error clears an earlier
/// one (a retry that worked).
fn note_message(message: &Value, reply: &mut Option<String>, error: &mut Option<String>) {
    if message["stopReason"] == "error" {
        *error = Some(
            message["errorMessage"]
                .as_str()
                .unwrap_or("provider error")
                .to_owned(),
        );
    } else {
        *error = None;
        *reply = Some(text_of(message));
    }
}

#[cfg(test)]
mod tests;
