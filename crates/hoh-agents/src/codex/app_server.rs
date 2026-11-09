//! One turn over `codex app-server` (see the module docs of [`super`]).
//!
//! Turn-scoped notifications (items, token usage, turn/started, turn/completed) count only for this attempt's thread
//! and turn; those of this thread that come before turn/start is answered wait until this turn's id is known and are
//! dropped if they turn out to be another turn's (M1T04R-01). Once the turn is being stopped or exec is to be used, a
//! late answer moves nothing (M1T04R-02). stdin is written by a task the loop watches: a write failure stops the
//! process (recorded; the stream's end still decides), and the task is aborted, not awaited, once the process has
//! exited (M1T04R-04). A user message item is matched to the first steer of the same text that was sent to this turn
//! and has no item yet (M1T04R-10); the steer is APPLIED when both its item and codex's answer have come, in either
//! order (M1T04R-05). A refused steer gives its item up with it, and an item id
//! counts once (M1T04R-08/09). Notifications of this thread still waiting for an unconfirmed turn are looked at when
//! the stream ends: a tool item among them makes the end UNKNOWN_EFFECT (M1T04R-06).

use super::{
    Ending, METHOD_NOT_FOUND, Mode, Outcome, Steer, SteerReceipt, Turn, app_server_args,
    failure_of, hard_stop, hashes, is_text_item, verdict,
};
use crate::adapter::{AdapterError, AttemptRef, Recorded, Stop, finish, now, soft_move, step};
use crate::frame::Frame;
use crate::known;
use crate::process::{Config, Event, Process};
use hoh_core::lifecycle::{AttemptState, InputState};
use hoh_core::persistence::{AttemptFacts, CancelLatch, PersistError, Store};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
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

/// The thread and turn a turn-scoped notification is about; `None` for other notifications.
fn scope_of(method: &str, params: &Value) -> Option<(Option<String>, Option<String>)> {
    let s = |v: &Value| v.as_str().map(str::to_owned);
    match method {
        "item/started" | "item/completed" | "thread/tokenUsage/updated" => {
            Some((s(&params["threadId"]), s(&params["turnId"])))
        }
        "turn/started" | "turn/completed" => {
            Some((s(&params["threadId"]), s(&params["turn"]["id"])))
        }
        _ => None,
    }
}

/// A steer sent with turn/steer, until it is APPLIED or REJECTED.
struct SentSteer {
    input_id: String,
    text: String,
    /// The turn it was aimed at (its expectedTurnId): only a message of that turn can be its.
    target: Option<String>,
    /// The user message item matched to it.
    item: Option<String>,
    /// codex answered the turn/steer (the input is DELIVERED).
    acked: bool,
}

/// Everything one App Server turn knows.
struct State<'a> {
    /// The store in the direct mode; none in the runner's, which records what the turn saw (docs/adr/ADR-022.md
    /// decision 12).
    store: Option<&'a Store>,
    recorded: std::sync::Mutex<Vec<Recorded>>,
    /// A cancel only interrupts the turn: the host's ladder ends the process (the runner's mode, decision 14).
    native_only: bool,
    attempt: &'a AttemptRef,
    turn: &'a Turn,
    conn: Conn,
    pid: u32,
    phase: Phase,
    running: bool,
    thread_id: Option<String>,
    turn_id: Option<String>,
    model: Option<String>,
    usage: Option<Value>,
    status: Option<String>,
    error: Option<Value>,
    reply: Option<String>,
    cancelled: bool,
    deadline: bool,
    too_large: bool,
    tool_started: bool,
    failure: Option<String>,
    degrade: Option<String>,
    stop_at: Option<Instant>,
    stopped: bool,
    interrupted: bool,
    counters: HashMap<&'static str, usize>,
    /// Steers sent and not yet APPLIED or REJECTED, in the order sent.
    sent: Vec<SentSteer>,
    /// User message item ids already looked at.
    seen_items: HashSet<String>,
    /// The prompt's own user message has been seen (it is matched before any steer).
    prompt_seen: bool,
    /// Turn-scoped notifications of this thread that came before this turn's id was known, with their bytes.
    early: Vec<(String, Value, Vec<u8>)>,
}

impl State<'_> {
    /// The next number for events named `name-N`.
    fn next(&mut self, name: &'static str) -> usize {
        let n = self.counters.entry(name).or_insert(0);
        *n += 1;
        *n
    }

    async fn event(&self, name: &str, kind: &str, mut payload: Value) -> Result<(), PersistError> {
        payload["attempt_id"] = json!(self.attempt.attempt_id);
        match self.store {
            Some(store) => {
                store
                    .attempt_event(&self.attempt.attempt_id, name, kind, payload)
                    .await
            }
            None => {
                if let Ok(mut recorded) = self.recorded.lock() {
                    recorded.push(Recorded::Event {
                        key: name.to_owned(),
                        kind: kind.to_owned(),
                        payload,
                    });
                }
                Ok(())
            }
        }
    }

    async fn diagnostic(&mut self, payload: Value) -> Result<(), PersistError> {
        let n = self.next("diagnostic");
        self.event(&format!("diagnostic-{n}"), "adapter.diagnostic", payload)
            .await
    }

    /// End the process now: close stdin, then SIGTERM and SIGKILL to the group.
    fn hard_stop(&mut self, process: &Process) {
        self.conn.close();
        if !self.stopped {
            process.cancel(hard_stop(self.turn.escalation));
            self.stopped = true;
        }
    }

    /// Stop the turn: interrupt the active turn and give it the native grace; before a turn is active, end the
    /// process.
    fn begin_stop(&mut self, process: &Process) {
        if self.native_only {
            // Only the agent's own cancel: interrupt an active turn, or close stdin before one is; nothing more.
            if self.phase == Phase::Active && self.conn.open() && !self.interrupted {
                if let (Some(t), Some(u)) = (&self.thread_id, &self.turn_id) {
                    let params = json!({"threadId": t, "turnId": u});
                    self.conn
                        .request("turn/interrupt", params, Pending::Interrupt);
                }
            } else {
                self.conn.close();
            }
            self.interrupted = true;
            return;
        }
        if self.phase == Phase::Active && self.conn.open() && !self.interrupted {
            if let (Some(t), Some(u)) = (&self.thread_id, &self.turn_id) {
                let params = json!({"threadId": t, "turnId": u});
                self.conn
                    .request("turn/interrupt", params, Pending::Interrupt);
            }
            self.interrupted = true;
            self.stop_at = Some(Instant::now() + self.turn.escalation.native_grace);
        } else {
            self.hard_stop(process);
        }
    }

    /// Close stdin and give the process the term grace to exit by itself.
    fn close_gently(&mut self) {
        self.conn.close();
        if self.stop_at.is_none() || self.interrupted {
            self.stop_at = Some(Instant::now() + self.turn.escalation.term_grace);
        }
    }

    fn send_steer(&mut self, s: Steer) {
        // Only where the schema has turn/steer (the probe's mid_turn_steer); otherwise the input waits for a later
        // turn, as on exec (M1T07R-28, REQ-027).
        if !self.turn.launch.requests.contains(super::STEER_REQUEST) {
            let _ = s.reply.send(SteerReceipt::CapabilityUnsupported);
            return;
        }
        let expected = s.expected_turn.clone().or_else(|| self.turn_id.clone());
        self.sent.push(SentSteer {
            input_id: s.input_id.clone(),
            text: s.text.clone(),
            target: expected.clone(),
            item: None,
            acked: false,
        });
        let params = json!({"threadId": self.thread_id, "expectedTurnId": expected,
                            "input": [{"type": "text", "text": s.text}]});
        self.conn
            .request(super::STEER_REQUEST, params, Pending::Steer(s));
    }

    async fn apply(&self, input_id: String) -> Result<(), PersistError> {
        let Some(store) = self.store else {
            return Ok(());
        };
        soft_move(
            store,
            &self.attempt.attempt_id,
            &input_id,
            InputState::Delivered,
            InputState::Applied,
            None,
        )
        .await
        .map(drop)
    }

    /// A notification of this thread and turn.
    async fn on_turn_notification(
        &mut self,
        method: &str,
        frame: &Value,
    ) -> Result<(), PersistError> {
        let params = &frame["params"];
        match method {
            "item/started" | "item/completed" => {
                let n = self.next("item");
                let item = &params["item"];
                let kind = item["type"].as_str().unwrap_or_default();
                self.event(
                    &format!("item-{n}"),
                    "adapter.item",
                    json!({"method": method, "threadId": params["threadId"], "turnId": params["turnId"],
                           "itemId": item["id"], "itemType": kind}),
                )
                .await?;
                if method == "item/completed" && kind == "agentMessage" {
                    self.reply = item["text"].as_str().map(str::to_owned);
                }
                if method == "item/started" && !is_text_item(kind) {
                    self.tool_started = true;
                }
                let id = item["id"].as_str().unwrap_or_default().to_owned();
                if method == "item/started"
                    && kind == "userMessage"
                    && self.seen_items.insert(id.clone())
                {
                    let text = text_of(item);
                    if !self.prompt_seen && text == self.turn.prompt {
                        self.prompt_seen = true;
                    } else if let Some(at) = self.sent.iter().position(|x| {
                        x.item.is_none() && x.text == text && x.target == self.turn_id
                    }) {
                        self.sent[at].item = Some(id);
                        if self.sent[at].acked {
                            let x = self.sent.remove(at);
                            self.apply(x.input_id).await?;
                        }
                    }
                }
            }
            "thread/tokenUsage/updated" => self.usage = Some(params["tokenUsage"].clone()),
            "turn/completed" => {
                self.status = params["turn"]["status"].as_str().map(str::to_owned);
                self.error = Some(params["turn"]["error"].clone()).filter(|e| !e.is_null());
                self.phase = Phase::Done;
                self.close_gently();
            }
            _ => {}
        }
        Ok(())
    }

    /// A notification (not a request) from codex.
    async fn on_notification(
        &mut self,
        method: &str,
        frame: &Value,
        line: &[u8],
    ) -> Result<(), PersistError> {
        let params = &frame["params"];
        if let Some((thread, turn)) = scope_of(method, params) {
            let this_thread = self.thread_id.is_some() && thread == self.thread_id;
            if this_thread && self.turn_id.is_none() && self.phase < Phase::Done {
                self.early
                    .push((method.to_owned(), frame.clone(), line.to_vec()));
            } else if this_thread && turn == self.turn_id {
                self.on_turn_notification(method, frame).await?;
            } else if method != "turn/started" {
                self.diagnostic(
                    json!({"reason": "a notification of another thread or turn", "method": method,
                                       "threadId": thread, "turnId": turn}),
                )
                .await?;
            }
            return Ok(());
        }
        match method {
            "error" => {
                let n = self.next("error");
                self.event(
                    &format!("error-{n}"),
                    "adapter.error",
                    json!({"error": params["error"], "willRetry": params["willRetry"],
                           "threadId": params["threadId"], "turnId": params["turnId"]}),
                )
                .await?;
            }
            _ if !self.turn.launch.known.contains(method) => {
                let n = self.next("unknown");
                self.event(
                    &format!("unknown-{n}"),
                    "adapter.unknown_event",
                    json!({"method": method, "key": method, "frame_sha256": known::frame_sha256(line),
                           "frame": frame}),
                )
                .await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// A server request: the headless policy answers it at once; one not in the version's registry is also kept as an
    /// `adapter.unknown_event`.
    async fn on_request(
        &mut self,
        method: &str,
        id: Value,
        frame: &Value,
        line: &[u8],
    ) -> Result<(), PersistError> {
        if !known::codex_app_server().contains(method) {
            let n = self.next("unknown");
            self.event(
                &format!("unknown-{n}"),
                "adapter.unknown_event",
                json!({"method": method, "key": method, "frame_sha256": known::frame_sha256(line),
                       "frame": frame}),
            )
            .await?;
        }
        let n = self.next("server-request");
        let answer = super::headless_answer(method);
        let (reply, recorded) = match answer {
            Ok(result) => (
                json!({"id": id, "result": result}),
                json!({"result": result}),
            ),
            Err((code, message)) => {
                let error = json!({"code": code, "message": message});
                (json!({"id": id, "error": error}), json!({"error": error}))
            }
        };
        self.conn.send(&reply);
        self.event(
            &format!("server-request-{n}"),
            "adapter.server_request",
            json!({"method": method, "id": id, "answer": recorded}),
        )
        .await
    }

    /// The answer to one of our requests.
    async fn on_response(
        &mut self,
        process: &Process,
        held: &mut Vec<Steer>,
        frame: &Value,
    ) -> Result<(), PersistError> {
        use AttemptState::*;
        let Some(what) = frame["id"]
            .as_i64()
            .and_then(|n| self.conn.pending.remove(&n))
        else {
            return self
                .diagnostic(json!({"reason": "a response to no request of ours", "frame": frame}))
                .await;
        };
        let refused = frame.get("error").filter(|e| !e.is_null()).cloned();
        let message = refused
            .as_ref()
            .and_then(|e| e["message"].as_str())
            .unwrap_or_default()
            .to_owned();
        // Once the turn is being stopped or exec is to be used, an answer to the handshake moves nothing.
        let settled_elsewhere = self.degrade.is_some() || self.stopped || !self.conn.open();
        match what {
            Pending::Initialize | Pending::Thread | Pending::TurnStart if settled_elsewhere => {
                self.diagnostic(json!({"reason": "an answer after the turn stopped or moved to exec", "frame": frame}))
                    .await?;
            }
            Pending::Initialize => match refused {
                Some(e) if e["code"].as_i64() == Some(METHOD_NOT_FOUND) => {
                    self.degrade = Some(format!("initialize: method not found ({message})"));
                    self.close_gently();
                }
                Some(_) => {
                    self.failure = Some(format!("initialize: {message}"));
                    self.begin_stop(process);
                }
                None => {
                    self.conn.send(&json!({"method": "initialized"}));
                    // In the runner's mode the runner recorded RUNNING before the gate opened.
                    if let Some(store) = self.store {
                        step(
                            store,
                            self.attempt,
                            Starting,
                            Running,
                            crate::identity::running_pid(self.pid),
                        )
                        .await?;
                    }
                    self.running = true;
                    self.phase = Phase::Thread;
                    let launch = &self.turn.launch;
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
                            self.conn
                                .request(super::RESUME_REQUEST, params, Pending::Thread);
                        }
                        None => self.conn.request("thread/start", params, Pending::Thread),
                    }
                }
            },
            Pending::Thread => match refused {
                Some(_) => {
                    let what = if self.turn.launch.resume.is_some() {
                        "thread/resume"
                    } else {
                        "thread/start"
                    };
                    self.failure = Some(format!("{what}: {message}"));
                    self.begin_stop(process);
                }
                None => {
                    let result = &frame["result"];
                    self.thread_id = result["thread"]["id"].as_str().map(str::to_owned);
                    self.model = result["model"].as_str().map(str::to_owned);
                    match (self.store, &self.thread_id) {
                        (Some(store), _) => {
                            store
                                .upsert_session(super::session(
                                    self.attempt,
                                    self.turn,
                                    true,
                                    self.thread_id.clone(),
                                ))
                                .await?;
                        }
                        (None, Some(thread)) => {
                            if let Ok(mut recorded) = self.recorded.lock() {
                                recorded.push(Recorded::NativeSession(thread.clone()));
                            }
                        }
                        (None, None) => {}
                    }
                    self.phase = Phase::TurnStart;
                    let params = json!({"threadId": self.thread_id,
                                        "input": [{"type": "text", "text": self.turn.prompt}]});
                    self.conn.request("turn/start", params, Pending::TurnStart);
                }
            },
            Pending::TurnStart => match refused {
                Some(_) => {
                    self.failure = Some(format!("turn/start: {message}"));
                    self.begin_stop(process);
                }
                None => {
                    self.turn_id = frame["result"]["turn"]["id"].as_str().map(str::to_owned);
                    self.phase = Phase::Active;
                    self.event(
                        "turn",
                        "adapter.turn",
                        json!({"threadId": self.thread_id, "turnId": self.turn_id}),
                    )
                    .await?;
                    // What came before this turn's id was known counts now, if it is this turn's.
                    for (method, early, bytes) in std::mem::take(&mut self.early) {
                        Box::pin(self.on_notification(&method, &early, &bytes)).await?;
                    }
                    for s in held.drain(..) {
                        if self.phase == Phase::Active {
                            self.send_steer(s);
                        } else {
                            let _ = s.reply.send(SteerReceipt::NotNow);
                        }
                    }
                }
            },
            Pending::Interrupt => {}
            Pending::Steer(s) => match refused {
                Some(_) => {
                    // A refused steer gives up the item matched to it.
                    self.sent.retain(|x| x.input_id != s.input_id);
                    // Someone else may have moved the input on; that is recorded, not fatal (M1T05R-01).
                    if let Some(store) = self.store {
                        soft_move(
                            store,
                            &self.attempt.attempt_id,
                            &s.input_id,
                            InputState::Queued,
                            InputState::Rejected,
                            Some(message.clone()),
                        )
                        .await?;
                    }
                    let _ = s.reply.send(SteerReceipt::Rejected { reason: message });
                }
                None => {
                    if let Some(store) = self.store {
                        soft_move(
                            store,
                            &self.attempt.attempt_id,
                            &s.input_id,
                            InputState::Queued,
                            InputState::Delivered,
                            None,
                        )
                        .await?;
                    }
                    let took = frame["result"]["turnId"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| self.turn_id.clone())
                        .unwrap_or_default();
                    // Its user message may have come first.
                    if let Some(at) = self.sent.iter().position(|x| x.input_id == s.input_id) {
                        if self.sent[at].item.is_some() {
                            let x = self.sent.remove(at);
                            self.apply(x.input_id).await?;
                        } else {
                            self.sent[at].acked = true;
                        }
                    }
                    let _ = s.reply.send(SteerReceipt::Delivered { turn_id: took });
                }
            },
        }
        Ok(())
    }
}

/// The raw log files of an App Server turn.
pub(super) fn logs(turn: &Turn) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        turn.logs.join("app-server.stdout.jsonl"),
        turn.logs.join("app-server.stderr.log"),
    )
}

/// The process settings of an App Server turn: its own ends stop hard, no deadline (the turn's is watched here).
pub(super) fn config(turn: &Turn) -> Config {
    let (stdout, stderr) = logs(turn);
    Config {
        escalation: hard_stop(turn.escalation),
        deadline: None,
        stdout_copy: Some(stdout),
        stderr_copy: Some(stderr),
        ..Config::default()
    }
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
    let (stdout, stderr) = logs(turn);
    let args = match app_server_args(help) {
        Ok(args) => args,
        Err(super::Blocked(why)) => return Ok(End::Degrade(why)),
    };
    let mut command = Command::new(&turn.launch.program);
    command.args(&args).current_dir(&turn.launch.cwd);
    let mut process = match Process::spawn(command, config(turn)) {
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
    let drove = match drive(
        Some(store),
        attempt,
        turn,
        &mut process,
        Stop::Latch(cancel),
        steers,
        held,
        None,
    )
    .await?
    {
        Drove::Degrade(reason, _) => return Ok(End::Degrade(reason)),
        Drove::Ended(drove) => *drove,
    };
    let state = finish(
        store,
        attempt,
        if drove.running { Running } else { Starting },
        drove.state,
        AttemptFacts {
            ended_at: now(),
            resolved_model: drove.model.clone(),
            failure_reason: drove.failure.clone(),
            cancel_requested: Some(drove.cancelled),
            usage: drove.usage.clone(),
            ..AttemptFacts::default()
        },
        cancel,
    )
    .await?;
    let (stdout_sha256, stderr_sha256) = hashes(&stdout, &stderr)?;
    let exit = drove.report.as_ref().and_then(|r| r.status);
    Ok(End::Ended(Box::new(Outcome {
        failure: if state == Cancelled {
            None
        } else {
            drove.failure
        },
        state,
        mode: Mode::AppServer,
        args,
        native_session: drove.thread_id,
        turn_id: drove.turn_id,
        model: drove.model,
        status: drove.status,
        reply: drove.reply,
        exit,
        stdout,
        stderr,
        stdout_sha256,
        stderr_sha256,
        items: drove.items,
        unknown_events: drove.unknown_events,
        degraded: None,
        pid: Some(drove.pid),
        max_buffered: drove.report.as_ref().map(|r| r.stdout.max_buffered),
        steps: drove.report.map(|r| r.steps).unwrap_or_default(),
    })))
}

/// How an App Server turn's drive ended: moved to exec (the handshake refused, before anything ranked above it), or
/// over with its verdict.
pub(super) enum Drove {
    /// The reason, and how the process ended.
    Degrade(String, Option<crate::process::ExitReport>),
    Ended(Box<Driven>),
}

/// What an App Server turn showed, and the adapter's verdict.
pub(super) struct Driven {
    pub(super) state: AttemptState,
    pub(super) failure: Option<String>,
    pub(super) running: bool,
    pub(super) cancelled: bool,
    pub(super) thread_id: Option<String>,
    pub(super) turn_id: Option<String>,
    pub(super) model: Option<String>,
    pub(super) usage: Option<Value>,
    pub(super) status: Option<String>,
    pub(super) reply: Option<String>,
    pub(super) items: usize,
    pub(super) unknown_events: usize,
    pub(super) pid: u32,
    pub(super) report: Option<crate::process::ExitReport>,
    /// What the turn saw, in the runner's mode (the direct mode wrote it to the store).
    pub(super) recorded: Vec<Recorded>,
}

/// Drive one App Server turn on `process` until it ended: the handshake, the thread, the turn, its notifications and
/// requests, a cancel from `stop`. `preamble` is written first (the runner's mode: the gate's `go`). With no store
/// (the runner's mode) nothing is written and the turn's events come back with it.
#[allow(clippy::too_many_arguments)]
pub(super) async fn drive(
    store: Option<&Store>,
    attempt: &AttemptRef,
    turn: &Turn,
    process: &mut Process,
    stop: Stop<'_>,
    steers: &mut mpsc::Receiver<Steer>,
    held: &mut Vec<Steer>,
    preamble: Option<Vec<u8>>,
) -> Result<Drove, AdapterError> {
    let (lines, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
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
    let mut s = State {
        store,
        recorded: std::sync::Mutex::new(Vec::new()),
        native_only: stop.is_native(),
        attempt,
        turn,
        conn: Conn {
            lines: Some(lines),
            next_id: 0,
            pending: HashMap::new(),
        },
        pid: process.pid(),
        phase: Phase::Init,
        running: false,
        thread_id: None,
        turn_id: None,
        model: None,
        usage: None,
        status: None,
        error: None,
        reply: None,
        cancelled: stop.requested_now(),
        deadline: false,
        too_large: false,
        tool_started: false,
        failure: None,
        degrade: None,
        stop_at: None,
        stopped: false,
        interrupted: false,
        counters: HashMap::new(),
        sent: Vec::new(),
        seen_items: HashSet::new(),
        prompt_seen: false,
        early: Vec::new(),
    };
    s.conn.request(
        "initialize",
        json!({"clientInfo": {"name": "hoh", "version": crate::VERSION}}),
        Pending::Initialize,
    );
    let handshake_until = Instant::now() + turn.handshake_timeout;
    let deadline_at = turn.deadline.map(|d| Instant::now() + d);
    let (mut steers_open, mut written) = (true, false);
    if s.cancelled {
        s.begin_stop(process);
    }
    let report = loop {
        let event = tokio::select! {
            biased;
            () = stop.requested(), if !s.cancelled => {
                s.cancelled = true;
                s.begin_stop(process);
                continue;
            }
            _ = tokio::time::sleep_until(at(deadline_at)), if deadline_at.is_some() && !s.deadline => {
                s.deadline = true;
                s.begin_stop(process);
                continue;
            }
            _ = tokio::time::sleep_until(handshake_until),
                if s.phase == Phase::Init && s.degrade.is_none() && !s.stopped => {
                s.degrade = Some(format!("initialize did not answer within {:?}", turn.handshake_timeout));
                s.close_gently();
                continue;
            }
            _ = tokio::time::sleep_until(at(s.stop_at)), if s.stop_at.is_some() && !s.stopped => {
                s.hard_stop(process);
                continue;
            }
            result = &mut writer, if !written => {
                written = true;
                // stdin failed while we still had something to say: the protocol is broken.
                let failed = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(e.to_string()),
                };
                // codex can no longer be told anything (it closed stdin or is gone): stop it. The stream's end decides
                // the outcome, so a tool that had started still makes it UNKNOWN_EFFECT.
                if let Some(e) = failed.filter(|_| s.conn.open()) {
                    s.diagnostic(json!({"reason": "stdin failed", "error": e})).await?;
                    s.hard_stop(process);
                }
                continue;
            }
            steer = steers.recv(), if steers_open => {
                match steer {
                    None => steers_open = false,
                    Some(st) if s.phase == Phase::Active && s.conn.open() && !s.interrupted => s.send_steer(st),
                    Some(st) if s.phase < Phase::Active && s.degrade.is_none() && !s.stopped => held.push(st),
                    Some(st) => {
                        let _ = st.reply.send(SteerReceipt::NotNow);
                    }
                }
                continue;
            }
            event = process.next() => event,
        };
        let line = match event {
            Some(Event::Frame(Frame::Line(bytes) | Frame::Partial(bytes))) => bytes,
            Some(Event::Frame(Frame::TooLarge(refused))) => {
                s.too_large = true;
                s.hard_stop(process);
                s.event(
                    "frame-too-large",
                    "adapter.diagnostic",
                    json!({"reason": "PROTOCOL_FRAME_TOO_LARGE",
                           "length": refused.length, "prefix_sha256": refused.prefix_sha256,
                           "head": String::from_utf8_lossy(&refused.head)}),
                )
                .await?;
                continue;
            }
            Some(Event::Exit(report)) => break Some(*report),
            None => break None,
        };
        let Ok(frame) = serde_json::from_slice::<Value>(&line) else {
            s.diagnostic(
                json!({"reason": "stdout line is not JSON", "bytes": line.len(),
                                "head": String::from_utf8_lossy(&line[..line.len().min(256)])}),
            )
            .await?;
            continue;
        };
        match (frame["method"].as_str(), frame.get("id").cloned()) {
            (Some(method), Some(id)) => s.on_request(method, id, &frame, &line).await?,
            (Some(method), None) => s.on_notification(method, &frame, &line).await?,
            (None, _) => s.on_response(process, held, &frame).await?,
        }
    };
    s.conn.close();
    // The process has exited: whatever is left to write goes nowhere, so the writer is not waited for.
    if !written {
        writer.abort();
    }
    // Steers nobody answered stay QUEUED.
    for (_, what) in s.conn.pending.drain() {
        if let Pending::Steer(st) = what {
            let _ = st.reply.send(SteerReceipt::NotNow);
        }
    }
    let logs_incomplete = report.as_ref().is_none_or(|r| {
        r.stdout.error.is_some() || r.stderr.error.is_some() || !r.stdout.eof || !r.stderr.eof
    });
    // A refused handshake moves the turn to exec only when nothing ranked above a refused request in ADR-012 §3
    // happened: a cancel, a frame too large, the deadline, a raw log cut short (M1R-02).
    if let Some(reason) = s
        .degrade
        .clone()
        .filter(|_| !s.cancelled && !s.too_large && !s.deadline && !logs_incomplete)
    {
        return Ok(Drove::Degrade(reason, report));
    }
    for st in held.drain(..) {
        let _ = st.reply.send(SteerReceipt::NotNow);
    }
    // This thread's notifications still waiting for a turn that was never confirmed: a tool among them may have acted.
    if !s.early.is_empty() {
        let early = std::mem::take(&mut s.early);
        let started: Vec<Value> = early
            .iter()
            .filter(|(m, _, _)| m == "item/started")
            .map(|(_, f, _)| f["params"]["item"].clone())
            .collect();
        if started
            .iter()
            .any(|i| !is_text_item(i["type"].as_str().unwrap_or_default()))
        {
            s.tool_started = true;
        }
        s.event(
            "unconfirmed-turn",
            "adapter.diagnostic",
            json!({"reason": "notifications of this thread for a turn never confirmed",
                   "methods": early.iter().map(|(m, _, _)| m.clone()).collect::<Vec<_>>(),
                   "items_started": started.iter().map(|i| json!({"id": i["id"], "type": i["type"]})).collect::<Vec<_>>()}),
        )
        .await?;
    }
    if logs_incomplete && let Some(r) = &report {
        s.event(
            "raw-log-incomplete",
            "adapter.diagnostic",
            json!({"reason": "RAW_LOG_INCOMPLETE",
                   "stdout": {"eof": r.stdout.eof, "error": r.stdout.error},
                   "stderr": {"eof": r.stderr.eof, "error": r.stderr.error}}),
        )
        .await?;
    }
    let exit = report.as_ref().and_then(|r| r.status);
    let (state, failure) = verdict(&Ending {
        cancelled: s.cancelled,
        too_large: s.too_large,
        deadline: s.deadline,
        logs_incomplete,
        failure: s.failure.as_deref(),
        status: s.status.as_deref(),
        error: s.error.as_ref(),
        auth_hint: false,
        tool_started: s.tool_started,
        exit_matters: false,
        exit,
    });
    if s.status.as_deref() == Some("failed") {
        let reason = failure_of(s.error.as_ref(), false);
        s.event(
            "turn-failed",
            "adapter.turn_failed",
            json!({"status": s.status, "error": s.error,
                   "blocked_reason": (reason == "BLOCKED_AUTH").then_some("BLOCKED_AUTH"),
                   "retry": if reason == "TRANSIENT_EXTERNAL" { "transient" } else { "none" }}),
        )
        .await?;
    }
    let recorded = s
        .recorded
        .lock()
        .map(|mut r| std::mem::take(&mut *r))
        .unwrap_or_default();
    Ok(Drove::Ended(Box::new(Driven {
        state,
        failure,
        running: s.running,
        cancelled: s.cancelled,
        items: s.counters.get("item").copied().unwrap_or(0),
        unknown_events: s.counters.get("unknown").copied().unwrap_or(0),
        thread_id: s.thread_id,
        turn_id: s.turn_id,
        model: s.model,
        usage: s.usage,
        status: s.status,
        reply: s.reply,
        pid: s.pid,
        report,
        recorded,
    })))
}
