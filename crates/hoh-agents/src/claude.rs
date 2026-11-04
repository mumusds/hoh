//! The Claude Code adapter (REQ-025, docs/13-adapters.md "Claude"): `claude -p --output-format stream-json --verbose`
//! with the prompt on stdin, one process per turn, resumed turns through `--resume <session_id>`. Flags come from the
//! probed `claude --help` text, never from assumptions: partial messages only when wanted and offered, the YOLO
//! profile only with `--dangerously-skip-permissions` (and never as root), and HoH Auto is never mapped to
//! `--permission-mode auto`. stdout and stderr are kept as separate raw files with their sha256; stderr is diagnostic
//! only. The attempt moves through the store (PREPARED -> STARTING -> RUNNING -> SETTLED/FAILED/CANCELLED) and the
//! session row records Claude's own session id.
//!
//! An attempt SETTLES only on a `result` frame with `subtype=success` and `is_error=false` and exit code 0, with its
//! prompt delivered whole and both raw logs complete; a stream that ends before its `result` frame is FAILED
//! (PROTOCOL_INCOMPLETE) even with exit code 0; any other result is FAILED with its subtype kept (also in the
//! `adapter.result` event), or BLOCKED_AUTH when an assistant frame carried Claude's `authentication_failed` error
//! (recorded case `unauthenticated`). A cancel requested before the end is recorded makes the attempt CANCELLED
//! whatever follows (Claude still prints `result(error_during_execution)` and exits 0 after SIGINT, CR-003): every end
//! is committed through the attempt's [`CancelLatch`], so a cancel accepted before that commit wins; a refused
//! frame (PROTOCOL_FRAME_TOO_LARGE) and the deadline (DEADLINE_EXCEEDED) stop the process and fail the turn whatever it
//! prints after. Stopping sends SIGINT, then SIGTERM and SIGKILL to the process group with graces between. Stdin,
//! stdout and the cancel are driven together, so a child that never reads its prompt cannot hold up a cancel.

pub use crate::adapter::{AdapterError, AttemptRef};
use crate::adapter::{finish, now, sha256_file, step};
use crate::frame::Frame;
use crate::process::{Cause, Config, Escalation, Event, Process, Signal, Step, Termination};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{AttemptFacts, CancelLatch, Session, Store};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

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

/// What the end of a turn is judged on (see [`verdict`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Ending<'a> {
    /// The caller asked to cancel before the end was recorded.
    pub cancelled: bool,
    /// A stdout frame was over the frame limit.
    pub too_large: bool,
    /// The turn's deadline stopped the process.
    pub deadline: bool,
    /// A raw log misses bytes: a copy failed or a stream never reached its end.
    pub logs_incomplete: bool,
    /// The prompt could not be written whole to stdin.
    pub prompt_undelivered: bool,
    /// An assistant frame carried Claude's `authentication_failed` error.
    pub auth_failed: bool,
    pub result: Option<&'a ResultFrame>,
    pub exit: Option<Termination>,
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
    let Some(result) = e.result else {
        return failed("PROTOCOL_INCOMPLETE");
    };
    match judge(result, e.exit) {
        (AttemptState::Settled, _) if e.prompt_undelivered => failed("PROMPT_NOT_DELIVERED"),
        (AttemptState::Failed, _) if e.auth_failed => failed("BLOCKED_AUTH"),
        judged => judged,
    }
}

/// The `result` frame and the exit code alone.
fn judge(result: &ResultFrame, exit: Option<Termination>) -> (AttemptState, Option<String>) {
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

/// Run one turn as `attempt` (created PREPARED by the caller); a cancel accepted by `cancel` cancels it.
pub async fn run(
    store: &Store,
    attempt: &AttemptRef,
    turn: &Turn,
    cancel: &CancelLatch,
) -> Result<Outcome, AdapterError> {
    use AttemptState::*;
    let (stdout, stderr) = (turn.logs.join("stdout.jsonl"), turn.logs.join("stderr.log"));
    let log_path = Some(turn.logs.to_string_lossy().into_owned());
    let argv = match argv(&turn.launch) {
        Ok(argv) => argv,
        Err(Blocked(detail)) => {
            // Nothing starts; the attempt ends where it is.
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
                    json!({"reason": "BLOCKED_CAPABILITY", "detail": detail}),
                )
                .await?;
            return Ok(Outcome {
                state,
                failure: (state == Failed).then(|| "BLOCKED_CAPABILITY".to_owned()),
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
    if let Err(e) = std::fs::create_dir_all(&turn.logs) {
        // Nothing starts without its raw logs; the attempt does not stay PREPARED.
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
        AttemptFacts {
            pid: Some(i64::from(pid)),
            ..AttemptFacts::default()
        },
    )
    .await?;
    // The prompt is written while the output and the cancel are watched; the pipe closes when it is written.
    let input = process.take_stdin();
    let mut feed = std::pin::pin!(async {
        let mut input = input.ok_or_else(|| std::io::Error::other("no stdin pipe"))?;
        input.write_all(&turn.prompt).await?;
        input.shutdown().await
    });
    let mut fed: Option<std::io::Result<()>> = None;
    let mut native_session = None;
    let mut model = None;
    let mut result: Option<ResultFrame> = None;
    let mut results = 0;
    let mut usage = None;
    let mut cost = None;
    let mut diagnostics = 0;
    let mut too_large = false;
    let mut auth_failed = false;
    let mut cancelled = cancel.is_requested();
    if cancelled {
        process.cancel(turn.escalation);
    }
    let report = loop {
        // A cancel is seen before output that is ready at the same time.
        let event = tokio::select! {
            biased;
            () = cancel.requested(), if !cancelled => {
                cancelled = true;
                process.cancel(turn.escalation);
                continue;
            }
            written = &mut feed, if fed.is_none() => {
                fed = Some(written);
                continue;
            }
            event = process.next() => event,
        };
        let line = match event {
            Some(Event::Frame(Frame::Line(bytes) | Frame::Partial(bytes))) => bytes,
            Some(Event::Frame(Frame::TooLarge(refused))) => {
                // Nothing more is framed: end the process rather than wait for it.
                too_large = true;
                process.cancel(turn.escalation);
                store
                    .attempt_event(
                        &attempt.attempt_id,
                        "frame-too-large",
                        "adapter.diagnostic",
                        json!({
                            "reason": "PROTOCOL_FRAME_TOO_LARGE",
                            "length": refused.length,
                            "prefix_sha256": refused.prefix_sha256,
                        }),
                    )
                    .await?;
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
            Some("assistant") if frame["error"] == "authentication_failed" => auth_failed = true,
            Some("result") => {
                let frame_result = ResultFrame {
                    subtype: frame["subtype"].as_str().map(str::to_owned),
                    is_error: frame["is_error"].as_bool(),
                    text: frame["result"].as_str().map(str::to_owned),
                };
                usage = frame.get("usage").filter(|u| u.is_object()).cloned();
                cost = frame["total_cost_usd"].as_f64();
                results += 1;
                // Kept whatever the end: after a cancel it is only a diagnostic (CR-003).
                store
                    .attempt_event(
                        &attempt.attempt_id,
                        &format!("result-{results}"),
                        "adapter.result",
                        json!({
                            "subtype": frame_result.subtype,
                            "is_error": frame_result.is_error,
                            "terminal_reason": frame["terminal_reason"],
                            "after_cancel": cancelled,
                        }),
                    )
                    .await?;
                result = Some(frame_result);
            }
            _ => {}
        }
    };
    let logs_incomplete = report.as_ref().is_none_or(|r| {
        r.stdout.error.is_some() || r.stderr.error.is_some() || !r.stdout.eof || !r.stderr.eof
    });
    if logs_incomplete && let Some(r) = &report {
        store
            .attempt_event(
                &attempt.attempt_id,
                "raw-log-incomplete",
                "adapter.diagnostic",
                json!({
                    "reason": "RAW_LOG_INCOMPLETE",
                    "stdout": {"eof": r.stdout.eof, "error": r.stdout.error},
                    "stderr": {"eof": r.stderr.eof, "error": r.stderr.error},
                }),
            )
            .await?;
    }
    let exit = report.as_ref().and_then(|r| r.status);
    // A cancel accepted from here on still wins: the store commits the end through the latch (M1T03R-03, M1T03R-10).
    let (state, failure) = verdict(&Ending {
        cancelled,
        too_large,
        deadline: report
            .as_ref()
            .is_some_and(|r| r.cause == Cause::DeadlineExceeded),
        logs_incomplete,
        prompt_undelivered: !matches!(fed, Some(Ok(()))),
        auth_failed,
        result: result.as_ref(),
        exit,
    });
    let state = finish(
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
        cancel,
    )
    .await?;
    let failure = if state == Cancelled { None } else { failure };
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
