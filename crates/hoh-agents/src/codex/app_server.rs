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
use crate::adapter::{
    AdapterError, AttemptRef, Changed, Expect, Observer, Recorded, Replay, Replayed, Stop, Watch,
    finish, now, soft_move, step,
};
use crate::frame::{Frame, Skip};
use crate::known;
use crate::process::{Config, Event, Process};
use hoh_core::lifecycle::{AttemptState, InputState};
use hoh_core::persistence::{AttemptFacts, CancelLatch, PersistError, Store};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub mod requests;

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

    /// Send the request `build` gives for the next id ([`requests`]), remembering what it was.
    fn request(&mut self, what: Pending, build: impl FnOnce(i64) -> Value) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, what);
        self.send(&build(id));
    }

    fn open(&self) -> bool {
        self.lines.is_some()
    }

    /// Whether initialize is still waiting for its answer on an open connection: only then does the handshake timeout
    /// apply. Once initialize is answered, or the turn is closing for any reason (stdin closed: a refusal, the re-check
    /// before the first input, a stop, a cancel), it no longer does (M2T15R-02).
    fn awaiting_initialize(&self) -> bool {
        self.open()
            && self
                .pending
                .values()
                .any(|what| matches!(what, Pending::Initialize))
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
    /// Shown each diagnostic the runner's mode collects.
    observer: Observer,
    /// What the gate verified (the runner's mode): re-checked before thread/start, the first input (docs/adr/ADR-030.md
    /// decision 9).
    expect: Option<Expect>,
    /// What that re-check found changed: no input was sent.
    changed: Option<Changed>,
    /// A cancel only interrupts the turn: the host's ladder ends the process (the runner's mode, decision 14).
    native_only: bool,
    exclude_turns: bool,
    attempt: &'a AttemptRef,
    turn: &'a Turn,
    conn: Conn,
    pid: u32,
    phase: Phase,
    running: bool,
    thread_id: Option<String>,
    turn_id: Option<String>,
    /// The thread request's answer (its `result`), as correlated: the thread's effective sandbox for the judge
    /// (docs/adr/ADR-032.md decision 8, E3).
    thread_result: Option<Value>,
    model: Option<String>,
    usage: Option<Value>,
    status: Option<String>,
    error: Option<Value>,
    reply: Option<String>,
    cancelled: bool,
    deadline: bool,
    too_large: bool,
    /// Frames over the limit the reader skipped ([`SKIP`]), numbering their events.
    skipped: usize,
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
    /// The rate-limit windows codex reported during the turn.
    limits: RateLimits,
}

/// The quotas codex reported during a turn (`account/rateLimits/updated`; M2T07R-256, docs/adr/ADR-022.md decision 16's
/// design correction): by limit id, the model the quota is for (`normalModelSlug`, once reported) and its windows
/// (primary, secondary), each with its used percentage and reset (unix seconds). A sparse update keeps what an earlier
/// one said: a slug, window or reset it leaves out or sends as null clears nothing.
#[derive(Debug, Default)]
struct RateLimits(BTreeMap<String, Quota>);

/// One quota as codex last reported it.
#[derive(Debug, Default)]
struct Quota {
    model: Option<String>,
    windows: BTreeMap<&'static str, Window>,
}

/// One rate-limit window as codex last reported it.
#[derive(Debug, Default)]
struct Window {
    used_percent: Option<i64>,
    resets_at: Option<Value>,
}

impl Window {
    fn used_up(&self) -> bool {
        self.used_percent.is_some_and(|percent| percent >= 100)
    }
}

impl RateLimits {
    fn see(&mut self, params: &Value) {
        let limits = &params["rateLimits"];
        let quota = self
            .0
            .entry(limits["limitId"].as_str().unwrap_or_default().to_owned())
            .or_default();
        if let Some(model) = limits["normalModelSlug"].as_str() {
            quota.model = Some(model.to_owned());
        }
        for side in ["primary", "secondary"] {
            let window = &limits[side];
            if !window.is_object() {
                continue;
            }
            let seen = quota.windows.entry(side).or_default();
            if let Some(percent) = window["usedPercent"].as_i64() {
                seen.used_percent = Some(percent);
            }
            if !window["resetsAt"].is_null() {
                seen.resets_at = Some(window["resetsAt"].clone());
            }
        }
    }

    /// When the quota a turn on `model` (the thread's) failed on lifts (ADR-022 decision 16, design correction rule 1):
    /// only a used-up quota positively identified as applying to the turn counts: the account's own ([`ACCOUNT`] with no
    /// model ever reported) or one whose reported model is `model`; another model's is unrelated. A used-up quota whose
    /// attribution is unknown (another id with no model reported, a model quota with `model` unknown), or more than one
    /// that applies, gives none; so does a used-up window of it without a reset that is an instant. Otherwise the latest
    /// reset of its used-up windows.
    fn lifts(&self, model: Option<&str>) -> Option<std::time::SystemTime> {
        let mut applying = None;
        for (id, quota) in &self.0 {
            if !quota.windows.values().any(Window::used_up) {
                continue;
            }
            let applies = match (quota.model.as_deref(), model) {
                (None, _) if id == ACCOUNT => true,
                (Some(its), Some(model)) => its == model,
                _ => return None,
            };
            if applies && applying.replace(quota).is_some() {
                return None;
            }
        }
        let mut latest = None;
        for window in applying?.windows.values().filter(|w| w.used_up()) {
            let at = crate::adapter::unix_seconds(window.resets_at.as_ref()?)?;
            latest = latest.max(Some(at));
        }
        latest
    }
}

/// The limit id codex reports the account's own quota under (recorded: codex/0.166.0/app-server-turn's live account,
/// and the snapshot `_provider-faults` got from the `x-codex-primary-*` headers).
const ACCOUNT: &str = "codex";

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
                self.observer.collected(kind, &payload);
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

    /// End the process now: close stdin, then SIGTERM and SIGKILL to the group (`None`: a replay, no process).
    fn hard_stop(&mut self, process: Option<&Process>) {
        self.conn.close();
        if !self.stopped {
            if let Some(process) = process {
                process.cancel(hard_stop(self.turn.escalation));
            }
            self.stopped = true;
        }
    }

    /// Stop the turn: interrupt the active turn and give it the native grace; before a turn is active, end the
    /// process.
    fn begin_stop(&mut self, process: Option<&Process>) {
        if self.native_only {
            // Only the agent's own cancel: interrupt an active turn, or close stdin before one is; nothing more.
            if self.phase == Phase::Active && self.conn.open() && !self.interrupted {
                if let (Some(t), Some(u)) = (&self.thread_id, &self.turn_id) {
                    self.conn
                        .request(Pending::Interrupt, |id| requests::turn_interrupt(id, t, u));
                }
            } else {
                self.conn.close();
            }
            self.interrupted = true;
            return;
        }
        if self.phase == Phase::Active && self.conn.open() && !self.interrupted {
            if let (Some(t), Some(u)) = (&self.thread_id, &self.turn_id) {
                self.conn
                    .request(Pending::Interrupt, |id| requests::turn_interrupt(id, t, u));
            }
            self.interrupted = true;
            self.stop_at = Some(Instant::now() + self.turn.escalation.native_grace);
        } else {
            self.hard_stop(process);
        }
    }

    /// Stop the turn after codex refused a request of its handshake (initialize, the thread, turn/start), before any
    /// turn is active: the direct mode ends the process at once ([`State::begin_stop`]); the runner's closes stdin and
    /// gives the process the term grace to exit by itself, then ends it ([`State::close_gently`]), as after the re-check
    /// before the first input. A refusal is the turn's own end: no cancel of the host's follows it (M2T15R-02).
    fn stop_refused(&mut self, process: Option<&Process>) {
        if self.native_only {
            self.close_gently();
        } else {
            self.begin_stop(process);
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
        if s.reply.is_closed() {
            return;
        }
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
        let (thread, text) = (self.thread_id.clone(), s.text.clone());
        self.conn.request(Pending::Steer(s), |id| {
            requests::turn_steer(id, thread.as_deref(), expected.as_deref(), &text)
        });
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
                let mut payload = json!({"method": method, "threadId": params["threadId"], "turnId": params["turnId"],
                                         "itemId": item["id"], "itemType": kind});
                // The item's own facts, from which the read-only judge enumerates the turn's driver runs and writes
                // (docs/adr/ADR-032.md design correction 7 item 2): never a regex over the stream.
                let facts: &[&str] = match kind {
                    "commandExecution" => &["command", "status", "exitCode", "aggregatedOutput"],
                    "fileChange" => &["status", "changes"],
                    _ => &[],
                };
                for key in facts {
                    payload[*key] = item[*key].clone();
                }
                self.event(&format!("item-{n}"), "adapter.item", payload)
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
            "account/rateLimits/updated" => self.limits.see(params),
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
        process: Option<&Process>,
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
                    self.stop_refused(process);
                }
                // Before the first input, the agent's files are still the ones its gate verified (docs/adr/ADR-030.md
                // decision 9, "首个输入之前的再核对"): else nothing more is sent and stdin is closed.
                None if let Some(changed) = self
                    .expect
                    .as_ref()
                    .and_then(|e| crate::adapter::recheck(e).changed()) =>
                {
                    self.failure = Some(crate::adapter::COMPAT_UNVERIFIED.to_owned());
                    let (key, payload) = changed.event();
                    self.event(&key, "adapter.diagnostic", payload).await?;
                    self.changed = Some(changed);
                    self.close_gently();
                }
                None => {
                    self.conn.send(&requests::initialized());
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
                    let (profile, model) = (launch.profile, launch.model.as_deref());
                    match &launch.resume {
                        Some(thread) => self.conn.request(Pending::Thread, |id| {
                            requests::resume(id, profile, model, thread, self.exclude_turns)
                        }),
                        None => self.conn.request(Pending::Thread, |id| {
                            requests::thread_start(id, profile, model)
                        }),
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
                    self.stop_refused(process);
                }
                None => {
                    let result = &frame["result"];
                    self.thread_result = Some(result.clone());
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
                    let (thread, prompt) = (self.thread_id.as_deref(), &self.turn.prompt);
                    self.conn.request(Pending::TurnStart, |id| {
                        requests::turn_start(id, thread, prompt)
                    });
                }
            },
            Pending::TurnStart => match refused {
                Some(_) => {
                    self.failure = Some(format!("turn/start: {message}"));
                    self.stop_refused(process);
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

impl<'a> State<'a> {
    /// A turn about to send initialize on `lines`: `native_only` in the runner's mode, `cancelled` if a cancel came
    /// before it started.
    fn new(
        store: Option<&'a Store>,
        attempt: &'a AttemptRef,
        turn: &'a Turn,
        lines: mpsc::UnboundedSender<Vec<u8>>,
        pid: u32,
        native_only: bool,
        cancelled: bool,
    ) -> State<'a> {
        State {
            store,
            recorded: std::sync::Mutex::new(Vec::new()),
            observer: Observer::default(),
            expect: None,
            changed: None,
            native_only,
            exclude_turns: true,
            attempt,
            turn,
            conn: Conn {
                lines: Some(lines),
                next_id: 0,
                pending: HashMap::new(),
            },
            pid,
            phase: Phase::Init,
            running: false,
            thread_id: None,
            turn_id: None,
            thread_result: None,
            model: None,
            usage: None,
            status: None,
            error: None,
            reply: None,
            cancelled,
            deadline: false,
            too_large: false,
            skipped: 0,
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
            limits: RateLimits::default(),
        }
    }

    /// Judge one stdout line: a request, a notification or the answer to one of ours (`process`: `None` in a replay).
    async fn on_line(
        &mut self,
        process: Option<&Process>,
        held: &mut Vec<Steer>,
        line: &[u8],
    ) -> Result<(), PersistError> {
        let Ok(frame) = serde_json::from_slice::<Value>(line) else {
            return self
                .diagnostic(
                    json!({"reason": "stdout line is not JSON", "bytes": line.len(),
                           "head": String::from_utf8_lossy(&line[..line.len().min(256)])}),
                )
                .await;
        };
        match (frame["method"].as_str(), frame.get("id").cloned()) {
            (Some(method), Some(id)) => self.on_request(method, id, &frame, line).await,
            (Some(method), None) => self.on_notification(method, &frame, line).await,
            (None, _) => self.on_response(process, held, &frame).await,
        }
    }

    /// What the turn recorded so far (the runner's mode), taken.
    fn take_recorded(&self) -> Vec<Recorded> {
        self.recorded
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default()
    }

    /// The end of a turn whose process ended (`report`; `None`: it was not seen to end) and whose stdin is closed:
    /// moved to exec, or its verdict with what it records.
    async fn conclude(
        mut self,
        report: Option<crate::process::ExitReport>,
        held: &mut Vec<Steer>,
    ) -> Result<Drove, PersistError> {
        // Once handed to the stdin writer a steer may have reached Codex. Even a matching userMessage item
        // cannot replace the correlated RPC answer (items are matched by text). Never offer it for blind retry.
        for (_, what) in self.conn.pending.drain() {
            if let Pending::Steer(st) = what {
                if let Some(store) = self.store {
                    soft_move(
                        store,
                        &self.attempt.attempt_id,
                        &st.input_id,
                        InputState::Queued,
                        InputState::Rejected,
                        Some("UNKNOWN_DELIVERY".into()),
                    )
                    .await?;
                }
                let _ = st.reply.send(SteerReceipt::Rejected {
                    reason: "UNKNOWN_DELIVERY".into(),
                });
            }
        }
        let logs_incomplete = report.as_ref().is_none_or(|r| {
            r.stdout.error.is_some() || r.stderr.error.is_some() || !r.stdout.eof || !r.stderr.eof
        });
        // A refused handshake moves the turn to exec only when nothing ranked above a refused request in ADR-012 §3
        // happened: a cancel, a frame too large, the deadline, a raw log cut short (M1R-02); and never once the
        // handshake's answer decided the turn's failure (a refused initialize, the re-check before the first input:
        // docs/adr/ADR-030.md decision 9, M2T15R-02).
        let degrade = self.degrade.clone().filter(|_| {
            !self.cancelled
                && !self.too_large
                && !self.deadline
                && !logs_incomplete
                && self.failure.is_none()
        });
        // The runner has no exec fallback in this Attempt, so return its never-submitted held requests.
        // The legacy store-backed run retains them for its exec fallback's CapabilityUnsupported replies.
        if self.store.is_none() || degrade.is_none() {
            for st in held.drain(..) {
                let _ = st.reply.send(SteerReceipt::NotNow);
            }
        }
        if let Some(reason) = degrade {
            return Ok(Drove::Degrade(Box::new(Degraded {
                reason,
                report,
                recorded: self.take_recorded(),
            })));
        }
        // This thread's notifications still waiting for a turn that was never confirmed: a tool among them may have
        // acted.
        if !self.early.is_empty() {
            let early = std::mem::take(&mut self.early);
            let started: Vec<Value> = early
                .iter()
                .filter(|(m, _, _)| m == "item/started")
                .map(|(_, f, _)| f["params"]["item"].clone())
                .collect();
            if started
                .iter()
                .any(|i| !is_text_item(i["type"].as_str().unwrap_or_default()))
            {
                self.tool_started = true;
            }
            self.event(
                "unconfirmed-turn",
                "adapter.diagnostic",
                json!({"reason": "notifications of this thread for a turn never confirmed",
                       "methods": early.iter().map(|(m, _, _)| m.clone()).collect::<Vec<_>>(),
                       "items_started": started.iter().map(|i| json!({"id": i["id"], "type": i["type"]})).collect::<Vec<_>>()}),
            )
            .await?;
        }
        if logs_incomplete && let Some(r) = &report {
            self.event(
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
            cancelled: self.cancelled,
            too_large: self.too_large,
            deadline: self.deadline,
            logs_incomplete,
            failure: self.failure.as_deref(),
            status: self.status.as_deref(),
            error: self.error.as_ref(),
            auth_hint: false,
            tool_started: self.tool_started,
            exit_matters: false,
            exit,
        });
        // A turn that failed on a usage or rate limit: when the windows codex reported used up lift.
        let limited = matches!(
            self.error
                .as_ref()
                .and_then(|e| e["codexErrorInfo"].as_str()),
            Some("usageLimitExceeded" | "rateLimitExceeded")
        );
        let retry_at = if state == AttemptState::Failed
            && self.failure.is_none()
            && !self.cancelled
            && !self.too_large
            && !self.deadline
            && !logs_incomplete
            && self.status.as_deref() == Some("failed")
            && limited
        {
            self.limits.lifts(self.model.as_deref())
        } else {
            None
        };
        if self.status.as_deref() == Some("failed") {
            let reason = failure_of(self.error.as_ref(), false);
            self.event(
                "turn-failed",
                "adapter.turn_failed",
                json!({"status": self.status, "error": self.error,
                       "blocked_reason": (reason == "BLOCKED_AUTH").then_some("BLOCKED_AUTH"),
                       "retry": if reason == "TRANSIENT_EXTERNAL" { "transient" } else { "none" }}),
            )
            .await?;
        }
        let recorded = self.take_recorded();
        Ok(Drove::Ended(Box::new(Driven {
            state,
            failure,
            running: self.running,
            cancelled: self.cancelled,
            items: self.counters.get("item").copied().unwrap_or(0),
            unknown_events: self.counters.get("unknown").copied().unwrap_or(0),
            thread_id: self.thread_id,
            turn_id: self.turn_id,
            model: self.model,
            usage: self.usage,
            status: self.status,
            reply: self.reply,
            pid: self.pid,
            report,
            recorded,
            changed: self.changed,
            retry_at,
        })))
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
        skip: Some(SKIP),
        reduce: Some(REDUCE),
        ..Config::default()
    }
}

/// The notifications this adapter reads nothing of and that a long turn can grow past the frame limit (CR-040):
/// `turn/diff/updated`, the turn's whole diff so far.
fn skippable(head: &[u8]) -> Option<&'static str> {
    crate::frame::by_prefix(
        head,
        &[(
            br#"{"method":"turn/diff/updated","params":"#,
            "turn/diff/updated",
        )],
    )
}

/// The frames over the limit an App Server turn skips (CR-040); exec has none.
pub const SKIP: Skip = Skip {
    known: skippable,
    max_length: crate::frame::SKIP_LIMIT,
};

/// CR-041: native compact JSON prefix, including the item type before its potentially large content.
fn reducible(head: &[u8]) -> Option<&'static str> {
    crate::frame::by_prefix(
        head,
        &[
            (
                br#"{"method":"item/completed","params":{"item":{"type":"commandExecution","#,
                "commandExecution",
            ),
            (
                br#"{"method":"item/completed","params":{"item":{"type":"fileChange","#,
                "fileChange",
            ),
            (
                br#"{"method":"item/started","params":{"item":{"type":"fileChange","#,
                "fileChange",
            ),
        ],
    )
}

pub const REDUCE: Skip = Skip {
    known: reducible,
    max_length: crate::frame::SKIP_LIMIT,
};

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
    let mut process = match Process::spawn(command, config(turn), &*turn.spawner) {
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
        &mut Watch::new(None),
        Observer::default(),
    )
    .await?
    {
        Drove::Degrade(degraded) => return Ok(End::Degrade(degraded.reason)),
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
    /// Boxed: an exit report is large (clippy::large_enum_variant).
    Degrade(Box<Degraded>),
    Ended(Box<Driven>),
}

/// An App Server turn moved to exec: why, how its process ended, and what it recorded until then.
pub(super) struct Degraded {
    pub(super) reason: String,
    pub(super) report: Option<crate::process::ExitReport>,
    /// What the turn saw, in the runner's mode (the direct mode wrote it to the store).
    pub(super) recorded: Vec<Recorded>,
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
    /// What its re-check before the first input found changed (docs/adr/ADR-030.md decision 9).
    pub(super) changed: Option<Changed>,
    /// When the limit the turn failed on lifts ([`RateLimits`]).
    pub(super) retry_at: Option<std::time::SystemTime>,
}

/// Drive one App Server turn on `process` until it ended: the handshake, the thread, the turn, its notifications and
/// requests, a cancel from `stop`. `preamble` is written first (the runner's mode: the gate's `go`). With no store
/// (the runner's mode) nothing is written and the turn's events come back with it, each diagnostic shown to
/// `observer` as it is collected. `watch`'s expectation is re-checked before thread/start, and while the turn runs as
/// it is due.
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
    watch: &mut Watch,
    observer: Observer,
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
    let mut s = State::new(
        store,
        attempt,
        turn,
        lines,
        process.pid(),
        stop.is_native(),
        stop.requested_now(),
    );
    s.observer = observer;
    s.expect = watch.expect().cloned();
    s.conn.request(Pending::Initialize, requests::initialize);
    let handshake_until = Instant::now() + turn.handshake_timeout;
    let deadline_at = turn.deadline.map(|d| Instant::now() + d);
    let (mut steers_open, mut written) = (true, false);
    if s.cancelled {
        s.begin_stop(Some(process));
    }
    let report = loop {
        let event = tokio::select! {
            biased;
            () = stop.requested(), if !s.cancelled => {
                s.cancelled = true;
                s.begin_stop(Some(process));
                continue;
            }
            // A re-check that is due comes before output (the same branch wakes an idle loop for it).
            () = watch.due(), if watch.on() => {
                for (key, payload) in watch.check() {
                    s.event(&key, "adapter.diagnostic", payload).await?;
                }
                continue;
            }
            _ = tokio::time::sleep_until(at(deadline_at)), if deadline_at.is_some() && !s.deadline => {
                s.deadline = true;
                s.begin_stop(Some(process));
                continue;
            }
            _ = tokio::time::sleep_until(handshake_until), if s.conn.awaiting_initialize() => {
                s.degrade = Some(format!("initialize did not answer within {:?}", turn.handshake_timeout));
                s.close_gently();
                continue;
            }
            _ = tokio::time::sleep_until(at(s.stop_at)), if s.stop_at.is_some() && !s.stopped => {
                s.hard_stop(Some(process));
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
                    s.hard_stop(Some(process));
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
            Some(Event::Frame(Frame::Reduced { line, original })) => {
                let n = s.next("reduced");
                let mut payload = crate::adapter::skipped_frame(&original);
                payload["reason"] = json!("known item strings truncated");
                s.event(&format!("frame-reduced-{n}"), "adapter.diagnostic", payload)
                    .await?;
                line
            }
            Some(Event::Frame(Frame::TooLarge(refused))) => {
                s.too_large = true;
                s.hard_stop(Some(process));
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
            Some(Event::Frame(Frame::Skipped(skipped))) => {
                s.skipped += 1;
                let key = format!("frame-skipped-{}", s.skipped);
                s.event(
                    &key,
                    "adapter.diagnostic",
                    crate::adapter::skipped_frame(&skipped),
                )
                .await?;
                continue;
            }
            Some(Event::Exit(report)) => break Some(*report),
            None => break None,
        };
        s.on_line(Some(process), held, &line).await?;
    };
    s.conn.close();
    // The process has exited: whatever is left to write goes nowhere, so the writer is not waited for.
    if !written {
        writer.abort();
    }
    Ok(s.conclude(report, held).await?)
}

/// Replay `bytes` (the App Server's stdout, with the host's cancel where [`crate::adapter::REPLAY_CANCEL`] stands)
/// through this transport's own judging as the runner's mode drives `turn`, no process started (docs/adr/ADR-030.md
/// decision 10): see [`Replayed`]. What the turn writes goes nowhere; an initialize the App Server refuses as unknown
/// ends it as the runner's mode does (FAILED, CAPABILITY_UNSUPPORTED).
pub(super) fn replay(bytes: &[u8], turn: &Turn) -> Replayed {
    match drove(bytes, turn) {
        Ok(Drove::Ended(driven)) => Replayed {
            state: driven.state,
            failure: driven.failure,
            unknown_events: driven.unknown_events,
        },
        Ok(Drove::Degrade(_)) => Replayed {
            state: AttemptState::Failed,
            failure: Some("CAPABILITY_UNSUPPORTED".to_owned()),
            unknown_events: 0,
        },
        Err(why) => crate::adapter::unreplayable(&why),
    }
}

/// Replay a recorded App Server turn as the runner's mode drives `turn` (its launch from the recorded observations,
/// docs/adr/ADR-032.md design correction 7 item 2), the process's end as `observed` recorded it: the adapter's facts
/// and every message it wrote, built from the launch alone (initialize; `initialized` and the thread request once
/// initialize is answered; turn/start once the thread request is; server requests answered headless).
pub(super) fn replay_facts(
    bytes: &[u8],
    turn: &Turn,
    observed: &crate::observed::Observed,
) -> crate::observed::ReplayFacts {
    use crate::observed::ReplayFacts;
    let attempt = AttemptRef {
        attempt_id: "replay".to_owned(),
        session_id: "replay".to_owned(),
        role: String::new(),
        agent_version: String::new(),
    };
    let (lines, mut written) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut s = State::new(None, &attempt, turn, lines, 0, true, false);
    s.exclude_turns = observed.codex.as_ref().is_some_and(|c| c.exclude_turns);
    s.conn.request(Pending::Initialize, requests::initialize);
    let mut held = Vec::new();
    for step in crate::adapter::replay_steps(bytes) {
        match step {
            Replay::Cancel if !s.cancelled => {
                s.cancelled = true;
                s.begin_stop(None);
            }
            Replay::Cancel => {}
            Replay::Frame(line) => match crate::adapter::at_once(s.on_line(None, &mut held, &line))
            {
                Some(Ok(())) => {}
                Some(Err(e)) => return ReplayFacts::failed(format!("REPLAY_INCOMPLETE: {e}")),
                None => {
                    return ReplayFacts::failed("REPLAY_INCOMPLETE: the judging waited".to_owned());
                }
            },
        }
    }
    let thread_result = s.thread_result.clone();
    s.conn.close();
    let drove = match crate::adapter::at_once(
        s.conclude(Some(crate::observed::exit_report(observed)), &mut held),
    ) {
        Some(Ok(drove)) => drove,
        Some(Err(e)) => return ReplayFacts::failed(format!("REPLAY_INCOMPLETE: {e}")),
        None => return ReplayFacts::failed("REPLAY_INCOMPLETE: the judging waited".to_owned()),
    };
    let mut sent = Vec::new();
    while let Ok(line) = written.try_recv() {
        sent.push(serde_json::from_slice(&line).unwrap_or(Value::Null));
    }
    match drove {
        Drove::Ended(driven) => ReplayFacts {
            state: driven.state,
            failure: driven.failure,
            final_text: driven.reply,
            native_session: driven.thread_id.clone(),
            thread: driven.thread_id,
            turn: driven.turn_id,
            thread_result,
            recorded: driven.recorded,
            sent: Some(sent),
        },
        // The runner's mode ends a degraded turn FAILED (CAPABILITY_UNSUPPORTED): no second process within it.
        Drove::Degrade(degraded) => ReplayFacts {
            recorded: degraded.recorded,
            sent: Some(sent),
            ..ReplayFacts::failed(format!("CAPABILITY_UNSUPPORTED: {}", degraded.reason))
        },
    }
}

/// `bytes` through this transport's own judging as the runner's mode drives `turn`, no process started: how the turn
/// ended, or why it could not be judged.
fn drove(bytes: &[u8], turn: &Turn) -> Result<Drove, String> {
    let attempt = AttemptRef {
        attempt_id: "replay".to_owned(),
        session_id: "replay".to_owned(),
        role: String::new(),
        agent_version: String::new(),
    };
    let (lines, _written) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut s = State::new(None, &attempt, turn, lines, 0, true, false);
    s.conn.request(Pending::Initialize, requests::initialize);
    let mut held = Vec::new();
    for step in crate::adapter::replay_steps(bytes) {
        match step {
            Replay::Cancel if !s.cancelled => {
                s.cancelled = true;
                s.begin_stop(None);
            }
            Replay::Cancel => {}
            Replay::Frame(line) => {
                match crate::adapter::at_once(s.on_line(None, &mut held, &line)) {
                    Some(Ok(())) => {}
                    Some(Err(e)) => return Err(e.to_string()),
                    None => return Err("the judging waited".to_owned()),
                }
            }
        }
    }
    s.conn.close();
    match crate::adapter::at_once(s.conclude(Some(crate::adapter::replayed_exit()), &mut held)) {
        Some(Ok(drove)) => Ok(drove),
        Some(Err(e)) => Err(e.to_string()),
        None => Err("the judging waited".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_017_02_handshake_degrade_returns_never_sent_steers() {
        let attempt = AttemptRef {
            attempt_id: "attempt".into(),
            session_id: "session".into(),
            role: "developer".into(),
            agent_version: String::new(),
        };
        let turn = super::super::replay_turn();
        let (lines, _written) = mpsc::unbounded_channel::<Vec<u8>>();
        let mut state = State::new(None, &attempt, &turn, lines, 0, true, false);
        state.degrade = Some("initialize: method not found".into());
        state.conn.close();
        let (reply, mut received) = tokio::sync::oneshot::channel();
        let mut held = vec![Steer {
            input_id: "never-sent".into(),
            text: "steer".into(),
            expected_turn: None,
            reply,
        }];
        assert!(matches!(
            state
                .conclude(Some(crate::adapter::replayed_exit()), &mut held)
                .await,
            Ok(Drove::Degrade(_))
        ));
        assert_eq!(received.try_recv(), Ok(SteerReceipt::NotNow));
        assert!(held.is_empty());
    }

    #[tokio::test]
    async fn a_failure_decided_on_the_handshakes_answer_is_no_degrade() {
        // M2T15R-02 (docs/adr/ADR-030.md decision 9, "首个输入之前的再核对"): a turn whose handshake answer decided its
        // failure (the re-check's COMPAT_UNVERIFIED, a refused initialize) ends with that verdict even if a degrade was
        // also set; a degrade alone still moves the turn to exec. Red before the fix: Degrade.
        let attempt = AttemptRef {
            attempt_id: "attempt".to_owned(),
            session_id: "session".to_owned(),
            role: String::new(),
            agent_version: String::new(),
        };
        let turn = super::super::replay_turn();
        let concluded = |failure: Option<&str>| {
            let (lines, _written) = mpsc::unbounded_channel::<Vec<u8>>();
            let mut s = State::new(None, &attempt, &turn, lines, 0, true, false);
            s.degrade = Some("initialize did not answer within 1s".to_owned());
            s.failure = failure.map(str::to_owned);
            s.conn.close();
            crate::adapter::at_once(
                s.conclude(Some(crate::adapter::replayed_exit()), &mut Vec::new()),
            )
        };
        for failure in [crate::adapter::COMPAT_UNVERIFIED, "initialize: boom"] {
            match concluded(Some(failure)) {
                Some(Ok(Drove::Ended(driven))) => assert_eq!(
                    (driven.state, driven.failure.as_deref()),
                    (AttemptState::Failed, Some(failure))
                ),
                Some(Ok(Drove::Degrade(d))) => panic!("{failure}: degraded ({})", d.reason),
                _ => panic!("{failure}: not concluded"),
            }
        }
        assert!(matches!(concluded(None), Some(Ok(Drove::Degrade(_)))));
    }

    /// The provider-fault recording `case` of codex 0.166.0 (tests/fixtures/protocol/_provider-faults, record_faults.py):
    /// its App Server frames and its meta.json.
    fn fault(case: &str) -> (Vec<Value>, Value) {
        let dir = format!(
            "{}/../../tests/fixtures/protocol/_provider-faults/codex/0.166.0/{case}",
            env!("CARGO_MANIFEST_DIR")
        );
        let frames = std::fs::read_to_string(format!("{dir}/stdout.jsonl"))
            .expect("stdout.jsonl")
            .lines()
            .map(|l| serde_json::from_str(l).expect("frame"))
            .collect();
        let meta = serde_json::from_str(
            &std::fs::read_to_string(format!("{dir}/meta.json")).expect("meta.json"),
        )
        .expect("meta");
        (frames, meta)
    }

    /// `frames` judged as the runner's mode drives a turn: its verdict and when its limit lifts.
    fn judged(frames: &[Value]) -> (AttemptState, Option<std::time::SystemTime>) {
        let bytes: Vec<u8> = frames
            .iter()
            .flat_map(|f| format!("{f}\n").into_bytes())
            .collect();
        match drove(&bytes, &super::super::replay_turn()) {
            Ok(Drove::Ended(driven)) => (driven.state, driven.retry_at),
            Ok(Drove::Degrade(d)) => panic!("degraded: {}", d.reason),
            Err(e) => panic!("not judged: {e}"),
        }
    }

    fn at(secs: u64) -> Option<std::time::SystemTime> {
        Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }

    /// The recorded usage-limit turn with its account/rateLimits/updated frames replaced by `limits` (each a
    /// rateLimits object) and its turn error's codexErrorInfo by `info`.
    fn usage_limit_with(limits: &[Value], info: Value) -> Vec<Value> {
        let (frames, _) = fault("app-server-fault-usage-limit");
        let mut out = Vec::new();
        for mut f in frames {
            if f["method"] == "account/rateLimits/updated" {
                for l in limits {
                    out.push(json!({"method": "account/rateLimits/updated", "params": {"rateLimits": l}}));
                }
                continue;
            }
            if f["method"] == "turn/completed" {
                f["params"]["turn"]["error"]["codexErrorInfo"] = info.clone();
            }
            out.push(f);
        }
        out
    }

    fn window(used: i64, resets_at: Value) -> Value {
        json!({"usedPercent": used, "windowDurationMins": 300, "resetsAt": resets_at})
    }

    #[test]
    fn the_recorded_usage_limit_says_when_it_lifts_and_a_recorded_retry_after_does_not() {
        // M2T07R-256 (REQ-042 behaviour 1): the exhausted window's resetsAt of account/rateLimits/updated is when the
        // usage limit that failed the turn lifts; a 429 or 503 Retry-After codex waited itself reaches no frame.
        let (frames, meta) = fault("app-server-fault-usage-limit");
        let sent = meta["provider"]["requests"][0]["headers"]["x-codex-primary-reset-at"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .expect("the reset sent");
        assert_eq!(judged(&frames), (AttemptState::Failed, at(sent)));
        for case in [
            "app-server-fault-429-retry-after",
            "app-server-fault-503-retry-after",
        ] {
            assert_eq!(
                judged(&fault(case).0),
                (AttemptState::Failed, None),
                "{case}"
            );
        }
    }

    #[test]
    fn only_an_exhausted_window_of_a_limit_failure_says_when() {
        let exhausted = json!({"limitId": "codex", "primary": window(100, json!(2_000)),
                               "secondary": window(40, json!(9_000))});
        let limit = json!("usageLimitExceeded");
        assert_eq!(
            judged(&usage_limit_with(
                std::slice::from_ref(&exhausted),
                limit.clone()
            ))
            .1,
            at(2_000)
        );
        assert_eq!(
            judged(&usage_limit_with(
                std::slice::from_ref(&exhausted),
                json!("rateLimitExceeded")
            ))
            .1,
            at(2_000)
        );
        // A sparse update keeps what an earlier one said (a null window clears nothing).
        let sparse =
            json!({"limitId": "codex", "primary": null, "secondary": window(40, json!(9_500))});
        assert_eq!(
            judged(&usage_limit_with(
                &[exhausted.clone(), sparse],
                limit.clone()
            ))
            .1,
            at(2_000)
        );
        // Every used-up window of the failing quota has to lift: the latest reset of them.
        let both = json!({"limitId": "codex", "primary": window(100, json!(2_000)),
                          "secondary": window(100, json!(9_000))});
        assert_eq!(
            judged(&usage_limit_with(
                std::slice::from_ref(&both),
                limit.clone()
            ))
            .1,
            at(9_000)
        );
        // A window that is not used up, or no update at all, says nothing.
        let partial = json!({"limitId": "codex", "primary": window(99, json!(2_000))});
        assert_eq!(judged(&usage_limit_with(&[partial], limit.clone())).1, None);
        assert_eq!(judged(&usage_limit_with(&[], limit.clone())).1, None);
        // Not a limit failure.
        assert_eq!(
            judged(&usage_limit_with(
                std::slice::from_ref(&exhausted),
                json!("serverOverloaded")
            )),
            (AttemptState::Failed, None)
        );
        assert_eq!(
            judged(&usage_limit_with(
                &[exhausted],
                json!({"responseTooManyFailedAttempts": {"httpStatusCode": 429}})
            ))
            .1,
            None
        );
    }

    /// The recorded usage-limit turn as [`usage_limit_with`], its thread's model (thread/start's answer) taken away.
    fn without_model(mut frames: Vec<Value>) -> Vec<Value> {
        for f in &mut frames {
            if f["id"] == 1 && f["result"].get("model").is_some() {
                f["result"]["model"] = Value::Null;
            }
        }
        frames
    }

    #[test]
    fn the_reset_is_that_of_the_quota_the_turn_failed_on() {
        // RA-01 (M2T07R-256 review): only the quotas that apply to the turn count: the account's own (no model slug)
        // and one whose normalModelSlug is the thread's model (gpt-fake-5 in the recording); another model's quota is
        // unrelated, whatever it says. Where the failing quota cannot be told, there is no reset (full jitter).
        let limit = json!("usageLimitExceeded");
        let account = json!({"limitId": "codex", "primary": window(100, json!(120))});
        let other_model = |reset: Value| json!({"limitId": "codex_other", "normalModelSlug": "gpt-other", "primary": window(100, reset)});
        // An unrelated model's later reset does not win (Codex's probe: T+120 against T+86400) ...
        assert_eq!(
            judged(&usage_limit_with(
                &[account.clone(), other_model(json!(86_400))],
                limit.clone()
            ))
            .1,
            at(120)
        );
        // ... nor does its used-up window without a reset suppress the account's.
        assert_eq!(
            judged(&usage_limit_with(
                &[account.clone(), other_model(Value::Null)],
                limit.clone()
            ))
            .1,
            at(120)
        );
        // Another model's quota alone says nothing about this turn.
        assert_eq!(
            judged(&usage_limit_with(
                &[other_model(json!(86_400))],
                limit.clone()
            ))
            .1,
            None
        );
        // The thread's own model quota alone applies. With the account's used up too there are two candidates, and
        // which one failed the turn cannot be told: none (design correction rule 1; RA-01's "both have to lift" was
        // the wrong expectation).
        let own_model = json!({"limitId": "codex_fake", "normalModelSlug": "gpt-fake-5",
                               "primary": window(100, json!(500))});
        assert_eq!(
            judged(&usage_limit_with(
                std::slice::from_ref(&own_model),
                limit.clone()
            ))
            .1,
            at(500)
        );
        assert_eq!(
            judged(&usage_limit_with(
                &[account.clone(), own_model.clone()],
                limit.clone()
            ))
            .1,
            None
        );
        // Two quotas of the account's own kind used up: which one failed the turn cannot be told.
        let second = json!({"limitId": "codex_second", "primary": window(100, json!(86_400))});
        assert_eq!(
            judged(&usage_limit_with(&[account.clone(), second], limit.clone())).1,
            None
        );
        // With the thread's model unknown, a model quota used up cannot be placed: no reset; the account's alone
        // still applies.
        assert_eq!(
            judged(&without_model(usage_limit_with(
                &[account.clone(), own_model],
                limit.clone()
            )))
            .1,
            None
        );
        assert_eq!(
            judged(&without_model(usage_limit_with(
                std::slice::from_ref(&account),
                limit
            )))
            .1,
            at(120)
        );
    }

    #[test]
    fn a_quota_of_unknown_attribution_gives_no_reset() {
        // RA-100 (M2T07R-256 review round 2; design correction rule 1): a quota is the account's only as limitId
        // `codex` with no model ever reported, and the thread's model's only by its (merged) normalModelSlug. A sparse
        // update that leaves the slug out proves neither, so a used-up quota so reported makes the reset unknown.
        let limit = json!("usageLimitExceeded");
        let own_model = json!({"limitId": "codex_fake", "normalModelSlug": "gpt-fake-5",
                               "primary": window(100, json!(120))});
        let unknown = json!({"limitId": "codex_x", "primary": window(100, json!(86_400))});
        // Codex's probes: the active model's T+120 and an unidentified T+86400; the unidentified one alone.
        assert_eq!(
            judged(&usage_limit_with(
                &[own_model.clone(), unknown.clone()],
                limit.clone()
            ))
            .1,
            None
        );
        assert_eq!(
            judged(&usage_limit_with(
                std::slice::from_ref(&unknown),
                limit.clone()
            ))
            .1,
            None
        );
        // A slug reported once is kept by a later sparse update of the same limit.
        let earlier = json!({"limitId": "codex_fake", "normalModelSlug": "gpt-fake-5",
                             "primary": window(40, json!(500))});
        let sparse = json!({"limitId": "codex_fake", "primary": window(100, json!(500))});
        assert_eq!(
            judged(&usage_limit_with(&[earlier, sparse], limit.clone())).1,
            at(500)
        );
        // `codex` that reports a model is that model's quota, not the account's.
        let named = json!({"limitId": "codex", "normalModelSlug": "gpt-other", "primary": window(100, json!(120))});
        assert_eq!(judged(&usage_limit_with(&[named], limit)).1, None);
    }

    #[test]
    fn a_reset_that_is_no_instant_or_a_turn_that_did_not_fail_on_it_says_nothing() {
        for resets_at in [
            json!(0),
            json!(-5),
            json!(1.5),
            json!("2000"),
            json!(null),
            json!(u64::MAX),
        ] {
            let l = json!({"limitId": "codex", "primary": window(100, resets_at.clone())});
            assert_eq!(
                judged(&usage_limit_with(&[l], json!("usageLimitExceeded"))).1,
                None,
                "{resets_at}"
            );
        }
        let exhausted = json!({"limitId": "codex", "primary": window(100, json!(2_000))});
        // A turn that completed after all.
        let mut completed = usage_limit_with(
            std::slice::from_ref(&exhausted),
            json!("usageLimitExceeded"),
        );
        for f in &mut completed {
            if f["method"] == "turn/completed" {
                f["params"]["turn"]["status"] = json!("completed");
                f["params"]["turn"]["error"] = Value::Null;
            }
        }
        assert_eq!(judged(&completed), (AttemptState::Settled, None));
        // A stream cut short before turn/completed.
        let mut cut = usage_limit_with(&[exhausted], json!("usageLimitExceeded"));
        cut.retain(|f| f["method"] != "turn/completed");
        assert_eq!(judged(&cut).1, None);
    }
}
