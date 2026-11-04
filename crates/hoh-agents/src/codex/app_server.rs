//! One turn over `codex app-server` (see the module docs of [`super`]).

use super::{
    Ending, METHOD_NOT_FOUND, Mode, Outcome, Steer, SteerReceipt, Turn, app_server_args,
    failure_of, hard_stop, hashes, is_text_item, verdict,
};
use crate::adapter::{AdapterError, AttemptRef, finish, now, step};
use crate::frame::Frame;
use crate::process::{Config, Event, Process};
use hoh_core::lifecycle::{AttemptState, InputState};
use hoh_core::persistence::{AttemptFacts, CancelLatch, InputMove, Store};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// How the App Server part ended.
pub(super) enum End {
    Ended(Box<Outcome>),
    /// Use exec instead (initialize was "method not found" or did not answer); the attempt is still STARTING.
    Degrade(String),
}

/// What a request of ours was.
enum Pending {
    Initialize,
    Thread,
    TurnStart,
    Interrupt,
    Steer(Steer),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Init,
    Thread,
    TurnStart,
    Active,
    Done,
}

/// Our side of the JSON-RPC connection: lines go to a writer task, so writing never holds up the loop.
struct Conn {
    lines: Option<mpsc::UnboundedSender<Vec<u8>>>,
    next_id: i64,
    pending: HashMap<i64, Pending>,
}

impl Conn {
    fn send(&self, message: &Value) {
        if let Some(lines) = &self.lines {
            let mut line = message.to_string().into_bytes();
            line.push(b'\n');
            let _ = lines.send(line);
        }
    }

    fn request(&mut self, method: &str, params: Value, what: Pending) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, what);
        self.send(&json!({"method": method, "id": id, "params": params}));
    }

    fn notify(&self, method: &str) {
        self.send(&json!({"method": method}));
    }

    fn open(&self) -> bool {
        self.lines.is_some()
    }

    /// Close stdin (once the writer has written what is queued).
    fn close(&mut self) {
        self.lines = None;
    }
}

/// A point far enough away to stand for "never".
fn at(when: Option<Instant>) -> Instant {
    when.unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400))
}

/// The user-message text of an item.
fn text_of(item: &Value) -> String {
    item["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

pub(super) async fn run(
    store: &Store,
    attempt: &AttemptRef,
    turn: &Turn,
    help: &str,
    cancel: &CancelLatch,
    steers: &mut mpsc::Receiver<Steer>,
    held: &mut Vec<Steer>,
) -> Result<End, AdapterError> {
    use AttemptState::*;
    let id = &attempt.attempt_id;
    let (stdout, stderr) = (
        turn.logs.join("app-server.stdout.jsonl"),
        turn.logs.join("app-server.stderr.log"),
    );
    let args = app_server_args(help);
    let mut command = Command::new(&turn.launch.program);
    command.args(&args).current_dir(&turn.launch.cwd);
    let config = Config {
        escalation: hard_stop(turn.escalation),
        deadline: None,
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
    let (lines, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
    let input = process.take_stdin();
    let writer = tokio::spawn(async move {
        let mut input = input.ok_or_else(|| std::io::Error::other("no stdin pipe"))?;
        while let Some(line) = queue.recv().await {
            input.write_all(&line).await?;
        }
        input.shutdown().await
    });
    let mut conn = Conn {
        lines: Some(lines),
        next_id: 0,
        pending: HashMap::new(),
    };
    conn.request(
        "initialize",
        json!({"clientInfo": {"name": "hoh", "version": crate::VERSION}}),
        Pending::Initialize,
    );
    let launch = &turn.launch;
    let handshake_until = Instant::now() + turn.handshake_timeout;
    let deadline_at = turn.deadline.map(|d| Instant::now() + d);
    let mut phase = Phase::Init;
    let mut running = false;
    let (mut thread_id, mut turn_id, mut model, mut usage) =
        (None::<String>, None::<String>, None, None);
    let (mut status, mut error, mut reply) = (None::<String>, None::<Value>, None::<String>);
    let mut cancelled = cancel.is_requested();
    let (mut deadline, mut too_large, mut tool_started) = (false, false, false);
    let (mut failure, mut degrade) = (None::<String>, None::<String>);
    let (mut stop_at, mut stopped, mut interrupted) = (None::<Instant>, false, false);
    let (mut items, mut unknown, mut diagnostics, mut requests, mut errors) = (0, 0, 0, 0, 0);
    let mut delivered: Vec<(String, String)> = Vec::new();
    let mut steers_open = true;
    // Stop: interrupt the active turn and give it the native grace; before a turn is active, end the process.
    macro_rules! begin_stop {
        () => {
            if phase == Phase::Active && conn.open() && !interrupted {
                if let (Some(t), Some(u)) = (&thread_id, &turn_id) {
                    conn.request(
                        "turn/interrupt",
                        json!({"threadId": t, "turnId": u}),
                        Pending::Interrupt,
                    );
                }
                interrupted = true;
                stop_at = Some(Instant::now() + turn.escalation.native_grace);
            } else if !stopped {
                conn.close();
                process.cancel(hard_stop(turn.escalation));
                stopped = true;
            }
        };
    }
    if cancelled {
        begin_stop!();
    }
    let report = loop {
        let event = tokio::select! {
            biased;
            () = cancel.requested(), if !cancelled => {
                cancelled = true;
                begin_stop!();
                continue;
            }
            _ = tokio::time::sleep_until(at(deadline_at)), if deadline_at.is_some() && !deadline => {
                deadline = true;
                begin_stop!();
                continue;
            }
            _ = tokio::time::sleep_until(handshake_until), if phase == Phase::Init && degrade.is_none() && !stopped => {
                degrade = Some(format!("initialize did not answer within {:?}", turn.handshake_timeout));
                conn.close();
                stop_at = Some(Instant::now() + turn.escalation.term_grace);
                continue;
            }
            _ = tokio::time::sleep_until(at(stop_at)), if stop_at.is_some() && !stopped => {
                conn.close();
                process.cancel(hard_stop(turn.escalation));
                stopped = true;
                continue;
            }
            steer = steers.recv(), if steers_open => {
                match steer {
                    None => steers_open = false,
                    Some(s) if phase == Phase::Active && conn.open() && !interrupted => {
                        let expected = s.expected_turn.clone().or_else(|| turn_id.clone());
                        conn.request(
                            "turn/steer",
                            json!({"threadId": thread_id, "expectedTurnId": expected,
                                   "input": [{"type": "text", "text": s.text}]}),
                            Pending::Steer(s),
                        );
                    }
                    Some(s) if phase < Phase::Active && degrade.is_none() && !stopped => held.push(s),
                    Some(s) => {
                        let _ = s.reply.send(SteerReceipt::NotNow);
                    }
                }
                continue;
            }
            event = process.next() => event,
        };
        let line = match event {
            Some(Event::Frame(Frame::Line(bytes) | Frame::Partial(bytes))) => bytes,
            Some(Event::Frame(Frame::TooLarge(refused))) => {
                too_large = true;
                conn.close();
                process.cancel(hard_stop(turn.escalation));
                stopped = true;
                store
                    .attempt_event(
                        id,
                        "frame-too-large",
                        "adapter.diagnostic",
                        json!({"attempt_id": id, "reason": "PROTOCOL_FRAME_TOO_LARGE",
                               "length": refused.length, "prefix_sha256": refused.prefix_sha256}),
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
                    id,
                    &format!("diagnostic-{diagnostics}"),
                    "adapter.diagnostic",
                    json!({"attempt_id": id, "reason": "stdout line is not JSON", "bytes": line.len(),
                           "head": String::from_utf8_lossy(&line[..line.len().min(256)])}),
                )
                .await?;
            continue;
        };
        if let Some(method) = frame["method"].as_str() {
            if let Some(request) = frame.get("id").cloned() {
                // A server request: the headless policy answers it at once.
                requests += 1;
                let answer = super::headless_answer(method);
                match &answer {
                    Ok(result) => conn.send(&json!({"id": request, "result": result})),
                    Err((code, message)) => conn
                        .send(&json!({"id": request, "error": {"code": code, "message": message}})),
                }
                store
                    .attempt_event(
                        id,
                        &format!("server-request-{requests}"),
                        "adapter.server_request",
                        json!({"attempt_id": id, "method": method, "id": request,
                               "answer": match answer {
                                   Ok(result) => json!({"result": result}),
                                   Err((code, message)) => json!({"error": {"code": code, "message": message}}),
                               }}),
                    )
                    .await?;
                continue;
            }
            let params = &frame["params"];
            match method {
                "item/started" | "item/completed" => {
                    items += 1;
                    let item = &params["item"];
                    let kind = item["type"].as_str().unwrap_or_default();
                    store
                        .attempt_event(
                            id,
                            &format!("item-{items}"),
                            "adapter.item",
                            json!({"attempt_id": id, "method": method, "threadId": params["threadId"],
                                   "turnId": params["turnId"], "itemId": item["id"], "itemType": kind}),
                        )
                        .await?;
                    if method == "item/completed" && kind == "agentMessage" {
                        reply = item["text"].as_str().map(str::to_owned);
                    }
                    if method == "item/started" && !is_text_item(kind) {
                        tool_started = true;
                    }
                    if method == "item/started" && kind == "userMessage" {
                        let text = text_of(item);
                        if let Some(at) = delivered.iter().position(|(_, t)| *t == text) {
                            let (input_id, _) = delivered.remove(at);
                            store
                                .move_input(InputMove {
                                    input_id,
                                    from: InputState::Delivered,
                                    to: InputState::Applied,
                                    reason: None,
                                })
                                .await?;
                        }
                    }
                }
                "thread/tokenUsage/updated" => usage = Some(params["tokenUsage"].clone()),
                "error" => {
                    errors += 1;
                    store
                        .attempt_event(
                            id,
                            &format!("error-{errors}"),
                            "adapter.error",
                            json!({"attempt_id": id, "error": params["error"], "willRetry": params["willRetry"]}),
                        )
                        .await?;
                }
                "turn/started" => {
                    if turn_id.is_none() {
                        turn_id = params["turn"]["id"].as_str().map(str::to_owned);
                    }
                }
                "turn/completed" => {
                    let ours = turn_id.as_deref().is_none_or(|t| params["turn"]["id"] == t);
                    if ours {
                        status = params["turn"]["status"].as_str().map(str::to_owned);
                        error = Some(params["turn"]["error"].clone()).filter(|e| !e.is_null());
                        phase = Phase::Done;
                        conn.close();
                        if stop_at.is_none() || interrupted {
                            stop_at = Some(Instant::now() + turn.escalation.term_grace);
                        }
                    }
                }
                _ if !launch.known.contains(method) => {
                    unknown += 1;
                    store
                        .attempt_event(
                            id,
                            &format!("unknown-{unknown}"),
                            "adapter.unknown_event",
                            json!({"attempt_id": id, "method": method, "frame": frame}),
                        )
                        .await?;
                }
                _ => {}
            }
            continue;
        }
        // A response to one of our requests.
        let what = frame["id"].as_i64().and_then(|n| conn.pending.remove(&n));
        let refused = frame.get("error").filter(|e| !e.is_null()).cloned();
        let message = refused
            .as_ref()
            .and_then(|e| e["message"].as_str())
            .unwrap_or_default()
            .to_owned();
        match what {
            Some(Pending::Initialize) => match refused {
                Some(e) if e["code"].as_i64() == Some(METHOD_NOT_FOUND) => {
                    degrade = Some(format!("initialize: method not found ({message})"));
                    conn.close();
                    stop_at = Some(Instant::now() + turn.escalation.term_grace);
                }
                Some(_) => {
                    failure = Some(format!("initialize: {message}"));
                    begin_stop!();
                }
                None => {
                    conn.notify("initialized");
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
                    running = true;
                    phase = Phase::Thread;
                    let mut params = if launch.yolo {
                        json!({"approvalPolicy": "never", "sandbox": "danger-full-access"})
                    } else {
                        json!({})
                    };
                    if let Some(m) = &launch.model {
                        params["model"] = json!(m);
                    }
                    match &launch.resume {
                        Some(thread) => {
                            params["threadId"] = json!(thread);
                            conn.request("thread/resume", params, Pending::Thread);
                        }
                        None => conn.request("thread/start", params, Pending::Thread),
                    }
                }
            },
            Some(Pending::Thread) => match refused {
                Some(_) => {
                    let what = if launch.resume.is_some() {
                        "thread/resume"
                    } else {
                        "thread/start"
                    };
                    failure = Some(format!("{what}: {message}"));
                    begin_stop!();
                }
                None => {
                    let result = &frame["result"];
                    thread_id = result["thread"]["id"].as_str().map(str::to_owned);
                    model = result["model"].as_str().map(str::to_owned);
                    store
                        .upsert_session(super::session(attempt, turn, true, thread_id.clone()))
                        .await?;
                    phase = Phase::TurnStart;
                    conn.request(
                        "turn/start",
                        json!({"threadId": thread_id, "input": [{"type": "text", "text": turn.prompt}]}),
                        Pending::TurnStart,
                    );
                }
            },
            Some(Pending::TurnStart) => match refused {
                Some(_) => {
                    failure = Some(format!("turn/start: {message}"));
                    begin_stop!();
                }
                None => {
                    turn_id = frame["result"]["turn"]["id"]
                        .as_str()
                        .map(str::to_owned)
                        .or(turn_id);
                    phase = Phase::Active;
                    store
                        .attempt_event(
                            id,
                            "turn",
                            "adapter.turn",
                            json!({"attempt_id": id, "threadId": thread_id, "turnId": turn_id}),
                        )
                        .await?;
                    for s in held.drain(..) {
                        let expected = s.expected_turn.clone().or_else(|| turn_id.clone());
                        conn.request(
                            "turn/steer",
                            json!({"threadId": thread_id, "expectedTurnId": expected,
                                   "input": [{"type": "text", "text": s.text}]}),
                            Pending::Steer(s),
                        );
                    }
                }
            },
            Some(Pending::Interrupt) => {}
            Some(Pending::Steer(s)) => match refused {
                Some(_) => {
                    store
                        .move_input(InputMove {
                            input_id: s.input_id.clone(),
                            from: InputState::Queued,
                            to: InputState::Rejected,
                            reason: Some(message.clone()),
                        })
                        .await?;
                    let _ = s.reply.send(SteerReceipt::Rejected { reason: message });
                }
                None => {
                    store
                        .move_input(InputMove {
                            input_id: s.input_id.clone(),
                            from: InputState::Queued,
                            to: InputState::Delivered,
                            reason: None,
                        })
                        .await?;
                    let took = frame["result"]["turnId"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| turn_id.clone())
                        .unwrap_or_default();
                    delivered.push((s.input_id, s.text));
                    let _ = s.reply.send(SteerReceipt::Delivered { turn_id: took });
                }
            },
            None => {
                diagnostics += 1;
                store
                    .attempt_event(
                        id,
                        &format!("diagnostic-{diagnostics}"),
                        "adapter.diagnostic",
                        json!({"attempt_id": id, "reason": "a response to no request of ours", "frame": frame}),
                    )
                    .await?;
            }
        }
    };
    conn.close();
    let _ = writer.await;
    // Steers nobody answered stay QUEUED.
    for (_, what) in conn.pending.drain() {
        if let Pending::Steer(s) = what {
            let _ = s.reply.send(SteerReceipt::NotNow);
        }
    }
    if let Some(reason) = degrade.filter(|_| !cancelled && !deadline) {
        return Ok(End::Degrade(reason));
    }
    for s in held.drain(..) {
        let _ = s.reply.send(SteerReceipt::NotNow);
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
    let exit = report.as_ref().and_then(|r| r.status);
    let (state, failure) = verdict(&Ending {
        cancelled,
        too_large,
        deadline,
        logs_incomplete,
        failure: failure.as_deref(),
        status: status.as_deref(),
        error: error.as_ref(),
        auth_hint: false,
        tool_started,
        exit_matters: false,
        exit,
    });
    if status.as_deref() == Some("failed") {
        let reason = failure_of(error.as_ref(), false);
        store
            .attempt_event(
                id,
                "turn-failed",
                "adapter.turn_failed",
                json!({"attempt_id": id, "status": status, "error": error,
                       "blocked_reason": (reason == "BLOCKED_AUTH").then_some("BLOCKED_AUTH"),
                       "retry": if reason == "TRANSIENT_EXTERNAL" { "transient" } else { "none" }}),
            )
            .await?;
    }
    let state = finish(
        store,
        attempt,
        if running { Running } else { Starting },
        state,
        AttemptFacts {
            ended_at: now(),
            resolved_model: model.clone(),
            failure_reason: failure.clone(),
            cancel_requested: Some(cancelled),
            usage,
            ..AttemptFacts::default()
        },
        cancel,
    )
    .await?;
    let (stdout_sha256, stderr_sha256) = hashes(&stdout, &stderr)?;
    Ok(End::Ended(Box::new(Outcome {
        failure: if state == Cancelled { None } else { failure },
        state,
        mode: Mode::AppServer,
        args,
        native_session: thread_id,
        turn_id,
        model,
        status,
        reply,
        exit,
        stdout,
        stderr,
        stdout_sha256,
        stderr_sha256,
        items,
        unknown_events: unknown,
        degraded: None,
        pid: Some(pid),
        steps: report.map(|r| r.steps).unwrap_or_default(),
    })))
}
