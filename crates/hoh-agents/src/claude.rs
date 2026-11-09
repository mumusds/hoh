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

pub use crate::adapter::{AdapterError, AttemptRef, Facts, GateCommand, Held, Recorded, Refused};
use crate::adapter::{Recorder, Stop, finish, now, offers, sha256_file, step};
use crate::frame::Frame;
use crate::known;
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
    /// The model to run (`--model`, which the help must offer); `None`: Claude's own default.
    pub model: Option<String>,
    /// Whether this process runs as root (Claude refuses the bypass as root).
    pub root: bool,
    /// What the probe found for this claude (its compat manifest entry); the session row records them, never more
    /// than this help offers (REQ-027).
    pub capabilities: crate::capabilities::Probed,
}

/// Why a turn was not started (`BLOCKED_CAPABILITY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked(pub String);

/// The command line of a turn and the capabilities its session has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argv {
    pub args: Vec<String>,
    /// The session row's capabilities: the probed ones this adapter implements, `partial_messages` and `resume` no
    /// more than the help offers (M1R-01).
    pub capabilities: Value,
}

/// Why this claude's help gives the adapter no way to run a turn at all, or `None`: every turn passes `-p`,
/// `--output-format stream-json` and `--verbose`, so each must be declared, `stream-json` among `--output-format`'s
/// own values. The probe applies the same rule, so doctor never calls a claude compatible that the adapter cannot
/// drive (M1R-03, M1T07R-14).
pub fn interface_missing(help: &str) -> Option<String> {
    if let Some(flag) = ["-p", "--verbose"].into_iter().find(|f| !offers(help, f)) {
        return Some(format!(
            "claude --help offers no {flag} (protocol incompatible)"
        ));
    }
    (!crate::adapter::option_text(help, "--output-format")
        .is_some_and(|t| crate::adapter::has_word(&t, "stream-json")))
    .then(|| {
        "claude --help offers no --output-format stream-json (protocol incompatible)".to_owned()
    })
}

/// The argv of a turn, or why it cannot start.
pub fn argv(launch: &Launch) -> Result<Argv, Blocked> {
    if let Some(why) = interface_missing(&launch.help) {
        return Err(Blocked(why));
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
    if let Some(model) = &launch.model {
        if !offers(&launch.help, "--model") {
            return Err(Blocked("claude --help offers no --model".to_owned()));
        }
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    Ok(Argv {
        args,
        // What this adapter implements: events, the result's token usage, and resume and deltas where the help offers
        // them. One prompt on stdin, then stdin closes, so no steer and no dialog (M1T04R-12, -15, -17).
        capabilities: crate::capabilities::of_session(
            &launch.capabilities,
            &[
                ("structured_events", true),
                ("token_usage", true),
                ("resume", resume),
                ("partial_messages", partial),
            ],
        ),
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
    /// The most stdout bytes the frame reader held at once (AC-027-03's probe; at most the frame limit and one read
    /// chunk); `None` when no process was read.
    pub max_buffered: Option<usize>,
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
                max_buffered: None,
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
    let mut process = match Process::spawn(command, config(turn, &stdout, &stderr)) {
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
    let mut recorder = Recorder::Store {
        store,
        attempt,
        agent: "claude",
        capabilities: &argv.capabilities,
    };
    let driven = drive(
        &mut recorder,
        &attempt.attempt_id,
        &mut process,
        turn.prompt.clone(),
        turn.escalation,
        Stop::Latch(cancel),
    )
    .await?;
    // A cancel accepted from here on still wins: the store commits the end through the latch (M1T03R-03, M1T03R-10).
    let (state, failure) = driven.verdict();
    let state = finish(
        store,
        attempt,
        Running,
        state,
        AttemptFacts {
            ended_at: now(),
            resolved_model: driven.model.clone(),
            failure_reason: failure.clone(),
            cancel_requested: Some(driven.cancelled),
            usage: driven.usage.clone(),
            cost_usd: driven.cost,
            ..AttemptFacts::default()
        },
        cancel,
    )
    .await?;
    let failure = if state == Cancelled { None } else { failure };
    let exit = driven.report.as_ref().and_then(|r| r.status);
    Ok(Outcome {
        state,
        failure,
        args: argv.args,
        native_session: driven.native_session,
        model: driven.model,
        result: driven.result,
        exit,
        stdout_sha256: sha256_file(&stdout)?,
        stderr_sha256: sha256_file(&stderr)?,
        stdout,
        stderr,
        diagnostics: driven.diagnostics,
        pid: Some(pid),
        max_buffered: driven.report.as_ref().map(|r| r.stdout.max_buffered),
        steps: driven.report.map(|r| r.steps).unwrap_or_default(),
    })
}

/// The process settings of `turn`: its escalation, its deadline and its raw logs.
fn config(turn: &Turn, stdout: &std::path::Path, stderr: &std::path::Path) -> Config {
    Config {
        escalation: turn.escalation,
        deadline: turn.deadline,
        stdout_copy: Some(stdout.to_path_buf()),
        stderr_copy: Some(stderr.to_path_buf()),
        ..Config::default()
    }
}

/// Start one turn in the runner's mode (docs/adr/ADR-022.md decisions 12 to 14): through `gate`, held there until
/// [`Held::open`] is sent, then the prompt on stdin; no attempt state moves and nothing is written to the store, the
/// facts come back for the runner to record. A cancel is native only (SIGINT): the host's ladder decides SIGTERM and
/// SIGKILL. No deadline: the runner's lease bounds the turn.
pub fn start(attempt: &AttemptRef, turn: &Turn, gate: &GateCommand) -> Result<Held, Refused> {
    let argv = argv(&turn.launch).map_err(|Blocked(detail)| Refused {
        code: "BLOCKED_CAPABILITY".to_owned(),
        detail,
    })?;
    std::fs::create_dir_all(&turn.logs).map_err(|e| Refused {
        code: "LOGS".to_owned(),
        detail: e.to_string(),
    })?;
    let (stdout, stderr) = (turn.logs.join("stdout.jsonl"), turn.logs.join("stderr.log"));
    let mut command = Command::new(&gate.program);
    command
        .args(&gate.args)
        .arg(&turn.launch.program)
        .args(argv.args.iter().map(OsString::from))
        .envs(gate.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&turn.launch.cwd);
    let config = Config {
        deadline: None,
        ..config(turn, &stdout, &stderr)
    };
    let mut process = Process::spawn(command, config).map_err(|e| Refused {
        code: "SPAWN".to_owned(),
        detail: e.to_string(),
    })?;
    let pid = process.pid();
    let (open, opened) = tokio::sync::oneshot::channel::<()>();
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let native = cancel.clone();
    let prompt = turn.prompt.clone();
    let escalation = turn.escalation;
    let raw_log_path = turn.logs.to_string_lossy().into_owned();
    let attempt_id = attempt.attempt_id.clone();
    let ended = tokio::spawn(async move {
        // Nothing reaches the gate before it is opened; unopened, its input ends and the agent never runs.
        if opened.await.is_err() {
            let report = crate::adapter::close_gate(&mut process).await;
            return Facts::gate_closed(raw_log_path, report.as_ref());
        }
        let mut input = b"go\n".to_vec();
        input.extend_from_slice(&prompt);
        let mut recorder = Recorder::Collect(Vec::new());
        let driven = drive(
            &mut recorder,
            &attempt_id,
            &mut process,
            input,
            escalation,
            Stop::Native(&native),
        )
        .await;
        let recorded = match recorder {
            Recorder::Collect(recorded) => recorded,
            Recorder::Store { .. } => Vec::new(),
        };
        let Ok(driven) = driven else {
            return Facts::incomplete(raw_log_path, recorded);
        };
        let (state, failure) = driven.verdict();
        Facts {
            state,
            failure,
            native_session: driven.native_session,
            resolved_model: driven.model,
            usage: driven.usage,
            raw_log_path,
            max_buffered: driven.report.as_ref().map(|r| r.stdout.max_buffered),
            group_reaped: driven.report.as_ref().is_some_and(|r| r.group_reaped),
            recorded,
            degraded: None,
        }
    });
    Ok(Held {
        pid,
        open,
        cancel,
        ended,
    })
}

/// What a cancel does to a Claude turn: the turn's plan in the direct mode, one SIGINT in the runner's.
fn act(stop: &Stop<'_>, process: &Process, escalation: Escalation) {
    if stop.is_native() {
        process.signal_leader(Signal::Interrupt);
    } else {
        process.cancel(escalation);
    }
}

/// What a turn's output and end showed.
struct Driven {
    native_session: Option<String>,
    model: Option<String>,
    result: Option<ResultFrame>,
    usage: Option<Value>,
    cost: Option<f64>,
    diagnostics: usize,
    too_large: bool,
    auth_failed: bool,
    cancelled: bool,
    fed: Option<std::io::Result<()>>,
    report: Option<crate::process::ExitReport>,
}

impl Driven {
    fn verdict(&self) -> (AttemptState, Option<String>) {
        let report = self.report.as_ref();
        let logs_incomplete = report.is_none_or(|r| {
            r.stdout.error.is_some() || r.stderr.error.is_some() || !r.stdout.eof || !r.stderr.eof
        });
        verdict(&Ending {
            cancelled: self.cancelled,
            too_large: self.too_large,
            deadline: report.is_some_and(|r| r.cause == Cause::DeadlineExceeded),
            logs_incomplete,
            prompt_undelivered: !matches!(self.fed, Some(Ok(()))),
            auth_failed: self.auth_failed,
            result: self.result.as_ref(),
            exit: report.and_then(|r| r.status),
        })
    }
}

/// Write `input` and read the turn's output until its process ended, recording what it shows; a cancel from `stop`
/// is acted on as it comes.
async fn drive(
    recorder: &mut Recorder<'_>,
    attempt_id: &str,
    process: &mut Process,
    input: Vec<u8>,
    escalation: Escalation,
    stop: Stop<'_>,
) -> Result<Driven, AdapterError> {
    // The input is written while the output and the cancel are watched; the pipe closes when it is written.
    let pipe = process.take_stdin();
    let mut feed = std::pin::pin!(async {
        let mut pipe = pipe.ok_or_else(|| std::io::Error::other("no stdin pipe"))?;
        pipe.write_all(&input).await?;
        pipe.shutdown().await
    });
    let mut driven = Driven {
        native_session: None,
        model: None,
        result: None,
        usage: None,
        cost: None,
        diagnostics: 0,
        too_large: false,
        auth_failed: false,
        cancelled: stop.requested_now(),
        fed: None,
        report: None,
    };
    let mut results = 0;
    let mut unknown_records = 0;
    if driven.cancelled {
        act(&stop, process, escalation);
    }
    driven.report = loop {
        // A cancel is seen before output that is ready at the same time.
        let event = tokio::select! {
            biased;
            () = stop.requested(), if !driven.cancelled => {
                driven.cancelled = true;
                act(&stop, process, escalation);
                continue;
            }
            written = &mut feed, if driven.fed.is_none() => {
                driven.fed = Some(written);
                continue;
            }
            event = process.next() => event,
        };
        let line = match event {
            Some(Event::Frame(Frame::Line(bytes) | Frame::Partial(bytes))) => bytes,
            Some(Event::Frame(Frame::TooLarge(refused))) => {
                // Nothing more is framed: end the process rather than wait for it.
                driven.too_large = true;
                process.cancel(escalation);
                recorder
                    .event(
                        "frame-too-large",
                        "adapter.diagnostic",
                        json!({
                            "reason": "PROTOCOL_FRAME_TOO_LARGE",
                            "length": refused.length,
                            "prefix_sha256": refused.prefix_sha256,
                            "head": String::from_utf8_lossy(&refused.head),
                        }),
                    )
                    .await?;
                continue;
            }
            Some(Event::Exit(report)) => break Some(*report),
            None => break None,
        };
        let Ok(frame) = serde_json::from_slice::<Value>(&line) else {
            driven.diagnostics += 1;
            recorder
                .event(
                    &format!("diagnostic-{}", driven.diagnostics),
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
        let key = known::claude_key(&frame);
        if !known::is_known(known::claude(), key.as_deref()) {
            unknown_records += 1;
            recorder
                .event(
                    &format!("unknown-{unknown_records}"),
                    "adapter.unknown_event",
                    known::unknown_event(attempt_id, key.as_deref(), &line, &frame),
                )
                .await?;
        }
        match frame["type"].as_str() {
            Some("system") if frame["subtype"] == "init" => {
                driven.native_session = frame["session_id"].as_str().map(str::to_owned);
                driven.model = frame["model"].as_str().map(str::to_owned);
                if let Some(native) = &driven.native_session {
                    recorder.native_session(native).await?;
                }
            }
            Some("assistant") if frame["error"] == "authentication_failed" => {
                driven.auth_failed = true;
            }
            Some("result") => {
                let frame_result = ResultFrame {
                    subtype: frame["subtype"].as_str().map(str::to_owned),
                    is_error: frame["is_error"].as_bool(),
                    text: frame["result"].as_str().map(str::to_owned),
                };
                driven.usage = frame.get("usage").filter(|u| u.is_object()).cloned();
                driven.cost = frame["total_cost_usd"].as_f64();
                results += 1;
                // Kept whatever the end: after a cancel it is only a diagnostic (CR-003).
                recorder
                    .event(
                        &format!("result-{results}"),
                        "adapter.result",
                        json!({
                            "subtype": frame_result.subtype,
                            "is_error": frame_result.is_error,
                            "terminal_reason": frame["terminal_reason"],
                            "after_cancel": driven.cancelled,
                        }),
                    )
                    .await?;
                driven.result = Some(frame_result);
            }
            _ => {}
        }
    };
    if let Some(r) = &driven.report
        && (r.stdout.error.is_some() || r.stderr.error.is_some() || !r.stdout.eof || !r.stderr.eof)
    {
        recorder
            .event(
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
    Ok(driven)
}

#[cfg(test)]
mod tests;
