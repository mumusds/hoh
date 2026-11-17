//! What an adapter observes outside the agent's stream, the input of its replay besides the recorded stdout
//! (docs/adr/ADR-032.md design correction 7 item 2, "输入契约"): recorded with a live turn and injected in a replay, so
//! that live and replay judge the same values. Every required observation must be there with its JSON type, and the
//! field sets are closed; anything missing, ill-typed or unknown is NOT_OBSERVED, never a default (M2T20D-900, -950,
//! -1001). Types are checked before values, as serde_json's `Value` tells them apart: a boolean is never a number, an
//! integer never a float.

use crate::Profile;
use crate::process::Termination;
use serde_json::{Map, Value};
use std::path::PathBuf;

/// The failure reason of a replay whose observations do not allow it.
pub const NOT_OBSERVED: &str = "NOT_OBSERVED";

/// A session file as pi.rs identifies it: canonical path, device, inode.
pub use crate::pi::SessionFile;

/// The launch a codex App Server turn starts from: its profile, the thread it resumes, its model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLaunch {
    pub profile: Profile,
    pub resume: Option<String>,
    pub model: Option<String>,
}

/// The launch a pi turn starts from, and for a resumed session the files noted before the start and the identity of
/// the file get_state named when its answer arrived (`None`: no such file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiLaunch {
    pub session_id: String,
    pub resume: bool,
    pub noted: Vec<SessionFile>,
    pub opened: Option<SessionFile>,
}

/// The observations of one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// The agent's version as the host probed it (`--version`).
    pub version: String,
    /// How the process ended: `None` without an exit status.
    pub exit: Option<Termination>,
    /// Both stdout and stderr were read to their end without a read error.
    pub logs_complete: bool,
    /// claude only: every byte of the prompt was written before stdin closed (claude.rs `fed`).
    pub prompt_fed: Option<bool>,
    pub codex: Option<CodexLaunch>,
    pub pi: Option<PiLaunch>,
}

/// The agents whose turns are replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Pi,
}

/// Recorded but read by no adapter, so neither required nor compared (named in the contract): the recorder's
/// description of how the process ended, and pi's `prompt_sent` and `get_state_session_file`.
const EXIT_FIELDS: &[&str] = &["returncode", "how"];

fn keys(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["version", "exit", "logs_complete", "prompt_fed"],
        Agent::Codex => &["version", "exit", "logs_complete", "launch"],
        Agent::Pi => &[
            "version",
            "exit",
            "logs_complete",
            "launch",
            "noted",
            "opened",
            "prompt_sent",
            "get_state_session_file",
        ],
    }
}

/// Collects what is wrong with the observations, by field path.
#[derive(Default)]
struct Problems(Vec<String>);

impl Problems {
    fn missing(&mut self, path: &str) {
        self.0.push(format!("{path} missing"));
    }
    fn ill_typed(&mut self, path: &str) {
        self.0.push(format!("{path} ill-typed"));
    }
    fn unknown(&mut self, path: &str) {
        self.0.push(format!("{path} unknown"));
    }
    /// The field `key` of `object` as `read` takes it, recording a missing or ill-typed one.
    fn field<T>(
        &mut self,
        object: &Map<String, Value>,
        path: &str,
        key: &str,
        read: impl FnOnce(&Value) -> Option<T>,
    ) -> Option<T> {
        let name = if path.is_empty() {
            key.to_owned()
        } else {
            format!("{path}.{key}")
        };
        match object.get(key) {
            None => {
                self.missing(&name);
                None
            }
            Some(v) => {
                let value = read(v);
                if value.is_none() {
                    self.ill_typed(&name);
                }
                value
            }
        }
    }
    /// The keys of `object` outside `known`, each unknown.
    fn closed(&mut self, object: &Map<String, Value>, path: &str, known: &[&str]) {
        for key in object.keys().filter(|k| !known.contains(&k.as_str())) {
            self.unknown(&if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            });
        }
    }
}

/// A non-empty string.
fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// null, or a non-empty string.
fn optional_text(v: &Value) -> Option<Option<String>> {
    if v.is_null() {
        Some(None)
    } else {
        text(v).map(Some)
    }
}

/// `[canonical path, device, inode]`.
fn session_file(v: &Value) -> Option<SessionFile> {
    match v.as_array()?.as_slice() {
        [path, dev, ino] => Some((PathBuf::from(text(path)?), dev.as_u64()?, ino.as_u64()?)),
        _ => None,
    }
}

/// The exit status: an integer code (negative: the signal that ended the process), or null for none.
fn exit_status(v: &Value) -> Option<Option<Termination>> {
    if v.is_null() {
        return Some(None);
    }
    let code = v.as_i64().filter(|_| v.is_i64())?;
    let code = i32::try_from(code).ok()?;
    Some(Some(if code < 0 {
        Termination::Signal(-code)
    } else {
        Termination::Code(code)
    }))
}

/// The observations of a turn of `agent`, or `NOT_OBSERVED (<every problem>)`.
pub fn parse(agent: Agent, observations: &Value) -> Result<Observed, String> {
    let mut p = Problems::default();
    let Some(obs) = observations.as_object() else {
        return Err(format!("{NOT_OBSERVED} (the observations are no object)"));
    };
    p.closed(obs, "", keys(agent));
    let version = p.field(obs, "", "version", text);
    let exit = p.field(obs, "", "exit", |v| v.as_object().cloned());
    let status = exit.as_ref().and_then(|exit| {
        p.closed(exit, "exit", EXIT_FIELDS);
        p.field(exit, "exit", "returncode", exit_status)
    });
    let logs_complete = p.field(obs, "", "logs_complete", Value::as_bool);
    let prompt_fed = match agent {
        Agent::Claude => p.field(obs, "", "prompt_fed", Value::as_bool),
        _ => None,
    };
    let mut codex = None;
    let mut pi = None;
    match agent {
        Agent::Claude => {}
        Agent::Codex => {
            if let Some(launch) = p.field(obs, "", "launch", |v| v.as_object().cloned()) {
                p.closed(&launch, "launch", &["profile", "resume", "model"]);
                let profile = p.field(&launch, "launch", "profile", |v| match v.as_str()? {
                    "read_only" => Some(Profile::ReadOnly),
                    "yolo" => Some(Profile::Yolo),
                    _ => None,
                });
                let resume = p.field(&launch, "launch", "resume", optional_text);
                let model = p.field(&launch, "launch", "model", optional_text);
                if let (Some(profile), Some(resume), Some(model)) = (profile, resume, model) {
                    codex = Some(CodexLaunch {
                        profile,
                        resume,
                        model,
                    });
                }
            }
        }
        Agent::Pi => {
            if let Some(launch) = p.field(obs, "", "launch", |v| v.as_object().cloned()) {
                p.closed(&launch, "launch", &["session_id", "resume"]);
                let session_id = p.field(&launch, "launch", "session_id", text);
                let resume = p.field(&launch, "launch", "resume", Value::as_bool);
                // A resumed session reopens a noted file: both observations are required then, read only then.
                let (noted, opened) = if resume == Some(true) {
                    (
                        p.field(obs, "", "noted", |v| {
                            v.as_array()?.iter().map(session_file).collect()
                        }),
                        p.field(obs, "", "opened", |v| {
                            if v.is_null() {
                                Some(None)
                            } else {
                                session_file(v).map(Some)
                            }
                        }),
                    )
                } else {
                    (Some(Vec::new()), Some(None))
                };
                if let (Some(session_id), Some(resume), Some(noted), Some(opened)) =
                    (session_id, resume, noted, opened)
                {
                    pi = Some(PiLaunch {
                        session_id,
                        resume,
                        noted,
                        opened,
                    });
                }
            }
        }
    }
    if !p.0.is_empty() {
        return Err(format!("{NOT_OBSERVED} ({})", p.0.join(", ")));
    }
    match (version, status, logs_complete) {
        (Some(version), Some(exit), Some(logs_complete)) => Ok(Observed {
            version,
            exit,
            logs_complete,
            prompt_fed,
            codex,
            pi,
        }),
        _ => Err(format!("{NOT_OBSERVED} (incomplete)")),
    }
}

/// What a product adapter's replay of a recorded turn gives the judge (docs/adr/ADR-032.md design correction 7,
/// `replay_facts`): the adapter's verdict and final message, the native session, for codex the thread and turn it took
/// as this turn's and the thread request's correlated result, what it recorded frame by frame, and every message it
/// wrote to the agent (claude writes only the prompt: `None`). The same code path as the host's.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayFacts {
    pub state: hoh_core::lifecycle::AttemptState,
    pub failure: Option<String>,
    pub final_text: Option<String>,
    pub native_session: Option<String>,
    pub thread: Option<String>,
    pub turn: Option<String>,
    pub thread_result: Option<Value>,
    pub recorded: Vec<crate::adapter::Recorded>,
    pub sent: Option<Vec<Value>>,
}

impl ReplayFacts {
    /// A replay that cannot run as the adapter would: never settled.
    pub fn failed(why: String) -> ReplayFacts {
        ReplayFacts {
            state: hoh_core::lifecycle::AttemptState::Failed,
            failure: Some(why),
            final_text: None,
            native_session: None,
            thread: None,
            turn: None,
            thread_result: None,
            recorded: Vec::new(),
            sent: None,
        }
    }
}

/// The value the comparison contract puts in place of a field it does not compare.
const NOT_COMPARED: &str = "<not compared>";

/// `message` with the fields the comparison contract does not compare replaced (docs/adr/ADR-032.md design correction 7
/// item 2): initialize's `params.clientInfo.version` (a non-empty string: the hoh build that sent it), turn/start's
/// `params.input[0].text` and pi's prompt `message` (a string: the turn's prompt). Each only where it has that type.
fn comparable(message: &Value) -> Value {
    let mut m = message.clone();
    if m["method"] == "initialize"
        && m["params"]["clientInfo"]["version"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    {
        m["params"]["clientInfo"]["version"] = Value::from(NOT_COMPARED);
    }
    if m["method"] == "turn/start"
        && m["params"]["input"]
            .as_array()
            .is_some_and(|i| i.len() == 1)
        && m["params"]["input"][0]["text"].is_string()
    {
        m["params"]["input"][0]["text"] = Value::from(NOT_COMPARED);
    }
    if m["type"] == "prompt" && m["message"].is_string() {
        m["message"] = Value::from(NOT_COMPARED);
    }
    m
}

/// Why the recorded request log is not the messages the adapter wrote in its replay, or `None` (docs/adr/ADR-032.md
/// design correction 7 item 2, the comparison contract): the same number, in the same order, each equal as typed JSON
/// (serde_json's `Value`: a boolean never equals a number, an integer never a float) key for key and value for value,
/// but for the two fields [`comparable`] names.
pub fn request_log_problem(sent: &[Value], log: &[Value]) -> Option<String> {
    if sent.len() != log.len() {
        return Some(format!(
            "the request log is not the product's own ({} messages, the adapter sends {})",
            log.len(),
            sent.len()
        ));
    }
    sent.iter()
        .zip(log)
        .position(|(a, b)| comparable(a) != comparable(b))
        .map(|i| format!("the request log is not the product's own (message {i})"))
}

/// The process's end as the replay injects it: `exit`, and both streams read to their end or not.
pub(crate) fn exit_report(observed: &Observed) -> crate::process::ExitReport {
    let stream = crate::process::StreamReport {
        eof: observed.logs_complete,
        ..crate::process::StreamReport::default()
    };
    crate::process::ExitReport {
        pid: 0,
        status: observed.exit,
        cause: crate::process::Cause::Exited,
        steps: Vec::new(),
        orphans: Some(0),
        group_reaped: true,
        stdout: stream.clone(),
        stderr: stream,
        stderr_tail: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
