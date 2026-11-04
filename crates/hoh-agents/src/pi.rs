//! The Pi adapter (REQ-026, docs/13-adapters.md "Pi"): `pi --mode rpc` on a persistent session, one process per
//! attempt.
//!
//! The session is HoH's: `--session-dir` (under `.hoh/`, fixed per session) and `--session-id` are the same on every
//! turn of a session, and `--no-session` is never used; the directory is the session's native path. A resumed turn
//! needs the session's file in its directory — Pi would otherwise silently start a new session under that id — and
//! the session id `get_state` reports must be the one asked for (SESSION_NOT_FOUND / SESSION_MISMATCH, nothing new
//! made up). stdout is framed by the process substrate on LF bytes only (one CR before it dropped, U+2028/U+2029 kept
//! inside frames, 1 MiB per frame) and read from the start, stderr is drained alongside; every record goes to the
//! turn's subscriber, if any.
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

use crate::adapter::{AdapterError, AttemptRef, finish, now, offers, sha256_file, step};
use crate::clock::{Clock, Timer};
use crate::frame::Frame;
use crate::process::{Config, Escalation, Event, Process, Step, Termination};
use hoh_core::lifecycle::{AttemptState, InputState};
use hoh_core::persistence::{AttemptFacts, CancelLatch, InputMove, PersistError, Session, Store};
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
}

/// Why a turn was not started (`BLOCKED_CAPABILITY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked(pub String);

/// The arguments of `pi --mode rpc` on the session (never `--no-session`).
pub fn args(launch: &Launch) -> Result<Vec<String>, Blocked> {
    for flag in ["--mode", "--session-dir", "--session-id"] {
        if !offers(&launch.help, flag) {
            return Err(Blocked(format!("pi --help offers no {flag}")));
        }
    }
    let mut args = vec![
        "--mode".to_owned(),
        "rpc".to_owned(),
        "--session-dir".to_owned(),
        launch.session_dir.to_string_lossy().into_owned(),
        "--session-id".to_owned(),
        launch.session_id.clone(),
    ];
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

/// The failure reason of a refused prompt (docs/13, pi 1.5.0 has no structured error codes).
pub fn refusal(error: &str) -> &'static str {
    if error.starts_with("No API key found") {
        "BLOCKED_AUTH"
    } else if error.starts_with("Model not found") {
        "MODEL_UNAVAILABLE"
    } else {
        "PERMANENT_EXTERNAL"
    }
}

/// A turn to run.
#[derive(Debug, Clone)]
pub struct Turn {
    pub launch: Launch,
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
    };
    // Nothing starts without its capability, or for a session whose file is gone.
    let refused = match args(launch) {
        Err(Blocked(detail)) => Some(("BLOCKED_CAPABILITY", detail)),
        Ok(_) if launch.resume && !session_file_exists(launch) => Some((
            "SESSION_NOT_FOUND",
            format!(
                "no file of session {} in {}",
                launch.session_id,
                launch.session_dir.display()
            ),
        )),
        Ok(_) => None,
    };
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
            capabilities: json!({
                "structured_events": true,
                "resume": true,
                "mid_turn_steer": false,
                "ui_dialogs": false,
                "agent_settled": launch.settled_event,
            }),
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
    let (lines, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
    let input = process.take_stdin();
    let mut writer = tokio::spawn(async move {
        let mut input = input.ok_or_else(|| std::io::Error::other("no stdin pipe"))?;
        while let Some(line) = queue.recv().await {
            input.write_all(&line).await?;
        }
        input.shutdown().await
    });
    let mut lines = Some(lines);
    let send = |lines: &Option<mpsc::UnboundedSender<Vec<u8>>>, message: Value| {
        if let Some(l) = lines {
            let mut line = message.to_string().into_bytes();
            line.push(b'\n');
            let _ = l.send(line);
        }
    };
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut next = 0usize;
    let mut command_id = |prefix: &str| {
        next += 1;
        format!("hoh-{prefix}-{next}")
    };
    let state_id = command_id("state");
    pending.insert(state_id.clone(), Pending::Session);
    send(&lines, json!({"id": state_id, "type": "get_state"}));
    let mut cancelled = cancel.is_requested();
    let (mut deadline, mut too_large, mut tool_started, mut aborted) = (false, false, false, false);
    let (mut failure, mut settled, mut disposition) =
        (None::<String>, None::<SettledBy>, None::<String>);
    let (mut native_session, mut model, mut reply) =
        (None::<String>, None::<String>, None::<String>);
    let (mut parse_errors, mut state_queries, mut agent_starts, mut dialogs, mut data) =
        (0, 0, 0, 0, 0);
    let (mut prompt_sent, mut stopped, mut abort_sent, mut written) = (false, false, false, false);
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
    macro_rules! close {
        () => {
            lines = None;
            if exit_timer.is_none() {
                exit_timer = Some(Box::pin(tokio::time::sleep(turn.escalation.term_grace)));
            }
        };
    }
    macro_rules! abort {
        () => {
            if prompt_sent && settled.is_none() && lines.is_some() && !abort_sent {
                let abort_id = command_id("abort");
                pending.insert(abort_id.clone(), Pending::Abort);
                send(&lines, json!({"id": abort_id, "type": "abort"}));
                abort_sent = true;
                abort_timer = Some(turn.clock.sleep(turn.escalation.native_grace));
            } else if !stopped {
                lines = None;
                hard_stop(&process);
                stopped = true;
            }
        };
    }
    if cancelled {
        abort!();
    }
    let report = loop {
        let event = tokio::select! {
            biased;
            () = cancel.requested(), if !cancelled => {
                cancelled = true;
                abort!();
                continue;
            }
            () = fire(&mut deadline_timer), if deadline_timer.is_some() && !deadline => {
                deadline = true;
                deadline_timer = None;
                abort!();
                continue;
            }
            () = fire(&mut abort_timer), if abort_timer.is_some() && !stopped => {
                abort_timer = None;
                lines = None;
                hard_stop(&process);
                stopped = true;
                continue;
            }
            () = async { if let Some(t) = exit_timer.as_mut() { t.await } }, if exit_timer.is_some() && !stopped => {
                exit_timer = None;
                hard_stop(&process);
                stopped = true;
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
                if let Some(e) = failed.filter(|_| lines.is_some()) {
                    store
                        .attempt_event(id, "stdin-failed", "adapter.diagnostic",
                                       json!({"attempt_id": id, "reason": "stdin failed", "error": e}))
                        .await?;
                    lines = None;
                    hard_stop(&process);
                    stopped = true;
                }
                continue;
            }
            event = process.next() => event,
        };
        let bytes = match event {
            Some(Event::Frame(Frame::Line(bytes) | Frame::Partial(bytes))) => bytes,
            Some(Event::Frame(Frame::TooLarge(refused))) => {
                too_large = true;
                lines = None;
                hard_stop(&process);
                stopped = true;
                store
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
        let Some(record) = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .filter(Value::is_object)
        else {
            parse_errors += 1;
            store
                .attempt_event(
                    id,
                    &format!("diagnostic-{parse_errors}"),
                    "adapter.diagnostic",
                    json!({"attempt_id": id, "reason": "stdout line is not a JSON object", "bytes": bytes.len(),
                           "head": String::from_utf8_lossy(&bytes[..bytes.len().min(256)])}),
                )
                .await?;
            continue;
        };
        if let Some(events) = &turn.events {
            let _ = events.send(record.clone());
        }
        match record["type"].as_str().unwrap_or_default() {
            "response" => {
                let what = record["id"].as_str().and_then(|i| pending.remove(i));
                let ok = record["success"] == true;
                let error = record["error"].as_str().unwrap_or_default().to_owned();
                match what {
                    Some(Pending::Session) if ok => {
                        native_session = record["data"]["sessionId"].as_str().map(str::to_owned);
                        model = record["data"]["model"]["id"].as_str().map(str::to_owned);
                        if native_session.as_deref() != Some(launch.session_id.as_str()) {
                            failure = Some("SESSION_MISMATCH".to_owned());
                            store
                                .attempt_event(id, "session-mismatch", "adapter.diagnostic",
                                               json!({"attempt_id": id, "reason": "SESSION_MISMATCH",
                                                      "asked": launch.session_id, "got": native_session}))
                                .await?;
                            close!();
                        } else if lines.is_some() {
                            let prompt_id = command_id("prompt");
                            pending.insert(prompt_id.clone(), Pending::Prompt);
                            send(
                                &lines,
                                json!({"id": prompt_id, "type": "prompt", "message": turn.prompt}),
                            );
                            prompt_sent = true;
                        }
                    }
                    Some(Pending::Session) => {
                        failure = Some(format!("get_state: {error}"));
                        close!();
                    }
                    Some(Pending::Prompt) if ok => {
                        disposition = record["data"]["disposition"].as_str().map(str::to_owned);
                        if let Some(input) = &turn.input_id {
                            let to = InputState::Delivered;
                            move_input(store, input, InputState::Queued, to, None).await?;
                        }
                        if disposition.as_deref() == Some("handled") {
                            settled = Some(SettledBy::Handled);
                            if let Some(input) = &turn.input_id {
                                move_input(
                                    store,
                                    input,
                                    InputState::Delivered,
                                    InputState::Applied,
                                    None,
                                )
                                .await?;
                            }
                            close!();
                        }
                    }
                    Some(Pending::Prompt) => {
                        let reason = refusal(&error);
                        failure = Some(reason.to_owned());
                        store
                            .attempt_event(
                                id,
                                "prompt-refused",
                                "adapter.prompt_refused",
                                json!({"attempt_id": id, "reason": reason, "error": error}),
                            )
                            .await?;
                        if let Some(input) = &turn.input_id {
                            move_input(
                                store,
                                input,
                                InputState::Queued,
                                InputState::Rejected,
                                Some(error),
                            )
                            .await?;
                        }
                        close!();
                    }
                    Some(Pending::Idle) => {
                        let d = &record["data"];
                        let idle = ok
                            && d["isStreaming"] == false
                            && d["isCompacting"] == false
                            && d["pendingMessageCount"].as_u64() == Some(0);
                        if idle && settled.is_none() {
                            settled = Some(SettledBy::State);
                            close!();
                        }
                    }
                    // Pi answers an abort once it is idle; its agent_settled came first.
                    Some(Pending::Abort) if settled.is_some() => {
                        close!();
                    }
                    Some(Pending::Abort) | None => {}
                }
            }
            "agent_start" => {
                agent_starts += 1;
                store
                    .attempt_event(
                        id,
                        &format!("agent-start-{agent_starts}"),
                        "adapter.agent_start",
                        json!({"attempt_id": id}),
                    )
                    .await?;
                if agent_starts == 1
                    && let Some(input) = &turn.input_id
                {
                    move_input(
                        store,
                        input,
                        InputState::Delivered,
                        InputState::Applied,
                        None,
                    )
                    .await?;
                }
            }
            "agent_end" => {
                if !launch.settled_event && settled.is_none() && lines.is_some() {
                    let check = command_id("state");
                    pending.insert(check.clone(), Pending::Idle);
                    send(&lines, json!({"id": check, "type": "get_state"}));
                    state_queries += 1;
                }
            }
            "agent_settled" => {
                if settled.is_none() {
                    settled = Some(SettledBy::AgentSettled);
                    aborted = record["aborted"] == true;
                }
                close!();
            }
            "tool_execution_start" => tool_started = true,
            "message_end" if record["message"]["role"] == "assistant" => {
                reply = Some(text_of(&record["message"]));
            }
            "extension_ui_request" => {
                let method = record["method"].as_str().unwrap_or_default();
                if DIALOGS.contains(&method) {
                    // Headless: no option is chosen for anyone.
                    dialogs += 1;
                    send(
                        &lines,
                        json!({"type": "extension_ui_response", "id": record["id"], "cancelled": true}),
                    );
                    store
                        .attempt_event(
                            id,
                            &format!("ui-request-{dialogs}"),
                            "adapter.ui_request",
                            json!({"attempt_id": id, "method": method, "request": record,
                                              "decision": "cancelled"}),
                        )
                        .await?;
                } else {
                    data += 1;
                    store
                        .attempt_event(
                            id,
                            &format!("ui-data-{data}"),
                            "adapter.ui_data",
                            json!({"attempt_id": id, "method": method, "request": record}),
                        )
                        .await?;
                }
            }
            _ => {}
        }
    };
    drop(lines);
    if !written {
        writer.abort();
    }
    let logs_incomplete = report.as_ref().is_none_or(|r| {
        r.stdout.error.is_some() || r.stderr.error.is_some() || !r.stdout.eof || !r.stderr.eof
    });
    if logs_incomplete && let Some(r) = &report {
        store
            .attempt_event(
                id,
                "raw-log-incomplete",
                "adapter.diagnostic",
                json!({"attempt_id": id, "reason": "RAW_LOG_INCOMPLETE",
                       "stdout": {"eof": r.stdout.eof, "error": r.stdout.error},
                       "stderr": {"eof": r.stderr.eof, "error": r.stderr.error}}),
            )
            .await?;
    }
    let (state, failure) = verdict(&Ending {
        cancelled,
        too_large,
        deadline,
        logs_incomplete,
        failure: failure.as_deref(),
        settled,
        aborted,
        tool_started,
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
            ..AttemptFacts::default()
        },
        cancel,
    )
    .await?;
    Ok(Outcome {
        failure: if state == Cancelled { None } else { failure },
        state,
        args,
        native_session,
        native_path: launch.session_dir.clone(),
        model,
        disposition,
        settled_by: settled,
        reply,
        parse_errors,
        state_queries,
        exit: report.as_ref().and_then(|r| r.status),
        stdout_sha256: sha256_file(&stdout)?,
        stderr_sha256: sha256_file(&stderr)?,
        stdout,
        stderr,
        pid: Some(pid),
        steps: report.map(|r| r.steps).unwrap_or_default(),
    })
}

/// Whether the session's file (`<timestamp>_<id>.jsonl`, as pi names it) is in its directory.
fn session_file_exists(launch: &Launch) -> bool {
    let suffix = format!("_{}.jsonl", launch.session_id);
    std::fs::read_dir(&launch.session_dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(&suffix))
    })
}

async fn move_input(
    store: &Store,
    input_id: &str,
    from: InputState,
    to: InputState,
    reason: Option<String>,
) -> Result<(), PersistError> {
    store
        .move_input(InputMove {
            input_id: input_id.to_owned(),
            from,
            to,
            reason,
        })
        .await
}

#[cfg(test)]
mod tests;
