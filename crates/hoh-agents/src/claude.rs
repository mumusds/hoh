//! The Claude Code adapter (REQ-025, docs/13-adapters.md "Claude"): `claude -p --output-format stream-json --verbose`
//! with the prompt on stdin, one process per turn, resumed turns through `--resume <session_id>`. Flags come from the
//! probed `claude --help` text, never from assumptions: partial messages only when wanted and offered, the YOLO
//! profile only with `--dangerously-skip-permissions` (and never as root), and HoH Auto is never mapped to
//! `--permission-mode auto`. stdout and stderr are kept as separate raw files with their sha256; stderr is diagnostic
//! only. The attempt moves through the store (PREPARED -> STARTING -> RUNNING -> SETTLED/FAILED/CANCELLED) and the
//! session row records Claude's own session id.
//!
//! An attempt SETTLES only on a `result` frame with `subtype=success` and `is_error=false` and exit code 0; a stream
//! that ends before its `result` frame is FAILED (PROTOCOL_INCOMPLETE) even with exit code 0; any other result is
//! FAILED with its subtype kept. A cancelled attempt is CANCELLED whatever follows (Claude still prints
//! `result(error_during_execution)` and exits 0 after SIGINT, CR-003); cancel sends SIGINT, then SIGTERM and SIGKILL to
//! the process group with graces between.

use crate::frame::Frame;
use crate::process::{Config, Escalation, Event, Process, Signal, Step, Termination};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{AttemptFacts, AttemptMove, PersistError, Session, Store};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

/// The YOLO permission flag; offered by the probed help or the profile is refused.
pub const YOLO_FLAG: &str = "--dangerously-skip-permissions";
/// Live deltas; added only when wanted and offered.
pub const PARTIAL_FLAG: &str = "--include-partial-messages";

/// How to start one turn.
#[derive(Debug, Clone)]
pub struct Launch {
    pub program: PathBuf,
    /// The probed `claude --help` output.
    pub help: String,
    pub cwd: PathBuf,
    /// Run with the agent's own permission bypass (HoH's YOLO runtime profile).
    pub yolo: bool,
    /// The caller wants live deltas.
    pub partial: bool,
    /// Claude's session id to continue.
    pub resume: Option<String>,
    /// Whether this process runs as root (Claude refuses the bypass as root).
    pub root: bool,
}

/// Why a turn was not started (`BLOCKED_CAPABILITY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked(pub String);

/// The command line of a turn and the capabilities it reveals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argv {
    pub args: Vec<String>,
    /// `partial_messages`, `resume`, `structured_events` as offered by the help.
    pub capabilities: Value,
}

fn offers(help: &str, flag: &str) -> bool {
    help.split(|c: char| c.is_whitespace() || c == ',' || c == '[' || c == ']' || c == '=')
        .any(|word| word == flag)
}

/// The argv of a turn, or why it cannot start.
pub fn argv(launch: &Launch) -> Result<Argv, Blocked> {
    if !offers(&launch.help, "--output-format") || !launch.help.contains("stream-json") {
        return Err(Blocked(
            "claude --help offers no --output-format stream-json (protocol incompatible)"
                .to_owned(),
        ));
    }
    let partial = offers(&launch.help, PARTIAL_FLAG);
    let resume = offers(&launch.help, "--resume");
    let mut args = vec!["-p", "--output-format", "stream-json", "--verbose"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if launch.partial && partial {
        args.push(PARTIAL_FLAG.to_owned());
    }
    if launch.yolo {
        if !offers(&launch.help, YOLO_FLAG) {
            return Err(Blocked(format!(
                "claude --help offers no {YOLO_FLAG} for the YOLO profile"
            )));
        }
        if launch.root {
            return Err(Blocked(format!(
                "{YOLO_FLAG} is refused as root; run HoH as a non-root user of the VM"
            )));
        }
        args.push(YOLO_FLAG.to_owned());
    }
    if let Some(id) = &launch.resume {
        if !resume {
            return Err(Blocked("claude --help offers no --resume".to_owned()));
        }
        args.push("--resume".to_owned());
        args.push(id.clone());
    }
    Ok(Argv {
        args,
        capabilities: json!({"structured_events": true, "resume": resume, "partial_messages": partial}),
    })
}

/// The attempt a turn runs as, and its session.
#[derive(Debug, Clone)]
pub struct AttemptRef {
    pub attempt_id: String,
    /// HoH's session id (`sessions.session_id`).
    pub session_id: String,
    pub role: String,
    pub agent_version: String,
}

/// A turn to run.
#[derive(Debug, Clone)]
pub struct Turn {
    pub launch: Launch,
    pub prompt: Vec<u8>,
    /// Directory for stdout.jsonl and stderr.log (created if needed).
    pub logs: PathBuf,
    /// native: SIGINT, then SIGTERM and SIGKILL to the group (docs/13: 10 s / 5 s initially).
    pub escalation: Escalation,
    pub deadline: Option<Duration>,
}

impl Turn {
    /// Claude's cancel plan with the initial graces.
    pub fn default_escalation() -> Escalation {
        Escalation {
            native: Some(Signal::Interrupt),
            native_grace: Duration::from_secs(10),
            term_grace: Duration::from_secs(5),
            ..Escalation::default()
        }
    }
}

/// The `result` frame's verdict fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResultFrame {
    pub subtype: Option<String>,
    pub is_error: Option<bool>,
    /// The final reply (`result`), as Claude reported it.
    pub text: Option<String>,
}

/// The end state and failure reason of a turn (pure; module docs).
pub fn verdict(
    cancelled: bool,
    too_large: bool,
    result: Option<&ResultFrame>,
    exit: Option<Termination>,
) -> (AttemptState, Option<String>) {
    if cancelled {
        return (AttemptState::Cancelled, None);
    }
    if too_large {
        return (
            AttemptState::Failed,
            Some("PROTOCOL_FRAME_TOO_LARGE".to_owned()),
        );
    }
    let Some(result) = result else {
        return (AttemptState::Failed, Some("PROTOCOL_INCOMPLETE".to_owned()));
    };
    let subtype = result.subtype.as_deref().unwrap_or("unknown");
    if subtype != "success" {
        return (AttemptState::Failed, Some(subtype.to_owned()));
    }
    if result.is_error != Some(false) {
        return (AttemptState::Failed, Some("is_error".to_owned()));
    }
    match exit {
        Some(Termination::Code(0)) => (AttemptState::Settled, None),
        Some(Termination::Code(code)) => (AttemptState::Failed, Some(format!("exit code {code}"))),
        Some(Termination::Signal(signal)) => {
            (AttemptState::Failed, Some(format!("signal {signal}")))
        }
        None => (AttemptState::Failed, Some("no exit status".to_owned())),
    }
}

/// What a turn did.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub state: AttemptState,
    pub failure: Option<String>,
    pub args: Vec<String>,
    /// Claude's session id (init frame).
    pub native_session: Option<String>,
    pub model: Option<String>,
    pub result: Option<ResultFrame>,
    pub exit: Option<Termination>,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
    /// Lines that were not JSON (each also an `adapter.diagnostic` event).
    pub diagnostics: usize,
    pub pid: Option<u32>,
    /// The signals sent to stop the process, with their times (a cancel or the deadline).
    pub steps: Vec<Step>,
}

/// Why a turn could not be recorded or run.
#[derive(Debug)]
pub enum AdapterError {
    Store(PersistError),
    Io(std::io::Error),
}

impl From<PersistError> for AdapterError {
    fn from(e: PersistError) -> Self {
        AdapterError::Store(e)
    }
}

impl From<std::io::Error> for AdapterError {
    fn from(e: std::io::Error) -> Self {
        AdapterError::Io(e)
    }
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Store(e) => write!(f, "store: {e}"),
            AdapterError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for AdapterError {}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn now() -> Option<String> {
    Some(hoh_core::time::now_rfc3339())
}

async fn step(
    store: &Store,
    attempt: &AttemptRef,
    from: AttemptState,
    to: AttemptState,
    facts: AttemptFacts,
) -> Result<(), PersistError> {
    store
        .move_attempt(AttemptMove {
            attempt_id: attempt.attempt_id.clone(),
            from,
            to,
            facts,
        })
        .await
}

/// Run one turn as `attempt` (created PREPARED by the caller). `cancel` turning true cancels it.
pub async fn run(
    store: &Store,
    attempt: &AttemptRef,
    turn: &Turn,
    mut cancel: watch::Receiver<bool>,
) -> Result<Outcome, AdapterError> {
    use AttemptState::*;
    std::fs::create_dir_all(&turn.logs)?;
    let (stdout, stderr) = (turn.logs.join("stdout.jsonl"), turn.logs.join("stderr.log"));
    let log_path = Some(turn.logs.to_string_lossy().into_owned());
    let argv = match argv(&turn.launch) {
        Ok(argv) => argv,
        Err(Blocked(detail)) => {
            // Nothing starts; the attempt ends where it is.
            step(
                store,
                attempt,
                Prepared,
                Failed,
                AttemptFacts {
                    ended_at: now(),
                    failure_reason: Some("BLOCKED_CAPABILITY".to_owned()),
                    ..AttemptFacts::default()
                },
            )
            .await?;
            store
                .attempt_event(
                    &attempt.attempt_id,
                    "blocked",
                    "adapter.blocked",
                    json!({"reason": "BLOCKED_CAPABILITY", "detail": detail}),
                )
                .await?;
            return Ok(Outcome {
                state: Failed,
                failure: Some("BLOCKED_CAPABILITY".to_owned()),
                args: Vec::new(),
                native_session: None,
                model: None,
                result: None,
                exit: None,
                stdout,
                stderr,
                stdout_sha256: String::new(),
                stderr_sha256: String::new(),
                diagnostics: 0,
                pid: None,
                steps: Vec::new(),
            });
        }
    };
    // The session row exists before the attempt names it.
    store
        .upsert_session(Session {
            session_id: attempt.session_id.clone(),
            agent: "claude".to_owned(),
            role: attempt.role.clone(),
            native_id: turn.launch.resume.clone(),
            native_path: None,
            agent_version: attempt.agent_version.clone(),
            capabilities: argv.capabilities.clone(),
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
            raw_log_path: log_path.clone(),
            ..AttemptFacts::default()
        },
    )
    .await?;
    let mut command = Command::new(&turn.launch.program);
    command
        .args(argv.args.iter().map(OsString::from))
        .current_dir(&turn.launch.cwd);
    let config = Config {
        escalation: turn.escalation,
        deadline: turn.deadline,
        stdout_copy: Some(stdout.clone()),
        stderr_copy: Some(stderr.clone()),
        ..Config::default()
    };
    let mut process = match Process::spawn(command, config) {
        Ok(p) => p,
        Err(e) => {
            step(
                store,
                attempt,
                Starting,
                Failed,
                AttemptFacts {
                    ended_at: now(),
                    failure_reason: Some(format!("spawn: {e}")),
                    ..AttemptFacts::default()
                },
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
        AttemptFacts {
            pid: Some(i64::from(pid)),
            ..AttemptFacts::default()
        },
    )
    .await?;
    if let Some(mut input) = process.take_stdin() {
        // A child that exits before reading its prompt is judged by what it printed.
        let _ = input.write_all(&turn.prompt).await;
        let _ = input.shutdown().await;
    }
    let mut native_session = None;
    let mut model = None;
    let mut result: Option<ResultFrame> = None;
    let mut usage = None;
    let mut cost = None;
    let mut diagnostics = 0;
    let mut too_large = false;
    let mut cancelled = *cancel.borrow();
    if cancelled {
        process.cancel(turn.escalation);
    }
    let report = loop {
        let event = tokio::select! {
            event = process.next() => event,
            changed = cancel.changed(), if !cancelled => {
                if changed.is_ok() && *cancel.borrow() {
                    cancelled = true;
                    process.cancel(turn.escalation);
                }
                continue;
            }
        };
        let line = match event {
            Some(Event::Frame(Frame::Line(bytes) | Frame::Partial(bytes))) => bytes,
            Some(Event::Frame(Frame::TooLarge(_))) => {
                too_large = true;
                continue;
            }
            Some(Event::Exit(report)) => break Some(*report),
            None => break None,
        };
        let Ok(frame) = serde_json::from_slice::<Value>(&line) else {
            diagnostics += 1;
            store
                .attempt_event(
                    &attempt.attempt_id,
                    &format!("diagnostic-{diagnostics}"),
                    "adapter.diagnostic",
                    json!({
                        "reason": "stdout line is not JSON",
                        "bytes": line.len(),
                        "head": String::from_utf8_lossy(&line[..line.len().min(256)]),
                    }),
                )
                .await?;
            continue;
        };
        match frame["type"].as_str() {
            Some("system") if frame["subtype"] == "init" => {
                native_session = frame["session_id"].as_str().map(str::to_owned);
                model = frame["model"].as_str().map(str::to_owned);
                store
                    .upsert_session(Session {
                        session_id: attempt.session_id.clone(),
                        agent: "claude".to_owned(),
                        role: attempt.role.clone(),
                        native_id: native_session.clone(),
                        native_path: None,
                        agent_version: attempt.agent_version.clone(),
                        capabilities: argv.capabilities.clone(),
                    })
                    .await?;
            }
            Some("result") => {
                result = Some(ResultFrame {
                    subtype: frame["subtype"].as_str().map(str::to_owned),
                    is_error: frame["is_error"].as_bool(),
                    text: frame["result"].as_str().map(str::to_owned),
                });
                usage = frame.get("usage").filter(|u| u.is_object()).cloned();
                cost = frame["total_cost_usd"].as_f64();
            }
            _ => {}
        }
    };
    let exit = report.as_ref().and_then(|r| r.status);
    let (state, failure) = verdict(cancelled, too_large, result.as_ref(), exit);
    step(
        store,
        attempt,
        Running,
        state,
        AttemptFacts {
            ended_at: now(),
            resolved_model: model.clone(),
            failure_reason: failure.clone(),
            cancel_requested: Some(cancelled),
            usage,
            cost_usd: cost,
            ..AttemptFacts::default()
        },
    )
    .await?;
    Ok(Outcome {
        state,
        failure,
        args: argv.args,
        native_session,
        model,
        result,
        exit,
        stdout_sha256: sha256_file(&stdout)?,
        stderr_sha256: sha256_file(&stderr)?,
        stdout,
        stderr,
        diagnostics,
        pid: Some(pid),
        steps: report.map(|r| r.steps).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests;
