//! Following a run's events (docs/adr/ADR-031.md decisions 9, 10 and 12): which event ends a follow, which events
//! replay history, and the loop that shows them. A follow only reads and unsubscribes: it sends no command (E8).

use crate::cli::Output;
use crate::client::{self, HostInfo};
use crate::exit::{ClientEvent, client_code};
use crate::forward::Stderr;
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The terminal run states: no way out of them (REQ-036 behaviour 3).
pub const TERMINAL: [&str; 3] = ["COMPLETED", "FAILED", "CANCELLED"];

/// When a follow ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// `hoh -p`: at the first `run.result` after the acceptance event of the client's last accepted command, the
    /// first event whose `payload.command_id` is `command_id` (REQ-002's decision, E7).
    Accepted { command_id: String },
    /// `hoh attach`: at the first `run.result` past the watermark `w0` the attach began at, or at one in a terminal
    /// state, after which a run has no event (E2); never at an older result of a run that stopped and may go on.
    Attached { w0: i64 },
}

/// Which events of a follow replay history (REQ-002's decision; CR-022): they carry `"replay": true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replay {
    /// None: a new run's follow, or an output that is not stream-json.
    None,
    /// The ones before the acceptance event of the client's first accepted command (`payload.command_id`).
    Before { command_id: String },
    /// The ones at or before the watermark the attach began at.
    UpTo(i64),
}

/// What became of an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    /// Its seq is not past the last one shown (or it has none): dropped.
    Dropped(Option<i64>),
    /// To be shown, marked when it replays history; `ends` when it ends the follow.
    Shown { event: Value, ends: bool },
}

/// A follow's account of what it has seen.
#[derive(Debug, Clone)]
pub struct Watch {
    end: End,
    replay: Replay,
    last: i64,
    accepted: Option<Value>,
    first: Option<Value>,
    results: usize,
}

impl Watch {
    /// A follow of the events after seq `after`.
    pub fn new(end: End, replay: Replay, after: i64) -> Watch {
        Watch {
            end,
            replay,
            last: after,
            accepted: None,
            first: None,
            results: 0,
        }
    }

    /// The next event of the stream.
    pub fn see(&mut self, mut event: Value) -> Seen {
        let seq = event["seq"].as_i64();
        match seq {
            Some(seq) if seq > self.last => self.last = seq,
            _ => return Seen::Dropped(seq),
        }
        let seq = self.last;
        let result = event["kind"] == "run.result";
        let command = event["payload"]["command_id"].as_str().map(str::to_owned);
        let replays = match &self.replay {
            Replay::None => false,
            Replay::Before { command_id } => {
                if self.first.is_none() && !result && command.as_deref() == Some(command_id) {
                    self.first = Some(event.clone());
                }
                self.first.is_none()
            }
            Replay::UpTo(w0) => seq <= *w0,
        };
        let ends = match &self.end {
            End::Accepted { command_id } => {
                if self.accepted.is_none() && !result && command.as_deref() == Some(command_id) {
                    self.accepted = Some(event.clone());
                }
                result
                    && self
                        .accepted
                        .as_ref()
                        .is_some_and(|a| a["seq"].as_i64() < Some(seq))
            }
            End::Attached { w0 } => {
                result
                    && (seq > *w0
                        || event["payload"]["state"]
                            .as_str()
                            .is_some_and(|state| TERMINAL.contains(&state)))
            }
        };
        if result {
            self.results += 1;
        }
        if replays {
            event["replay"] = json!(true);
        }
        Seen::Shown { event, ends }
    }

    /// The seq of the last event shown (or where the follow began).
    pub fn last(&self) -> i64 {
        self.last
    }

    /// How many `run.result` events were shown.
    pub fn results(&self) -> usize {
        self.results
    }

    /// The acceptance event of the client's first accepted command, once seen (`Replay::Before`).
    pub fn first(&self) -> Option<&Value> {
        self.first.as_ref()
    }

    /// The acceptance event of the command the follow ends after, once seen (`End::Accepted`).
    #[cfg(test)]
    pub fn accepted(&self) -> Option<&Value> {
        self.accepted.as_ref()
    }
}

/// How a follow ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// At a `run.result`: its payload.
    Result(Value),
    /// A run that ended before run results were written, in this terminal state (the quiet fallback, decision 12).
    Ended(String),
    /// The client's own ending: a signal (130, 143, 129), the host (69) or stdout (74).
    Code(u8),
}

/// How long a follow of a run that has shown no `run.result` waits with nothing new before it asks whether the run
/// ended (a run that ended before run results were written): long after any replay of its events.
const QUIET: Duration = Duration::from_secs(5);

/// Write `text` to stdout, flushed: a closed stdout (EPIPE) is an error, never a panic.
pub fn emit(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}

/// A follow of run `run_id`.
pub struct Follower<'a> {
    pub project: &'a Path,
    pub run_id: &'a str,
    pub output: Output,
    pub signals: &'a AtomicUsize,
    pub stderr: &'a Stderr,
    /// Whether a run that shows no `run.result` ends when it is found ended after [`QUIET`] (attach only).
    pub quiet: bool,
}

impl Follower<'_> {
    /// Follow the run after `watch`'s last seq until `watch` ends it, a signal comes (130, 143, 129), the host is gone
    /// for good (69), an event cannot be read (69: never a gap) or stdout cannot be written (74). The host found again
    /// after it went is followed on after the last seq shown.
    pub fn follow(&self, mut host: HostInfo, watch: &mut Watch) -> Outcome {
        let mut stream: Option<client::Events> = None;
        let mut checked = Instant::now();
        let mut quiet_since = Instant::now();
        loop {
            let signal = self.signals.load(Ordering::SeqCst);
            if signal != 0 {
                return Outcome::Code(u8::try_from(signal).unwrap_or(1));
            }
            let events = match stream.as_mut() {
                Some(events) => events,
                None => match client::Events::open(&host, self.run_id, watch.last()) {
                    Ok(events) => stream.insert(events),
                    Err(_) => {
                        // The host went: the next one is found or started, and the stream goes on after the last.
                        std::thread::sleep(Duration::from_millis(200));
                        match client::ensure(self.project) {
                            Ok(again) => host = again,
                            Err(failure) => {
                                self.stderr
                                    .say(&format!("hoh: {}: {}", failure.code, failure.message));
                                return Outcome::Code(failure.exit);
                            }
                        }
                        continue;
                    }
                },
            };
            match events.next() {
                Ok(Some(event)) => {
                    quiet_since = Instant::now();
                    match watch.see(event) {
                        Seen::Dropped(seq) => self.stderr.say(&format!(
                            "hoh: 丢弃了 seq 不在 {} 之后的事件（seq {}）",
                            watch.last(),
                            seq.map_or_else(|| "无".to_owned(), |s| s.to_string())
                        )),
                        Seen::Shown { event, ends } => {
                            if let Err(code) = self.show(&event) {
                                return Outcome::Code(code);
                            }
                            if ends {
                                return Outcome::Result(event["payload"].clone());
                            }
                        }
                    }
                }
                Ok(None) => {
                    if self.quiet
                        && watch.results() == 0
                        && quiet_since.elapsed() >= QUIET
                        && checked.elapsed() >= Duration::from_secs(1)
                    {
                        checked = Instant::now();
                        if let Ok(Some(run)) = client::get_run(&host, self.run_id)
                            && let Some(state) = run["state"].as_str()
                            && TERMINAL.contains(&state)
                        {
                            return Outcome::Ended(state.to_owned());
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    self.stderr.say(&format!(
                        "hoh: {e}；停止跟随，不跳过它（Run {}）",
                        self.run_id
                    ));
                    return Outcome::Code(client_code(ClientEvent::HostUnavailable));
                }
                Err(_) => stream = None,
            }
        }
    }

    /// Show `event` as the output says; stdout that cannot be written ends the follow with 74.
    fn show(&self, event: &Value) -> Result<(), u8> {
        for attempt in [
            event["attempt_id"].as_str(),
            event["payload"]["attempt_id"].as_str(),
        ]
        .into_iter()
        .flatten()
        {
            self.stderr.attempt(attempt);
        }
        let kind = event["kind"].as_str().unwrap_or_default();
        if kind == "run.waiting" {
            self.stderr.say(&format!(
                "hoh: 等待外部恢复，下次重试 {}",
                event["payload"]["next_retry_at"].as_str().unwrap_or("未知")
            ));
        }
        match self.output {
            Output::StreamJson => {
                let written = crate::output::json_line(event)
                    .map_err(std::io::Error::other)
                    .and_then(|line| emit(&format!("{line}\n")));
                if let Err(e) = written {
                    self.stderr.say(&format!(
                        "hoh: stdout 无法写入（{e}）；停止跟随，Run 不受影响"
                    ));
                    return Err(client_code(ClientEvent::TerminalIo));
                }
            }
            Output::Text => self.stderr.say(&format!(
                "#{} {kind}",
                event["seq"].as_i64().unwrap_or_default()
            )),
            Output::Json => {}
        }
        Ok(())
    }
}

/// The exit code a follow's outcome ends the client with, once its result is printed as `output` says (json: the
/// result object; text: [`crate::output::text_line`]; stream-json: already shown). A signal is said with the run and
/// how to attach again: nothing was sent (E8).
pub fn conclude(outcome: &Outcome, output: Output, run_id: &str, stderr: &Stderr) -> u8 {
    let printed = |text: String| match emit(&text) {
        Ok(()) => None,
        Err(e) => {
            stderr.say(&format!("hoh: stdout 无法写入（{e}）；Run 不受影响"));
            Some(client_code(ClientEvent::TerminalIo))
        }
    };
    match outcome {
        Outcome::Result(result) => {
            let code = result["exit_code"]
                .as_u64()
                .and_then(|c| u8::try_from(c).ok())
                .or_else(|| crate::exit::run_code(result["state"].as_str().unwrap_or_default()))
                .unwrap_or(1);
            let failed = match output {
                Output::Json => match crate::output::json_line(result) {
                    Ok(line) => printed(format!("{line}\n")),
                    Err(e) => {
                        stderr.say(&format!("hoh: 结果无法序列化：{e}"));
                        Some(1)
                    }
                },
                Output::Text => printed(format!("{}\n", crate::output::text_line(result))),
                Output::StreamJson => None,
            };
            failed.unwrap_or(code)
        }
        Outcome::Ended(state) => {
            let code = crate::exit::run_code(state).unwrap_or(1);
            let failed = match output {
                Output::Text => printed(format!("{run_id} {state}\n")),
                Output::Json | Output::StreamJson => None,
            };
            failed.unwrap_or(code)
        }
        Outcome::Code(code @ (129 | 130 | 143)) => {
            stderr.say(&format!(
                "hoh: 已脱离 Run {run_id}（它在后台继续，没有发出任何命令）；用 hoh attach {run_id} 重新附着"
            ));
            *code
        }
        Outcome::Code(code) => *code,
    }
}

#[cfg(test)]
mod tests;
