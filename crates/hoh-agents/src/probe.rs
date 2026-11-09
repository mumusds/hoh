//! `probe()`: what an installed agent CLI is and what it can do, from its launcher, `--version`, `--help` and native
//! schema (REQ-027, docs/13-adapters.md). Capabilities come from what was probed, never from the brand; what the
//! probe cannot establish stays `None` for the compat suite to decide. Probe commands are short, run without stdin,
//! with a timeout, in their own process group, which is killed on timeout (the full process substrate is M1-T02).

use crate::compat::{Agent, AgentEntry, Suite, SuiteStatus, Transport};
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, kill_process, kill_process_group, waitid,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Capability keys every entry carries.
pub const CAPABILITIES: &[&str] = &[
    "structured_events",
    "resume",
    "mid_turn_steer",
    "partial_messages",
    "custom_tools",
    "images",
    "browser",
    "ui_dialogs",
    "token_usage",
];

/// Where and how to probe.
#[derive(Debug, Clone)]
pub struct ProbeEnv {
    /// PATH used to find the launcher and given to the probe commands.
    pub path: OsString,
    /// `tests/fixtures/protocol`-style root (`<agent>/<version>/`) whose tree hash is the suite's fixture set.
    pub fixture_root: Option<PathBuf>,
    /// Limit for each probe command.
    pub timeout: Duration,
    /// Where every command run is written down, if anywhere (doctor lists them, REQ-006).
    pub log: Option<RunLog>,
    /// A limit shared by every command: each gets the smaller of `timeout` and the time left before it, so commands
    /// run one after another end together by then (doctor's total limit, REQ-006).
    pub deadline: Option<Instant>,
    /// How whoever probes stops (the host's stop): every command a probe started is killed, by the stop or by its starter
    /// as it puts the command in its slot, and no other starts; the window before that is stated in docs/adr/ADR-022.md
    /// decision 10 v5 (M2T07R-202, -300, -450, -500, -600).
    pub cancel: Option<ProbeStop>,
}

/// The stop of whoever probes (docs/adr/ADR-022.md decision 10 v5, M2T07R-300, -450, -500, -600):
/// [`ProbeStop::stop`] kills every command already in its slot and not reaped, its process group and its leader,
/// before it returns, and never waits for a start: a start registers an empty slot under the stop's lock (none once
/// stopped) and spawns outside it; once spawned, its starter puts the child in its slot and looks at the stop in one
/// hold of the slot's lock, so the stop or the starter kills it, whichever comes second. Until its starter lets go of
/// that lock, its pid is known to its starter alone (the residual of decision 10 v5). The starter still reaps it,
/// taking it out of its slot first.
#[derive(Debug, Clone, Default)]
pub struct ProbeStop(std::sync::Arc<Stopping>);

#[derive(Debug, Default)]
struct Stopping {
    /// Set under `commands`' lock; read without it.
    stopped: std::sync::atomic::AtomicBool,
    /// The commands started, each held until it is reaped.
    commands: std::sync::Mutex<Vec<std::sync::Weak<std::sync::Mutex<Option<std::process::Child>>>>>,
}

impl ProbeStop {
    /// Stop: kill every command in its slot and not reaped (its group and its leader), and start none from now on; a
    /// command still being spawned is killed by its starter as it puts it in its slot.
    pub fn stop(&self) {
        let mut commands = locked(&self.0.commands);
        self.0
            .stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for command in commands.drain(..).filter_map(|c| c.upgrade()) {
            // Its leader not reaped (whoever reaps it takes it out first), the number is still its own.
            if let Some(child) = locked(&command).as_ref() {
                let pid = Pid::from_child(child);
                let _ = kill_process_group(pid, Signal::KILL);
                let _ = kill_process(pid, Signal::KILL);
            }
        }
    }

    /// Whether [`ProbeStop::stop`] was called.
    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `spawn` run unless stopped, in a slot registered for the stop before it: `None` when stopped. The spawn runs
    /// outside the stop's lock, so a stop never waits for an exec (M2T07R-450); a caller that finds the stop once this
    /// returns ends what it started. `at` is where a test holds the starter once its spawn returned and once the child
    /// is in its slot (see [`between`]).
    fn start(
        &self,
        at: &Path,
        spawn: impl FnOnce() -> io::Result<std::process::Child>,
    ) -> Option<io::Result<Started>> {
        let slot: Slot = std::sync::Arc::default();
        {
            let mut commands = locked(&self.0.commands);
            if self.is_stopped() {
                return None;
            }
            commands.retain(|c| c.strong_count() > 0);
            commands.push(std::sync::Arc::downgrade(&slot));
        }
        Some(spawn().map(|child| {
            between("spawned", at);
            let started = Started::new(slot, child, Some(self));
            between("placed", at);
            started
        }))
    }
}

/// A started command's child, until whoever reaps it takes it out (a stop signals it only while it is there).
type Slot = std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>;

/// A command just started: its child in its slot, its pid and its outputs.
struct Started {
    slot: Slot,
    pid: Pid,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
}

impl Started {
    /// `child` put in `slot`, where a stop finds it from now on; with `stop`, the starter's half of the handoff: the
    /// child put in its slot and the stop looked at in one hold of the slot's lock, killed there when stopped. The stop
    /// sets its flag first and takes each slot's lock after, so one of the two kills it whichever comes second, and
    /// once the lock is let go nothing waits on the starter running again (decision 10 v5, M2T07R-500, -600).
    fn new(slot: Slot, mut child: std::process::Child, stop: Option<&ProbeStop>) -> Started {
        let pid = Pid::from_child(&child);
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let mut held = locked(&slot);
        *held = Some(child);
        if stop.is_some_and(ProbeStop::is_stopped) {
            let _ = kill_process_group(pid, Signal::KILL);
            let _ = kill_process(pid, Signal::KILL);
        }
        drop(held);
        Started {
            slot,
            pid,
            stdout,
            stderr,
        }
    }
}

/// `mutex` locked, whatever a panic left in it.
fn locked<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One command a probe ran.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Run {
    pub argv: Vec<String>,
    /// `None` when it did not start, timed out or was ended by a signal.
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub timed_out: bool,
    /// `false` when the shared deadline had passed before it (it was not run).
    pub started: bool,
}

/// The commands run so far, shared between the threads that probe.
pub type RunLog = std::sync::Arc<std::sync::Mutex<Vec<Run>>>;

/// Why a probe failed.
#[derive(Debug)]
pub enum ProbeError {
    /// The CLI is not on PATH.
    NotFound {
        agent: Agent,
    },
    /// A probe command did not end within its limit; its process group was killed.
    TimedOut {
        argv: Vec<String>,
        after: Duration,
        /// The limit was the time left before the shared deadline, shorter than the command's own timeout.
        cut_by_deadline: bool,
    },
    /// The shared deadline had passed before the command, so it was not started.
    NotStarted {
        argv: Vec<String>,
    },
    /// A probe command could not be run or failed.
    Failed {
        argv: Vec<String>,
        detail: String,
    },
    /// The CLI lacks what HoH needs to drive it.
    Unsupported {
        agent: Agent,
        detail: String,
    },
    Io(io::Error),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { agent } => write!(f, "{} is not on PATH", agent.command()),
            Self::TimedOut {
                argv,
                after,
                cut_by_deadline,
            } => write!(
                f,
                "{}: timed out after {after:?}{}",
                argv.join(" "),
                if *cut_by_deadline {
                    " (what was left of the shared limit)"
                } else {
                    ""
                }
            ),
            Self::NotStarted { argv } => {
                write!(
                    f,
                    "{}: not started: the shared limit was used up",
                    argv.join(" ")
                )
            }
            Self::Failed { argv, detail } => write!(f, "{}: {detail}", argv.join(" ")),
            Self::Unsupported { agent, detail } => {
                write!(f, "{} cannot be driven: {detail}", agent.command())
            }
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProbeError {}

impl From<io::Error> for ProbeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// What an installed agent CLI is, before what it can do is probed: its launcher and implementation with their sha256,
/// and its `--version` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub launcher: PathBuf,
    pub launcher_sha256: String,
    pub implementation: PathBuf,
    pub implementation_sha256: String,
    pub cli_version: String,
}

/// Find the agent's CLI and identify it: launcher, implementation, `--version`.
pub fn identify(agent: Agent, env: &ProbeEnv) -> Result<Identity, ProbeError> {
    let launcher =
        find_on_path(agent.command(), &env.path).ok_or(ProbeError::NotFound { agent })?;
    let implementation = resolve_implementation(agent, &launcher)?;
    let cli_version = version_line(&launcher, env)?;
    Ok(Identity {
        launcher_sha256: file_sha256(&launcher)?,
        implementation_sha256: file_sha256(&implementation)?,
        launcher,
        implementation,
        cli_version,
    })
}

/// Probe one agent: the entry it would get in the compat manifest (suite not run).
pub fn probe(agent: Agent, env: &ProbeEnv) -> Result<AgentEntry, ProbeError> {
    probe_identified(agent, identify(agent, env)?, env)
}

/// Probe what an identified agent CLI can do (its help and schema), so a caller that is refused here still knows
/// what was installed (doctor reports its version and hash, M1QA-08).
pub fn probe_identified(
    agent: Agent,
    identity: Identity,
    env: &ProbeEnv,
) -> Result<AgentEntry, ProbeError> {
    let mut capabilities: BTreeMap<String, Option<bool>> = CAPABILITIES
        .iter()
        .map(|c| ((*c).to_owned(), None))
        .collect();
    let launcher = &identity.launcher;
    let (transport, help, schema_sha256) = match agent {
        Agent::Codex => probe_codex(launcher, env, &mut capabilities)?,
        Agent::Claude => probe_claude(launcher, env, &mut capabilities)?,
        Agent::Pi => probe_pi(launcher, env, &mut capabilities)?,
    };
    Ok(AgentEntry {
        launcher_sha256: identity.launcher_sha256,
        launcher_path: identity.launcher.display().to_string(),
        implementation_sha256: identity.implementation_sha256,
        implementation_path: identity.implementation.display().to_string(),
        suite: Suite {
            status: SuiteStatus::NotRun,
            fixture_set_sha256: fixture_set_sha256(env, agent, &identity.cli_version)?,
            run_at: None,
            cases: None,
        },
        cli_version: identity.cli_version,
        transport,
        help_sha256: hex(&Sha256::digest(&help)),
        schema_sha256,
        capabilities,
    })
}

type Probed = (Transport, Vec<u8>, Option<String>);

fn set(capabilities: &mut BTreeMap<String, Option<bool>>, key: &str, value: bool) {
    capabilities.insert(key.to_owned(), Some(value));
}

/// Codex: its transports are the subcommands `codex --help` lists in its Commands column (M1R-03). App Server when it
/// lists `app-server` and the schema has everything a turn uses (the schema then decides steer, resume, deltas and
/// token usage), else `exec --json`; refused when neither can run a turn (M1T07R-15, M1R-04). A listed subcommand
/// whose help cannot be shown fails the probe.
fn probe_codex(
    launcher: &Path,
    env: &ProbeEnv,
    caps: &mut BTreeMap<String, Option<bool>>,
) -> Result<Probed, ProbeError> {
    let top = run_ok(launcher, &["--help"], env)?.stdout;
    let top_text = String::from_utf8_lossy(&top).into_owned();
    let listed = |name: &str| crate::adapter::lists_command(&top_text, name);
    let mut help = top.clone();
    let exec_text = match listed("exec") {
        true => {
            let exec_help = run_ok(launcher, &["exec", "--help"], env)?.stdout;
            help.extend_from_slice(&exec_help);
            Some(String::from_utf8_lossy(&exec_help).into_owned())
        }
        false => None,
    };
    let mut no_app_server = "codex --help lists no app-server".to_owned();
    if listed("app-server") {
        let app = run_ok(launcher, &["app-server", "--help"], env)?.stdout;
        // It must start on stdio before its schema is worth reading (M1T07R-27).
        let usable = match crate::codex::app_server_launch_missing(&String::from_utf8_lossy(&app)) {
            Some(why) => Err(why),
            None => {
                let schema = codex_schema(launcher, env)?;
                match crate::codex::app_server_interface_missing(
                    &schema.requests,
                    &schema.client_notifications,
                    &schema.notifications,
                ) {
                    None => Ok(schema),
                    Some(why) => Err(why),
                }
            }
        };
        match usable {
            Ok(schema) => {
                help.extend_from_slice(&app);
                set(caps, "structured_events", true);
                for (method, capability) in crate::codex::APP_SERVER_GATED {
                    set(caps, capability, schema.requests.contains(*method));
                }
                set(
                    caps,
                    "partial_messages",
                    schema.notifications.contains("item/agentMessage/delta"),
                );
                set(
                    caps,
                    "token_usage",
                    schema.notifications.contains("thread/tokenUsage/updated"),
                );
                return Ok((Transport::AppServer, help, Some(schema.hash)));
            }
            Err(why) => no_app_server = why,
        }
    }
    let why = match &exec_text {
        None => Some("codex --help lists no exec".to_owned()),
        Some(text) => crate::codex::exec_interface_missing(text),
    };
    if let Some(why) = why {
        return Err(ProbeError::Unsupported {
            agent: Agent::Codex,
            detail: format!("{no_app_server}, and {why}"),
        });
    }
    let exec_text = exec_text.unwrap_or_default();
    set(caps, "structured_events", true);
    set(
        caps,
        "resume",
        crate::adapter::lists_command(&exec_text, "resume"),
    );
    set(caps, "mid_turn_steer", false);
    set(caps, "partial_messages", false);
    Ok((Transport::Exec, help, None))
}

/// What `codex app-server generate-json-schema` produced: its tree hash and the request and notification methods
/// of both directions, read only from validated schema files ([`codex_schema`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSchema {
    pub hash: String,
    pub requests: std::collections::BTreeSet<String>,
    pub client_notifications: std::collections::BTreeSet<String>,
    pub notifications: std::collections::BTreeSet<String>,
}

#[cfg(test)]
thread_local! {
    /// How many diagnostic paths this thread has written out (a well-formed schema writes none; M1T07R-40).
    static RENDERED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many schema files this thread has parsed into values (only the three protocol files are; M1T07R-40).
    static PARSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Called between the walk or the removal learning of an entry and acting on it, so a test can change the entry
    /// there as a helper the CLI left running could (M1T07R-44).
    static BETWEEN: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

/// What a test runs at [`between`].
#[cfg(test)]
type Hook = Box<dyn FnMut(&str, &Path)>;

/// Set by the test that sees whether the probe waits for its reading to end after the result came.
#[cfg(test)]
static HOLD_AFTER_RESULT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The reading's last moment, after it sent its result: held 300 ms when [`HOLD_AFTER_RESULT`] is set.
#[cfg(test)]
fn hold_after_result() {
    if HOLD_AFTER_RESULT.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Released by the test that stalls the reading, all stalls at once (see [`between`]).
#[cfg(test)]
static RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn between(moment: &str, path: &Path) {
    // An entry named `hoh-test-stall-<moment>`, at `path` or in it, stalls the reading there, on whatever thread, as
    // a call that does not return would, until the test releases it (M1T07R-47).
    // Removing the entry releases it too, so a test can release its own stall alone.
    let marker = format!("hoh-test-stall-{moment}");
    let at = if path.file_name().is_some_and(|name| name == marker.as_str()) {
        path.to_path_buf()
    } else {
        path.join(&marker)
    };
    // Held open, it releases the stall when it is removed wherever it was moved meanwhile.
    if let Ok(held) = std::fs::File::open(&at) {
        let started = Instant::now();
        while !RELEASE.load(std::sync::atomic::Ordering::SeqCst)
            && held
                .metadata()
                .is_ok_and(|m| std::os::unix::fs::MetadataExt::nlink(&m) > 0)
            && started.elapsed() < Duration::from_secs(120)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    BETWEEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(moment, path);
        }
    });
}

#[cfg(not(test))]
fn between(_: &str, _: &Path) {}

/// The longest definition name (and so `$ref` target) the probe reads: resolving a reference then costs a bounded
/// amount, however often it is used (M1T07R-40). The recorded 0.166.0 schema's longest is 54 bytes.
const MAX_NAME: usize = 256;

/// The largest App Server schema output the probe reads: a file, all files, the number of entries (files and
/// directories; the recorded 0.166.0 output is 321 files, 3.7 MB in all, the largest 750 KB); beyond them it is
/// refused before parsing (M1T07R-40).
const MAX_SCHEMA_FILE: u64 = 16 * 1024 * 1024;
const MAX_SCHEMA_TOTAL: u64 = 64 * 1024 * 1024;
const MAX_SCHEMA_FILES: usize = 4096;

/// How deep the probe walks or removes a schema output, each level holding one open directory: deeper output is
/// refused, and left when removed (the recorded 0.166.0 output is 2 deep; M1T07R-44).
const MAX_SCHEMA_DEPTH: usize = 64;

/// The most output the probe keeps from a command, stdout and stderr each: more fails the command, read no further
/// (the recorded helps are at most 22 KB; M1T07R-44).
const MAX_OUTPUT: u64 = 16 * 1024 * 1024;

/// The longest number token the probe reads; longer ones are refused, so reading one is bounded (M1T07R-42). The
/// recorded 0.166.0 schema's longest is 3 characters.
const MAX_NUMBER: usize = 64;

/// The draft-07 `$schema` the App Server schema files declare.
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

/// Draft-07 well-formedness for the keyword set the App Server schema uses (recorded 0.166.0: `title`,
/// `description`, `type`, `enum`, `properties`, `required`, `$ref`, `oneOf`, `anyOf`, `allOf`, `items`,
/// `additionalProperties`, `format`, `minimum`, `minLength`, `default`, and `$schema`/`definitions` at the root).
/// Every schema position is checked against the draft-07 meta-schema rule for its keyword; any other keyword, an
/// ill-typed value, or a `$ref` other than `#/definitions/<name>` naming a root definition refuses the document, so a
/// newer schema form fails the probe instead of being half-read (M1T01R-25, -26, -27). `minLength` must be written as
/// an integer literal (M1T01R-28).
fn draft7_well_formed(root: &serde_json::Value, work: &Work) -> Result<(), String> {
    use serde_json::{Map, Value};
    /// One step of the path to a schema position; the path is written out only for an error, so a long key is never
    /// copied once per descendant (M1T07R-40).
    #[derive(Clone, Copy)]
    enum Step<'a> {
        Key(&'a str),
        Index(usize),
    }
    fn render(path: &[Step]) -> String {
        #[cfg(test)]
        RENDERED.with(|n| n.set(n.get() + 1));
        path.iter()
            .map(|step| match step {
                Step::Key(key) => format!("/{key}"),
                Step::Index(i) => format!("/{i}"),
            })
            .collect()
    }
    fn type_name(v: &Value) -> bool {
        matches!(
            v.as_str(),
            Some("null" | "boolean" | "object" | "array" | "number" | "integer" | "string")
        )
    }
    fn unique_strings(v: &Value) -> bool {
        v.as_array().is_some_and(|a| {
            a.iter().all(Value::is_string)
                && a.iter()
                    .filter_map(Value::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == a.len()
        })
    }
    fn schema<'a>(
        v: &'a Value,
        defs: &Map<String, Value>,
        path: &mut Vec<Step<'a>>,
        work: &Work,
    ) -> Result<(), String> {
        if !work.spend(1) {
            return Err(format!("{}: not read within the time limit", render(path)));
        }
        let obj = match v {
            Value::Bool(_) => return Ok(()),
            Value::Object(obj) => obj,
            _ => {
                return Err(format!(
                    "{}: a schema must be an object or a boolean",
                    render(path)
                ));
            }
        };
        let root = path.is_empty();
        for (key, v) in obj {
            path.push(Step::Key(key));
            // An array or string a keyword walks costs its length (M1T07R-40).
            let length = match (key.as_str(), v) {
                ("required" | "type", Value::Array(items)) => items.len(),
                ("$ref", Value::String(target)) => target.len(),
                _ => 0,
            };
            if !work.spend(length) {
                return Err(format!("{}: not read within the time limit", render(path)));
            }
            let ok = match key.as_str() {
                "$schema" => root && v == DRAFT_07,
                "definitions" => root,
                "title" | "description" | "format" => v.is_string(),
                "default" => true,
                "type" => {
                    type_name(v)
                        || (unique_strings(v)
                            && v.as_array()
                                .is_some_and(|a| !a.is_empty() && a.iter().all(type_name)))
                }
                "enum" => v.is_array(),
                "required" => unique_strings(v),
                "minimum" => v.is_number(),
                // A JSON integer literal only: serde_json rounds other spellings (`0.9999999999999999` reads as
                // 1.0), so integral-looking floats are refused rather than judged after rounding (M1T01R-28).
                "minLength" => v.is_u64(),
                "$ref" => v
                    .as_str()
                    .and_then(|r| r.strip_prefix("#/definitions/"))
                    .is_some_and(|name| {
                        !name.is_empty()
                            && name
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
                            && defs.contains_key(name)
                    }),
                "properties" => match v.as_object() {
                    Some(props) => {
                        for (name, s) in props {
                            path.push(Step::Key(name));
                            schema(s, defs, path, work)?;
                            path.pop();
                        }
                        true
                    }
                    None => false,
                },
                "items" | "additionalProperties" => {
                    schema(v, defs, path, work)?;
                    true
                }
                "oneOf" | "anyOf" | "allOf" => match v.as_array().filter(|a| !a.is_empty()) {
                    Some(list) => {
                        for (i, s) in list.iter().enumerate() {
                            path.push(Step::Index(i));
                            schema(s, defs, path, work)?;
                            path.pop();
                        }
                        true
                    }
                    None => false,
                },
                _ => return Err(format!("{}: unsupported keyword", render(path))),
            };
            if !ok {
                return Err(format!("{}: ill-formed value", render(path)));
            }
            path.pop();
        }
        Ok(())
    }
    let empty = Map::new();
    let defs = match root.get("definitions") {
        None => &empty,
        Some(Value::Object(defs)) => defs,
        Some(_) => return Err("/definitions: not an object".to_owned()),
    };
    for (name, s) in defs {
        if name.len() > MAX_NAME {
            let start: String = name.chars().take(32).collect();
            return Err(format!(
                "/definitions/{start}...: a definition name longer than {MAX_NAME} bytes"
            ));
        }
        let mut path = vec![Step::Key("definitions"), Step::Key(name)];
        schema(s, defs, &mut path, work)?;
    }
    schema(root, defs, &mut Vec::new(), work)
}

/// The `method` names a well-formed protocol schema file (see [`draft7_well_formed`]) lists, read only from the form
/// the App Server schema uses: a root `oneOf` (with `$schema`, `definitions`, `properties`, `title`, `description`
/// beside it) whose every branch is `{type: "object", required, properties: {method: {type: "string", enum:
/// [names]}, ...}}` with optional `title`/`description`. Any other shape rejects the whole file, so malformed output
/// never yields a capability (M1T01R-15, -18, -23, -27).
///
/// What the probe does not interpret cannot narrow the methods (M1T07R-32); `Err` when the file was not read within
/// `work` (M1T07R-40): in a file of what HoH sends (`sent`:
/// ClientRequest.json, ClientNotification.json) the root has no `properties`, which would constrain HoH's messages; in
/// ServerNotification.json the root `properties` may describe other fields (0.166.0: `emittedAtMs`) but not `method`;
/// and no method is listed by two branches, which `oneOf` would refuse.
fn schema_methods(
    value: &serde_json::Value,
    sent: bool,
    work: &Work,
) -> Result<Option<std::collections::BTreeSet<String>>, ()> {
    use serde_json::Value;
    let Some(root) = value.as_object() else {
        return Ok(None);
    };
    let root_ok = root.iter().all(|(k, v)| match k.as_str() {
        "$schema" | "definitions" | "title" | "description" | "oneOf" => true,
        "properties" => !sent && v.get("method").is_none(),
        _ => false,
    });
    let Some(variants) = root
        .get("oneOf")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    let mut methods = std::collections::BTreeSet::new();
    for branch in variants {
        let Some(branch) = branch.as_object() else {
            return Ok(None);
        };
        let branch_ok = branch.iter().all(|(key, v)| match key.as_str() {
            "type" => v == "object",
            "required" | "properties" | "title" | "description" => true,
            _ => false,
        });
        let Some(method) = branch
            .get("properties")
            .and_then(|p| p.get("method"))
            .and_then(Value::as_object)
        else {
            return Ok(None);
        };
        let method_ok = method.get("type").is_some_and(|t| t == "string")
            && method
                .keys()
                .all(|k| matches!(k.as_str(), "type" | "enum" | "title" | "description"));
        let Some(names) = method
            .get("enum")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
        else {
            return Ok(None);
        };
        // Each branch and each of its names cost a step (M1T07R-40).
        if !work.spend(names.len() + 1) {
            return Err(());
        }
        if !root_ok || !branch_ok || !method_ok || !names.iter().all(Value::is_string) {
            return Ok(None);
        }
        for name in names.iter().filter_map(Value::as_str) {
            if !methods.insert(name.to_owned()) {
                return Ok(None);
            }
        }
    }
    Ok(Some(methods))
}

/// The `oneOf` branch of a protocol schema file listing each method, in one pass (the known form lists a method once),
/// a step per method.
fn branches_by_method<'a>(
    file: &'a serde_json::Value,
    work: &Work,
) -> std::collections::HashMap<&'a str, &'a serde_json::Value> {
    let mut branches = std::collections::HashMap::new();
    for branch in file["oneOf"].as_array().into_iter().flatten() {
        for name in branch["properties"]["method"]["enum"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            // Past the deadline a method has no branch, so a request is not kept (M1T07R-40).
            if !work.spend(1) {
                return branches;
            }
            branches.insert(name, branch);
        }
    }
    branches
}

/// A protocol schema file made to accept any `params` in each branch, in place: the probe checks a method's envelope,
/// not its parameters, which the compat suite verifies against recorded traffic (ADR-015 decision 2, M1T07R-33).
fn params_open(open: &mut serde_json::Value) {
    // The root's `properties` (ServerNotification.json) and every branch's (M1T07R-37).
    if let Some(params) = open.get_mut("properties").and_then(|p| p.get_mut("params")) {
        *params = serde_json::Value::Bool(true);
    }
    for branch in open["oneOf"].as_array_mut().into_iter().flatten() {
        if let Some(params) = branch["properties"].get_mut("params") {
            *params = serde_json::Value::Bool(true);
        }
    }
}

/// The most values (keys included) a protocol schema file may parse into; the recorded 0.166.0 files have at most
/// 10,609 by [`json_values`]. A parsed value costs many times its text (an object with one key is hundreds of bytes
/// for six bytes of text), so a file within the size limits could take gigabytes; past this it is refused before it
/// is parsed, and the three files take a few hundred megabytes at most (M1T07R-50).
const MAX_SCHEMA_VALUES: usize = 200_000;

/// An upper bound of the values a JSON text parses into, keys included: one, and one for each `{`, `[`, `,` and `:`
/// outside strings.
fn json_values(text: &[u8]) -> usize {
    let (mut values, mut in_string, mut escaped) = (1usize, false, false);
    for &byte in text {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if matches!(byte, b'{' | b'[' | b',' | b':') {
            values += 1;
        }
    }
    values
}

/// How deep schemas may nest while validating; deeper is undecided, so a cyclic definition cannot loop.
const REF_DEPTH: usize = 64;

/// How many steps one reading of the App Server schema may take in all: every schema node visited and every element it
/// examines (members of `allOf`/`anyOf`/`oneOf`, values of `enum`, names of `required`, properties, an instance's
/// keys) is a step. Depth alone does not bound the work of a definition naming itself twice, or of wide members and
/// long enums reused many times; past the budget the reading fails (unknown) within a second, never reporting the
/// method absent (M1T07R-40). The recorded 0.166.0 schema needs about 150,000.
const VALIDATION_BUDGET: u64 = 4_000_000;

/// When reading the App Server schema must end: at doctor's shared deadline, and never later than the probe's own
/// command limit from `now`, so a probe without a shared deadline is bounded too (M1T07R-40).
fn reading_deadline(env: &ProbeEnv, now: Instant) -> Instant {
    let own = now + env.timeout;
    env.deadline.map_or(own, |shared| shared.min(own))
}

/// What one reading of the App Server schema may still spend: steps ([`VALIDATION_BUDGET`]) and, if set, doctor's
/// shared deadline, read every 4096 steps; once either is used up validation is undecided (M1T07R-40).
struct Work {
    steps: std::cell::Cell<u64>,
    until: Option<Instant>,
    /// The budget or the deadline stopped a step.
    out: std::cell::Cell<bool>,
    /// Set when whoever waits for this work has given up on it: every later step is refused (M1T07R-47).
    stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Work {
    fn new(steps: u64, until: Option<Instant>) -> Work {
        Work {
            steps: std::cell::Cell::new(steps),
            until,
            out: std::cell::Cell::new(false),
            stop: None,
        }
    }

    /// This work, refusing every step once `flag` is set.
    fn stopped_by(mut self, flag: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> Work {
        self.stop = Some(flag.clone());
        self
    }

    /// Whether the budget or the deadline stopped a step.
    fn ran_out(&self) -> bool {
        self.out.get()
    }

    /// Take `n` steps; false once the budget or the deadline is used up.
    fn spend(&self, n: usize) -> bool {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        let left = self.steps.get();
        let given_up = self
            .stop
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst));
        if given_up || left < n {
            self.steps.set(0);
            self.out.set(true);
            return false;
        }
        let after = left - n;
        self.steps.set(after);
        if left / 4096 != after / 4096 && self.until.is_some_and(|until| Instant::now() >= until) {
            self.steps.set(0);
            self.out.set(true);
            return false;
        }
        true
    }

    #[cfg(test)]
    fn left(&self) -> u64 {
        self.steps.get()
    }
}

/// The value of a JSON number token as (negative, significant digits, power of ten), without leading or trailing zero
/// digits; every zero is (false, "", 0). `None` for what is no number token.
fn decimal(token: &str) -> Option<(bool, String, i64)> {
    let (negative, rest) = match token.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(at) => (&rest[..at], rest[at + 1..].parse::<i64>().ok()?),
        None => (rest, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    let digits = format!("{whole}{fraction}");
    let mut digits = digits.trim_start_matches('0').to_owned();
    while digits.ends_with('0') {
        digits.pop();
        exponent = exponent.checked_add(1)?;
    }
    Some(match digits.is_empty() {
        true => (false, String::new(), 0),
        false => (negative, digits, exponent),
    })
}

/// Whether a JSON number token reads back as written: an integer within 64 bits, or a number whose parsed value prints
/// (shortest, round-trip) as the same decimal, and from 2^53 on is that very integer. Such a number compares with an integer exactly as its decimal does, so
/// `valid` stays exact on the probe's envelopes; `1e-400` (read as 0), `0.99999999999999999` (read as 1),
/// `18446744073709551616` (beyond 64 bits) do not (M1T07R-39).
fn reads_exactly(token: &str) -> bool {
    if token.len() > MAX_NUMBER {
        return false;
    }
    if !token.contains(['.', 'e', 'E']) {
        return token.parse::<i64>().is_ok() || token.parse::<u64>().is_ok();
    }
    match token.parse::<f64>() {
        // From 2^53 on every float is an integer, and its shortest print may differ from it (1000000000000000100.0
        // is stored as 1000000000000000128): the token must be that integer itself (M1T07R-39).
        Ok(f) if f.is_finite() && f.abs() >= 9_007_199_254_740_992.0 => {
            decimal(token).is_some() && decimal(token) == decimal(&format!("{f:.0}"))
        }
        Ok(f) if f.is_finite() => {
            decimal(token).is_some() && decimal(token) == decimal(&format!("{f:e}"))
        }
        _ => false,
    }
}

/// The first number token of a JSON text (outside its strings) that does not read back as written ([`reads_exactly`]);
/// `Err` when the text could not be scanned within `work` (doctor's deadline; M1T07R-40).
fn inexact_number(text: &str, work: &Work) -> Result<Option<String>, ()> {
    let bytes = text.as_bytes();
    let (mut at, mut in_string) = (0, false);
    while at < bytes.len() {
        if !work.spend(1) {
            return Err(());
        }
        let b = bytes[at];
        if in_string {
            match b {
                b'\\' => at += 1,
                b'"' => in_string = false,
                _ => {}
            }
            at += 1;
        } else if b == b'"' {
            in_string = true;
            at += 1;
        } else if b == b'-' || b.is_ascii_digit() {
            let end = bytes[at..]
                .iter()
                .position(|c| !(c.is_ascii_digit() || b"+-.eE".contains(c)))
                .map_or(bytes.len(), |n| at + n);
            let token = &text[at..end];
            if !work.spend(token.len()) {
                return Err(());
            }
            if !reads_exactly(token) {
                return Ok(Some(token.to_owned()));
            }
            at = end;
        } else {
            at += 1;
        }
    }
    Ok(None)
}

/// A validation result: what the probe cannot decide exactly is `Unknown`, and only `Yes` keeps a method, so an
/// undecided part never turns into a decision the other way (inside `oneOf`, a wrong "no" would be a wrong "yes";
/// M1T07R-34).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Truth {
    Yes,
    No,
    Unknown,
}

impl Truth {
    fn of(yes: bool) -> Truth {
        if yes { Truth::Yes } else { Truth::No }
    }

    /// Every item holds: no if one is no, else undecided if one is.
    fn all(items: impl IntoIterator<Item = Truth>) -> Truth {
        let mut all = Truth::Yes;
        for item in items {
            match item {
                Truth::No => return Truth::No,
                Truth::Unknown => all = Truth::Unknown,
                Truth::Yes => {}
            }
        }
        all
    }

    /// Some item holds: yes if one is yes, else undecided if one is.
    fn any(items: impl IntoIterator<Item = Truth>) -> Truth {
        let mut any = Truth::No;
        for item in items {
            match item {
                Truth::Yes => return Truth::Yes,
                Truth::Unknown => any = Truth::Unknown,
                Truth::No => {}
            }
        }
        any
    }

    /// Exactly one item holds.
    fn one(items: impl IntoIterator<Item = Truth>) -> Truth {
        let (mut yes, mut unknown) = (0, 0);
        for item in items {
            match item {
                Truth::Yes => yes += 1,
                Truth::Unknown => unknown += 1,
                Truth::No => {}
            }
        }
        match (yes, unknown) {
            (2.., _) => Truth::No,
            (1, 0) => Truth::Yes,
            (0, 0) => Truth::No,
            _ => Truth::Unknown,
        }
    }
}

/// Two JSON numbers compared exactly (`0` equals `0.0`, `9007199254740993` is more than `9007199254740992`); `None`
/// when they cannot be (a NaN).
fn number_order(a: &serde_json::Number, b: &serde_json::Number) -> Option<std::cmp::Ordering> {
    let integer = |n: &serde_json::Number| {
        n.as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
    };
    // An integer against a float: against the float's floor, the float being more when it has a fraction.
    let against = |i: i128, f: f64| -> Option<std::cmp::Ordering> {
        if f.is_nan() {
            return None;
        }
        let floor = f.floor();
        if floor >= 1.7e38 {
            return Some(std::cmp::Ordering::Less);
        }
        if floor <= -1.7e38 {
            return Some(std::cmp::Ordering::Greater);
        }
        let floor = floor as i128;
        Some(match i.cmp(&floor) {
            std::cmp::Ordering::Equal if f.fract() != 0.0 => std::cmp::Ordering::Less,
            order => order,
        })
    };
    match (integer(a), integer(b)) {
        (Some(x), Some(y)) => Some(x.cmp(&y)),
        (Some(x), None) => against(x, b.as_f64()?),
        (None, Some(y)) => against(y, a.as_f64()?).map(std::cmp::Ordering::reverse),
        (None, None) => a.as_f64()?.partial_cmp(&b.as_f64()?),
    }
}

/// JSON equality as JSON Schema means it: numbers by value, arrays and objects member by member (M1T07R-35).
fn json_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            number_order(x, y) == Some(std::cmp::Ordering::Equal)
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_equal(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_equal(v, w)))
        }
        _ => a == b,
    }
}

/// Whether `instance` is valid under `schema` by draft-07, for the keyword set [`draft7_well_formed`] accepts (the
/// only schemas that reach here): `$ref` alone counts where it is (its siblings are ignored, as draft-07 says);
/// `format`, `default`, `title`, `description` are annotations; nesting deeper than [`REF_DEPTH`] is `Unknown`
/// (M1T07R-33, -34).
fn valid(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
    work: &Work,
) -> Truth {
    use serde_json::Value;
    if !work.spend(1) {
        return Truth::Unknown;
    }
    let obj = match schema {
        Value::Bool(b) => return Truth::of(*b),
        Value::Object(_) if depth > REF_DEPTH => return Truth::Unknown,
        Value::Object(obj) => obj,
        _ => return Truth::No,
    };
    let sub = |s: &Value, x: &Value| valid(s, x, defs, depth + 1, work);
    if let Some(target) = obj.get("$ref") {
        return match target
            .as_str()
            .and_then(|r| r.strip_prefix("#/definitions/"))
            .and_then(|name| defs.get(name))
        {
            Some(s) => sub(s, instance),
            None => Truth::No,
        };
    }
    let is_type = |name: &str| match name {
        "null" => instance.is_null(),
        "boolean" => instance.is_boolean(),
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "number" => instance.is_number(),
        "integer" => {
            instance.is_i64()
                || instance.is_u64()
                || instance.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        "string" => instance.is_string(),
        _ => false,
    };
    // A keyword that examines `n` elements spends `n` steps first; members are visited in place (M1T07R-40).
    fn members(v: &serde_json::Value) -> &[serde_json::Value] {
        v.as_array().map(Vec::as_slice).unwrap_or_default()
    }
    Truth::all(obj.iter().map(|(key, v)| {
        match key.as_str() {
            "type" => Truth::of(match v {
                Value::String(name) => is_type(name),
                Value::Array(names) => names.iter().filter_map(Value::as_str).any(is_type),
                _ => false,
            }),
            "enum" => match v.as_array() {
                None => Truth::No,
                Some(values) if !work.spend(values.len()) => Truth::Unknown,
                Some(values) => Truth::of(values.iter().any(|e| json_equal(e, instance))),
            },
            "required" => match (instance.as_object(), v.as_array()) {
                (None, _) => Truth::Yes,
                (Some(_), None) => Truth::No,
                (Some(_), Some(names)) if !work.spend(names.len()) => Truth::Unknown,
                (Some(o), Some(names)) => Truth::of(
                    names
                        .iter()
                        .all(|n| n.as_str().is_some_and(|n| o.contains_key(n))),
                ),
            },
            "properties" => match (instance.as_object(), v.as_object()) {
                (None, _) => Truth::Yes,
                (Some(_), None) => Truth::No,
                (Some(_), Some(props)) if !work.spend(props.len()) => Truth::Unknown,
                (Some(o), Some(props)) => Truth::all(
                    props
                        .iter()
                        .filter_map(|(n, s)| o.get(n).map(|x| sub(s, x))),
                ),
            },
            "additionalProperties" => match instance.as_object() {
                None => Truth::Yes,
                Some(o) if !work.spend(o.len()) => Truth::Unknown,
                Some(o) => {
                    let declared = obj.get("properties").and_then(Value::as_object);
                    Truth::all(
                        o.iter()
                            .filter(|(n, _)| !declared.is_some_and(|d| d.contains_key(*n)))
                            .map(|(_, x)| sub(v, x)),
                    )
                }
            },
            "items" => match instance.as_array() {
                None => Truth::Yes,
                Some(a) => Truth::all(a.iter().map(|x| sub(v, x))),
            },
            "allOf" => Truth::all(members(v).iter().map(|s| sub(s, instance))),
            "anyOf" => Truth::any(members(v).iter().map(|s| sub(s, instance))),
            "oneOf" => Truth::one(members(v).iter().map(|s| sub(s, instance))),
            "minimum" => match (instance.as_number(), v.as_number()) {
                (None, _) => Truth::Yes,
                (Some(x), Some(m)) => match number_order(x, m) {
                    Some(order) => Truth::of(order != std::cmp::Ordering::Less),
                    None => Truth::Unknown,
                },
                (Some(_), None) => Truth::No,
            },
            "minLength" => {
                Truth::of(instance.as_str().is_none_or(|text| {
                    v.as_u64().is_some_and(|m| text.chars().count() as u64 >= m)
                }))
            }
            "format" | "default" | "title" | "description" | "$schema" | "definitions" => {
                Truth::Yes
            }
            _ => Truth::No,
        }
    }))
}

/// Whether every request id the adapter may use (it numbers them 0, 1, 2, ...) is valid under `schema`. A sound check
/// that refuses what it cannot show: `true`; a `$ref` to such a schema (its siblings ignored, as draft-07 says); or
/// an object with no `enum` or `oneOf`, a `type` that admits integers, a `minimum` of at most 0, every `allOf` member
/// and some `anyOf` member such a schema, and otherwise only keywords that do not apply to a number (M1T07R-36).
fn takes_every_id(
    schema: &serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
    work: &Work,
) -> bool {
    use serde_json::Value;
    if !work.spend(1) {
        return false;
    }
    let obj = match schema {
        Value::Bool(b) => return *b,
        Value::Object(obj) if depth <= REF_DEPTH => obj,
        _ => return false,
    };
    let sub = |s: &Value| takes_every_id(s, defs, depth + 1, work);
    if let Some(target) = obj.get("$ref") {
        return target
            .as_str()
            .and_then(|r| r.strip_prefix("#/definitions/"))
            .and_then(|name| defs.get(name))
            .is_some_and(sub);
    }
    let integers = |name: &str| matches!(name, "integer" | "number");
    obj.iter().all(|(key, v)| match key.as_str() {
        "type" => match v {
            Value::String(name) => integers(name),
            Value::Array(names) => names.iter().filter_map(Value::as_str).any(integers),
            _ => false,
        },
        "minimum" => v.as_number().is_some_and(|m| {
            number_order(m, &serde_json::Number::from(0))
                .is_some_and(|o| o != std::cmp::Ordering::Greater)
        }),
        "allOf" => v.as_array().is_some_and(|l| l.iter().all(sub)),
        "anyOf" => v.as_array().is_some_and(|l| l.iter().any(sub)),
        "required"
        | "properties"
        | "additionalProperties"
        | "items"
        | "minLength"
        | "format"
        | "default"
        | "title"
        | "description" => true,
        _ => false,
    })
}

/// `codex app-server generate-json-schema --out <dir>`: every `.json` file must parse, and `ClientRequest.json`,
/// `ClientNotification.json` and `ServerNotification.json` must be protocol schemas whose method lists the interface
/// and the capabilities are read from; anything else is a failure, not an empty or guessed schema (M1T01R-08, -15,
/// -26, -27, M1R-04). doctor --live reads the schema through it too, so it never drives an App Server whose schema the
/// probe refused (M1T07R-31).
pub fn codex_schema(launcher: &Path, env: &ProbeEnv) -> Result<CodexSchema, ProbeError> {
    // Making the scratch directory, generating, reading and removing the schema share one window: doctor's shared
    // deadline, or without one the probe's own command limit (M1T07R-40).
    let until = reading_deadline(env, Instant::now());
    let generate = argv(launcher, &["app-server", "generate-json-schema"]);
    // All of it runs apart; the caller does no file-system work and starts no process for it, and waits only until
    // the window and the cleanup grace end, so no step, counted or not, blocking or not, holds the probe longer
    // (M1T07R-40, -54). Given up on, the work does nothing more to the output past the step in progress: every step
    // checks the flag (M1T07R-47).
    let given_up = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = Held::default();
    let (tx, rx) = mpsc::channel();
    let work = {
        let (launcher, env, generate) = (launcher.to_path_buf(), env.clone(), generate.clone());
        let (given_up, held) = (given_up.clone(), held.clone());
        move || {
            let _ = tx.send(schema_in_scratch(
                &launcher, &env, &generate, until, &given_up, &held,
            ));
            #[cfg(test)]
            hold_after_result();
        }
    };
    let worker = match std::thread::Builder::new()
        .name("hoh-schema".to_owned())
        .spawn(work)
    {
        Ok(worker) => worker,
        Err(e) => {
            return Err(ProbeError::Failed {
                argv: generate,
                detail: format!("cannot read the schema output: {e}"),
            });
        }
    };
    // Waited for in slices, so a stop of whoever probes is seen within one (M2T07R-202).
    let given_up_at = until + CLEANUP_GRACE + READER_MARGIN;
    let received = loop {
        let left = given_up_at.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.min(Duration::from_millis(50))) {
            Ok(result) => break Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) if !left.is_zero() && !cancelled(env) => {}
            Err(e) => {
                // Stopped, the reading still gets its cleanup grace to remove its output, as one at its time limit
                // does, and is given up only after it; its command the stop killed already (docs/adr/ADR-022.md
                // decision 10 v4, M2T07R-300).
                if matches!(e, mpsc::RecvTimeoutError::Timeout) && cancelled(env) {
                    let grace = (Instant::now() + CLEANUP_GRACE + READER_MARGIN).min(given_up_at);
                    if let Ok(result) =
                        rx.recv_timeout(grace.saturating_duration_since(Instant::now()))
                    {
                        break Ok(result);
                    }
                }
                given_up.store(true, std::sync::atomic::Ordering::SeqCst);
                // A result sent as the wait ended is still taken.
                break rx.try_recv().map_err(|_| e);
            }
        }
    };
    match received {
        Ok(result) => {
            // Its result sent, the work ends at once: joined, it holds nothing past the probe.
            let _ = worker.join();
            result
        }
        Err(e) => Err(ProbeError::Failed {
            argv: generate,
            detail: format!(
                "{}; {}",
                match e {
                    mpsc::RecvTimeoutError::Timeout =>
                        "the schema output was not read and removed within the time limit",
                    mpsc::RecvTimeoutError::Disconnected =>
                        "reading the schema output ended without a result",
                },
                held.may_be_left()
            ),
        }),
    }
}

/// Why a probe command was ended or not started: whoever probes stops.
const STOPPING: &str = "cancelled: whoever probes is stopping";

/// Whether whoever probes with `env` stops.
fn cancelled(env: &ProbeEnv) -> bool {
    env.cancel.as_ref().is_some_and(ProbeStop::is_stopped)
}

/// How long past the cleanup grace [`codex_schema`] waits for the reading to report.
const READER_MARGIN: Duration = Duration::from_millis(500);

/// The directories a schema reading holds (its scratch directory, then its quarantine), shared with the caller as
/// they are made, so a caller that gives up names where they are when it does (M1T07R-52); `None` for one made whose
/// copy could not be kept, so it is never taken for gone (M1T07R-82).
#[derive(Clone, Default)]
struct Held(std::sync::Arc<std::sync::Mutex<Vec<Option<std::os::fd::OwnedFd>>>>);

impl Held {
    fn add(&self, fd: std::os::fd::BorrowedFd<'_>) {
        let copy = fd.try_clone_to_owned().ok();
        #[cfg(test)]
        let copy = copy.filter(|_| !COPY_FAILS.with(std::cell::Cell::get));
        match self.0.lock() {
            Ok(mut held) => held.push(copy),
            Err(poisoned) => poisoned.into_inner().push(copy),
        }
    }

    /// Where the output may be left, as far as the directories held tell now.
    fn may_be_left(&self) -> String {
        let held = match self.0.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        if held.is_empty() {
            return format!(
                "its scratch directory under {} may be left",
                std::env::temp_dir().display()
            );
        }
        let mut places = Vec::new();
        let mut unread = false;
        for fd in held.iter() {
            match fd
                .as_ref()
                .map_or(Place::Unread, |fd| where_now(fd.as_fd()))
            {
                Place::Gone => {}
                Place::At(path) => places.push(path.display().to_string()),
                Place::Unread => unread = true,
            }
        }
        const UNREAD: &str = "a directory whose path cannot be read";
        match places.split_first() {
            None if unread => format!("it may be left in {UNREAD}"),
            None => "nothing of it is left".to_owned(),
            Some((first, rest)) => {
                let mut rest = rest.to_vec();
                if unread {
                    rest.push(UNREAD.to_owned());
                }
                let mut said = format!("it may be left at {first}");
                if !rest.is_empty() {
                    said.push_str(&format!(" (and {})", rest.join(", ")));
                }
                said
            }
        }
    }
}

/// Where a directory held is now.
enum Place {
    /// Removed: no link left.
    Gone,
    At(PathBuf),
    /// Still there, or not known to be gone, but its path cannot be read (longer than `PATH_MAX`, no `/proc`).
    Unread,
}

/// Where the directory `fd` is now (`/proc/self/fd`): gone only when its link count is 0, never because the path
/// cannot be read (M1T07R-81), nor by the ` (deleted)` the kernel appends, which a directory's own name may end with
/// (M1T07R-58).
fn where_now(fd: std::os::fd::BorrowedFd<'_>) -> Place {
    if rustix::fs::fstat(fd).is_ok_and(|s| s.st_nlink == 0) {
        return Place::Gone;
    }
    let link = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&fd));
    std::fs::read_link(link).map_or(Place::Unread, Place::At)
}

/// Make the scratch directory, generate the schema into it, read it and remove it: [`codex_schema`]'s work, apart.
fn schema_in_scratch(
    launcher: &Path,
    env: &ProbeEnv,
    generate: &[String],
    until: Instant,
    given_up: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    held: &Held,
) -> Result<CodexSchema, ProbeError> {
    let stopped = || ProbeError::Failed {
        argv: generate.to_vec(),
        detail: "given up before the schema was generated".to_owned(),
    };
    between("scratch", launcher.parent().unwrap_or(launcher));
    if given_up.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(stopped());
    }
    let scratch = Scratch::new(until + CLEANUP_GRACE, given_up.clone(), held.clone())?;
    // Nothing is started for a caller that has gone.
    if given_up.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(stopped());
    }
    let out = scratch.dir.to_string_lossy().into_owned();
    // Generation ends with the window, however long making the directory took.
    let env = ProbeEnv {
        deadline: Some(until),
        ..env.clone()
    };
    let generated = run_ok(
        launcher,
        &["app-server", "generate-json-schema", "--out", &out],
        &env,
    )
    .map(|_| ());
    read_and_remove(scratch, generated, generate, Some(until))
}

/// Read the schema the CLI generated into `scratch` (if it did), then remove the output; what is not removed fails
/// the probe, saying where it is left, whatever the reading came to (M1T07R-40, -45).
fn read_and_remove(
    mut scratch: Scratch,
    generated: Result<(), ProbeError>,
    generate: &[String],
    until: Option<Instant>,
) -> Result<CodexSchema, ProbeError> {
    let unusable = |detail: String| ProbeError::Failed {
        argv: generate.to_vec(),
        detail,
    };
    let result = generated.and_then(|()| {
        // Output written anywhere but the directory the probe made is not read (M1T07R-44).
        if !scratch.holds() {
            return Err(unusable(format!(
                "the schema output directory {} was replaced",
                scratch.dir.display()
            )));
        }
        // The checks before validation are linear in the files; only the deadline bounds them, each file's bytes
        // counted before it is read (M1T07R-40, -42).
        let checks = Work::new(u64::MAX, until).stopped_by(&scratch.given_up);
        let files = schema_files(&scratch.dir, scratch.owned.as_fd(), &checks).map_err(unusable)?;
        read_schema(
            &scratch.dir,
            &files,
            &Work::new(VALIDATION_BUDGET, until).stopped_by(&scratch.given_up),
            &checks,
        )
        .map_err(unusable)
    });
    if scratch.remove() {
        return result;
    }
    let mut left = scratch.held.may_be_left();
    if let Some(why) = &scratch.unstarted {
        left = format!("{left}; {why}");
    }
    Err(match result {
        Err(ProbeError::Failed { argv, detail }) => ProbeError::Failed {
            argv,
            detail: format!("{detail}; the output could not be removed: {left}"),
        },
        Ok(_) => unusable(format!("the schema output could not be removed: {left}")),
        Err(other) => unusable(format!("{other}; the output could not be removed: {left}")),
    })
}

/// The three protocol schemas of a schema output (`dir`, its files read): the methods each lists in the known form
/// whose message can be valid, and the tree hash. Every other `.json` file must parse too, and is not kept. Work the
/// budget or the deadline cut short is a failure, never fewer methods (M1T07R-40).
fn read_schema(
    dir: &Path,
    files: &[(PathBuf, Vec<u8>)],
    work: &Work,
    checks: &Work,
) -> Result<CodexSchema, String> {
    const PROTOCOL: [&str; 3] = [
        "ClientRequest.json",
        "ClientNotification.json",
        "ServerNotification.json",
    ];
    between("parse", dir);
    let mut parsed = BTreeMap::new();
    for (file, bytes) in files {
        if !file.extension().is_some_and(|e| e == "json") {
            continue;
        }
        if !checks.spend(bytes.len()) {
            return Err(format!(
                "{} was not read within the time limit",
                file.display()
            ));
        }
        let not_json = |e: &dyn fmt::Display| format!("{} is not JSON: {e}", file.display());
        let text = std::str::from_utf8(bytes).map_err(|e| not_json(&e))?;
        let protocol = file
            .strip_prefix(dir)
            .ok()
            .and_then(|rel| rel.to_str())
            .filter(|rel| PROTOCOL.contains(rel));
        match protocol {
            Some(name) => {
                if json_values(bytes) > MAX_SCHEMA_VALUES {
                    return Err(format!(
                        "{} has more than {MAX_SCHEMA_VALUES} values",
                        file.display()
                    ));
                }
                let value =
                    serde_json::from_str::<serde_json::Value>(text).map_err(|e| not_json(&e))?;
                #[cfg(test)]
                PARSED.with(|n| n.set(n.get() + 1));
                parsed.insert(name, (value, text));
            }
            None => {
                serde_json::from_str::<serde::de::IgnoredAny>(text).map_err(|e| not_json(&e))?;
            }
        }
    }
    // The methods a file lists in the known form whose message can be valid: `envelope` is the message for a method,
    // checked against the file with the params left to the compat suite (M1T07R-33).
    // Each file's value is taken, opened in place and dropped once its methods are read, never copied.
    let mut methods = |name: &str, sent: bool, envelope: &dyn Fn(&str) -> serde_json::Value| {
        let (mut value, text) = parsed
            .remove(name)
            .ok_or_else(|| format!("{name} is missing"))?;
        draft7_well_formed(&value, checks)
            .map_err(|e| format!("{name} is not a well-formed schema: {e}"))?;
        match inexact_number(text, checks) {
            Ok(Some(number)) => {
                return Err(format!(
                    "{name} has a number the probe cannot read exactly: {number}"
                ));
            }
            Err(()) => return Err(format!("{name} was not read within the time limit")),
            Ok(None) => {}
        }
        let listed = schema_methods(&value, sent, checks)
            .map_err(|()| format!("{name} was not read within the time limit"))?
            .ok_or_else(|| format!("{name} does not list its methods in the known form"))?;
        params_open(&mut value);
        let open = value;
        let empty = serde_json::Map::new();
        let defs = open
            .get("definitions")
            .and_then(serde_json::Value::as_object)
            .unwrap_or(&empty);
        let branches = branches_by_method(&open, checks);
        Ok::<_, String>(
            listed
                .into_iter()
                .filter(|m| valid(&open, &envelope(m), defs, 0, work) == Truth::Yes)
                .filter(|m| {
                    // A request also needs an id schema that takes every id the adapter numbers (M1T07R-36).
                    !name.starts_with("ClientRequest")
                        || branches.get(m.as_str()).is_some_and(|branch| {
                            branch["properties"]
                                .get("id")
                                .is_none_or(|id| takes_every_id(id, defs, 0, work))
                        })
                })
                .collect(),
        )
    };
    // HoH's requests (any id will do here: whether the id schema takes every id is checked apart), its only
    // notification `initialized` without params, and the server's notifications with `method` and `params`.
    let request = |m: &str| serde_json::json!({"id": 0, "method": m, "params": {}});
    between("validate", dir);
    let requests = methods("ClientRequest.json", true, &request)?;
    let schema = CodexSchema {
        requests,
        client_notifications: methods(
            "ClientNotification.json",
            true,
            &|m: &str| serde_json::json!({"method": m}),
        )?,
        notifications: methods(
            "ServerNotification.json",
            false,
            &|m: &str| serde_json::json!({"method": m, "params": {}}),
        )?,
        hash: {
            between("hash", dir);
            tree_sha256_of(dir, files, checks)
                .map_err(|e| format!("hashing the schema output: {e}"))?
        },
    };
    // A method left out because the work ran out is unknown, not absent: the probe fails (M1T07R-40).
    if work.ran_out() || checks.ran_out() {
        return Err("the schema was not read within its time and work limits".to_owned());
    }
    Ok(schema)
}

/// How long removing a probe's scratch directory may run past the reading window.
const CLEANUP_GRACE: Duration = Duration::from_secs(2);

/// A device and inode: what an entry was when it was opened.
type Inode = (u64, u64);

fn inode(stat: &rustix::fs::Stat) -> Inode {
    (stat.st_dev, stat.st_ino)
}

/// A probe's scratch directory, removed within its window once ([`remove_within`]), at the latest when dropped.
struct Scratch {
    dir: PathBuf,
    /// The temporary directory the scratch directory was made in.
    temp: std::os::fd::OwnedFd,
    /// The directory the probe made at `dir`: everything under it is reached from here (M1T07R-44, -51).
    owned: std::os::fd::OwnedFd,
    /// Set when the probe stopped waiting for the reading: nothing more is done to the output (M1T07R-47).
    given_up: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The directories held, shared with the caller (M1T07R-52).
    held: Held,
    until: Instant,
    removed: Option<bool>,
    /// Why removing did not start, when the quarantine could not be made (M1T07R-90).
    unstarted: Option<String>,
}

impl Scratch {
    /// A new directory under the system's temporary directory, held open, to be removed by `until` (M1T07R-44).
    fn new(
        until: Instant,
        given_up: std::sync::Arc<std::sync::atomic::AtomicBool>,
        held: Held,
    ) -> Result<Scratch, ProbeError> {
        let dir = std::env::temp_dir().join(unique_name("hoh-probe"));
        Scratch::at(dir, until, given_up, held).map_err(ProbeError::from)
    }

    /// A new directory at `dir`, held open.
    fn at(
        dir: PathBuf,
        until: Instant,
        given_up: std::sync::Arc<std::sync::atomic::AtomicBool>,
        held: Held,
    ) -> io::Result<Scratch> {
        use rustix::fs::{Mode, OFlags};
        let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
            return Err(io::Error::other("a scratch directory needs a parent"));
        };
        let temp = rustix::fs::openat(
            rustix::fs::CWD,
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let owned = make_dir(temp.as_fd(), name, &dir)?;
        held.add(owned.as_fd());
        Ok(Scratch {
            dir,
            temp,
            owned,
            given_up,
            held,
            until,
            removed: None,
            unstarted: None,
        })
    }

    /// Whether `dir` is still the directory the probe made.
    fn holds(&self) -> bool {
        match (self.dir.file_name(), rustix::fs::fstat(&self.owned)) {
            (Some(name), Ok(made)) => names(self.temp.as_fd(), Path::new(name), inode(&made)),
            _ => false,
        }
    }

    /// Remove it, once; true when it is gone.
    fn remove(&mut self) -> bool {
        if let Some(gone) = self.removed {
            return gone;
        }
        let work = Work::new(u64::MAX, Some(self.until)).stopped_by(&self.given_up);
        let gone = work.spend(1)
            && match Quarantine::new(self.temp.as_fd(), &self.dir, &self.held) {
                Ok(quarantine) => {
                    remove_within(
                        &self.dir,
                        self.temp.as_fd(),
                        self.owned.as_fd(),
                        &quarantine,
                        &work,
                    ) & quarantine.remove(self.temp.as_fd())
                }
                Err(e) => {
                    self.unstarted = Some(format!("no quarantine to remove it through: {e}"));
                    false
                }
            };
        self.removed = Some(gone);
        gone
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.remove();
    }
}

/// A new directory `name` in `temp` (at `path`), made and opened as the probe's own: the directory opened must be the
/// one just made, not one put in its place between making and opening it (M1T07R-51, -57). Linux cannot make and open a
/// directory in one call, so what is opened is checked: this process's own, empty, and born no earlier than a second
/// before the call: a directory born more than a second before is refused (M1T07R-102). One a same-user process
/// watching `temp` makes and puts in place within that second cannot be told from the probe's own; that is outside the
/// guarantee (ADR-015 decision 2, M1T07R-57, -91): a birth time cannot close the gap, a shorter room only narrows it.
/// On any doubt nothing is removed by name — whatever is at `name` may not be the probe's — and the error says where
/// it may be left.
fn make_dir(
    temp: std::os::fd::BorrowedFd<'_>,
    name: &OsStr,
    path: &Path,
) -> io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    // File system clocks are coarse (a directory's birth time may read a tick before now): a second of room.
    let since = SystemTime::now() - Duration::from_secs(1);
    rustix::fs::mkdirat(temp, name, Mode::from_raw_mode(0o700))?;
    between("made", path);
    let refused = |why: &str| {
        io::Error::other(format!(
            "the directory {} {why}; it may be left there",
            path.display()
        ))
    };
    let owned = open_beneath(
        temp,
        Path::new(name),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
    )
    .map_err(|e| refused(&format!("could not be opened as made ({e})")))?;
    match made_since(owned.as_fd(), since) {
        Some(true) => Ok(owned),
        Some(false) => Err(refused("was replaced as it was made")),
        None => Err(refused(
            "cannot be told from one put in its place: its file system records no birth time",
        )),
    }
}

/// Whether the directory `fd` is this process's own, empty and born at `since` or later; `None` when its file system
/// records no birth time.
fn made_since(fd: std::os::fd::BorrowedFd<'_>, since: SystemTime) -> Option<bool> {
    use rustix::fs::{AtFlags, StatxFlags, statx};
    let stat = statx(
        fd,
        "",
        AtFlags::EMPTY_PATH,
        StatxFlags::BTIME | StatxFlags::UID,
    )
    .ok()?;
    if stat.stx_mask & StatxFlags::BTIME.bits() == 0 {
        return None;
    }
    let born = u64::try_from(stat.stx_btime.tv_sec)
        .ok()
        .map(|s| UNIX_EPOCH + Duration::new(s, stat.stx_btime.tv_nsec));
    let empty = rustix::fs::Dir::read_from(fd).is_ok_and(|mut dir| {
        std::iter::from_fn(|| dir.read())
            .all(|e| e.is_ok_and(|e| e.file_name() == c"." || e.file_name() == c".."))
    });
    Some(
        stat.stx_uid == rustix::process::geteuid().as_raw()
            && empty
            && born.is_some_and(|born| born >= since),
    )
}

/// `<prefix>-<pid>-<nanoseconds>-<sequence>`: the sequence counts the names this process made, so names made at the
/// same moment on several threads differ (FU-20).
fn unique_name(prefix: &str) -> String {
    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{prefix}-{}-{nanos}-{n}", std::process::id())
}

/// How everything under the scratch directory is reached: never above where the resolution starts, never through a
/// link, never into another file system. A mount is `EXDEV` before the file system mounted there is asked anything,
/// so one that never answers (a FUSE mount a helper put there) cannot hold the probe (M1T07R-44, -49, -51).
const BENEATH: rustix::fs::ResolveFlags = rustix::fs::ResolveFlags::BENEATH
    .union(rustix::fs::ResolveFlags::NO_SYMLINKS)
    .union(rustix::fs::ResolveFlags::NO_XDEV)
    .union(rustix::fs::ResolveFlags::NO_MAGICLINKS);

/// `rel` (names from listings, joined; empty for `at` itself) opened from `at` as it is now, [`BENEATH`] it: what is
/// opened was under `at` at that moment, wherever a directory on the way was before or is after (M1T07R-51). Never
/// waits on a FIFO (`flags` carries `O_NONBLOCK` where it could be one). A kernel without `openat2`, or one a seccomp
/// filter keeps from it, opens nothing (`ENOSYS`): the probe fails rather than reach the output another way.
fn open_beneath(
    at: std::os::fd::BorrowedFd<'_>,
    rel: &Path,
    flags: rustix::fs::OFlags,
) -> rustix::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags, openat2};
    #[cfg(test)]
    if rel.components().any(|c| c.as_os_str() == "hoh-test-mount") {
        return Err(rustix::io::Errno::XDEV);
    }
    let rel = if rel.as_os_str().is_empty() {
        Path::new(".")
    } else {
        rel
    };
    match openat2(at, rel, flags | OFlags::CLOEXEC, Mode::empty(), BENEATH) {
        Err(rustix::io::Errno::NOSYS | rustix::io::Errno::PERM) => Err(rustix::io::Errno::NOSYS),
        other => other,
    }
}

/// Whether `rel` under `at`, not followed and not crossing into a mount, is `made` (M1T07R-44, -49).
fn names(at: std::os::fd::BorrowedFd<'_>, rel: &Path, made: Inode) -> bool {
    use rustix::fs::OFlags;
    open_beneath(at, rel, OFlags::PATH | OFlags::NOFOLLOW)
        .and_then(|fd| rustix::fs::fstat(&fd))
        .is_ok_and(|now| inode(&now) == made)
}

/// What a failed `open_beneath` means for the output.
fn beneath_error(path: &Path, e: rustix::io::Errno) -> String {
    match e {
        rustix::io::Errno::XDEV => format!(
            "{} is a mount point or left the output while it was read; the probe reads no other file system and \
             nothing outside its output",
            path.display()
        ),
        rustix::io::Errno::NOSYS => {
            "the kernel refuses openat2, which the probe reads its output with".to_owned()
        }
        e => format!("{}: {e}", path.display()),
    }
}

/// The files of the schema output directory `owned` (at `root`) with their bytes, by path (M1T07R-40, -44, -51).
/// Directories are listed for names only; every entry is opened from `owned` by its path under it, [`BENEATH`] it,
/// and judged as opened, so what a helper the CLI left running moves, links or mounts during the walk cannot take it
/// outside the output: links and what is neither a directory nor a regular file are skipped, a mount refuses the
/// output. Every entry costs a step; more than [`MAX_SCHEMA_FILES`] entries, or nesting deeper than
/// [`MAX_SCHEMA_DEPTH`], refuse the output; a file is refused by its size before it is read and read no further than
/// the limits leave, so one that grows meanwhile cannot grow the reading.
fn schema_files(
    root: &Path,
    owned: std::os::fd::BorrowedFd<'_>,
    work: &Work,
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    use rustix::fs::{Dir, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let failed = |path: &Path, e: &dyn fmt::Display| format!("{}: {e}", path.display());
    between("walk", root);
    let top = open_beneath(owned, Path::new(""), flags | OFlags::DIRECTORY)
        .and_then(Dir::new)
        .map_err(|e| beneath_error(root, e))?;
    let mut out = Vec::new();
    let (mut entries, mut total) = (0usize, 0u64);
    // Each directory being listed, with its path under the output.
    let mut stack = vec![(top, PathBuf::new())];
    while let Some((reader, at)) = stack.last_mut() {
        let Some(entry) = reader.read() else {
            stack.pop();
            continue;
        };
        let entry = entry.map_err(|e| failed(&root.join(&*at), &e))?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        entries += 1;
        if entries > MAX_SCHEMA_FILES {
            return Err(format!(
                "too many schema files or directories (more than {MAX_SCHEMA_FILES})"
            ));
        }
        if !work.spend(1) {
            return Err("the schema output was not read within the time limit".to_owned());
        }
        let rel = at.join(OsStr::from_bytes(name.to_bytes()));
        let path = root.join(&rel);
        between("entry", &path);
        // What is judged is what is read: the entry as opened, not as listed.
        let file = match open_beneath(owned, &rel, flags) {
            Ok(fd) => std::fs::File::from(fd),
            // A link, a socket, or an entry moved or removed meanwhile.
            Err(rustix::io::Errno::LOOP | rustix::io::Errno::NXIO | rustix::io::Errno::NOENT) => {
                continue;
            }
            Err(e) => return Err(beneath_error(&path, e)),
        };
        let meta = file.metadata().map_err(|e| failed(&path, &e))?;
        if meta.is_dir() {
            if stack.len() > MAX_SCHEMA_DEPTH {
                return Err(format!(
                    "the schema output is nested too deep: {} (at most {MAX_SCHEMA_DEPTH} directories)",
                    path.display()
                ));
            }
            let dir = Dir::new(std::os::fd::OwnedFd::from(file)).map_err(|e| failed(&path, &e))?;
            stack.push((dir, rel));
        } else if meta.is_file() {
            // Another link can be a file outside the output: such a file is not read (M1T07R-46).
            if std::os::unix::fs::MetadataExt::nlink(&meta) > 1 {
                return Err(format!(
                    "{} has {} links; a file of the output has one",
                    path.display(),
                    std::os::unix::fs::MetadataExt::nlink(&meta)
                ));
            }
            let room = MAX_SCHEMA_FILE.min(MAX_SCHEMA_TOTAL - total);
            if meta.len() > room {
                return Err(format!(
                    "the schema output is too large: {} is {} bytes, {} in all (at most {MAX_SCHEMA_FILE} a file, {MAX_SCHEMA_TOTAL} in all)",
                    path.display(),
                    meta.len(),
                    total.saturating_add(meta.len())
                ));
            }
            if !work.spend(usize::try_from(meta.len()).unwrap_or(usize::MAX)) {
                return Err("the schema output was not read within the time limit".to_owned());
            }
            between("read", &path);
            let bytes = read_bounded(file, room)
                .map_err(|e| failed(&path, &e))?
                .ok_or_else(|| {
                    format!(
                        "{} changed while it was read: more than {room} bytes",
                        path.display()
                    )
                })?;
            total += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            out.push((path, bytes));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// At most `room` bytes of `file`; `None` when it holds more, having read one byte more.
fn read_bounded(file: impl Read, room: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    file.take(room.saturating_add(1)).read_to_end(&mut bytes)?;
    Ok((u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= room).then_some(bytes))
}

/// A directory of the probe's own, made beside its scratch directory once the CLI has exited and never named to it,
/// that what is removed passes through: an entry is moved in, checked to be what was opened under the output, and
/// only then removed (M1T07R-51).
struct Quarantine {
    name: String,
    fd: std::os::fd::OwnedFd,
    made: Inode,
    /// How many entries were moved in.
    taken: std::cell::Cell<u64>,
    /// Entries moved in that are still there.
    left: std::cell::Cell<u64>,
}

impl Quarantine {
    /// Made in `temp` beside the scratch directory `of`, named after it; shared in `held` so what stays in it can be
    /// named. Not made, the error says why and where what was made may be left (M1T07R-90).
    fn new(temp: std::os::fd::BorrowedFd<'_>, of: &Path, held: &Held) -> io::Result<Quarantine> {
        let of_name = of.file_name().unwrap_or(of.as_os_str()).to_string_lossy();
        let name = unique_name(&format!("{of_name}-q"));
        // Its own as the scratch directory is: one put in its place is never moved into or removed (M1T07R-57).
        let fd = make_dir(temp, OsStr::new(&name), &of.with_file_name(&name))?;
        held.add(fd.as_fd());
        let made = inode(&rustix::fs::fstat(&fd)?);
        Ok(Quarantine {
            name,
            fd,
            made,
            taken: Default::default(),
            left: Default::default(),
        })
    }

    /// Move `name` out of `parent` and remove it, if it is `opened`: true when it is gone. Whatever else was moved
    /// in (something put in its place since it was opened, or a directory not empty) goes back where it was found, or
    /// stays here.
    fn take(
        &self,
        parent: std::os::fd::BorrowedFd<'_>,
        name: &OsStr,
        opened: Inode,
        directory: bool,
        path: &Path,
    ) -> bool {
        use rustix::fs::{AtFlags, RenameFlags, renameat_with, statat, unlinkat};
        let n = self.taken.get();
        self.taken.set(n + 1);
        let here = n.to_string();
        between("take", path);
        if renameat_with(
            parent,
            name,
            &self.fd,
            here.as_str(),
            RenameFlags::NOREPLACE,
        )
        .is_err()
        {
            return false;
        }
        let what = statat(&self.fd, here.as_str(), AtFlags::SYMLINK_NOFOLLOW);
        let flags = if directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        if what.is_ok_and(|s| inode(&s) == opened)
            && unlinkat(&self.fd, here.as_str(), flags).is_ok()
        {
            return true;
        }
        if renameat_with(
            &self.fd,
            here.as_str(),
            parent,
            name,
            RenameFlags::NOREPLACE,
        )
        .is_err()
        {
            self.left.set(self.left.get() + 1);
        }
        false
    }

    /// Remove the quarantine itself if nothing stayed in it: true when it is gone.
    fn remove(&self, temp: std::os::fd::BorrowedFd<'_>) -> bool {
        self.left.get() == 0
            && names(temp, Path::new(&self.name), self.made)
            && rustix::fs::unlinkat(temp, self.name.as_str(), rustix::fs::AtFlags::REMOVEDIR)
                .is_ok()
    }
}

/// Remove what is under the scratch directory `owned` (made as `dir` in `temp`) and then `owned` itself, every step
/// spending from `work`; true when it is gone (M1T07R-40, -44, -51). Entries are reached as the reading reaches them
/// ([`open_beneath`] from `owned`) and none is unlinked where it is: each is moved from its parent, opened under
/// `owned` just before, through `quarantine`, where it is removed only if it is the entry opened under `owned`, held
/// open until then so its inode cannot be another's; so nothing that was never under the output is removed, whatever
/// a helper the CLI left running moves meanwhile. Everything but directories goes first, then the directories,
/// children before their parent, so a directory enters the quarantine, where a helper holding it could find the
/// quarantine (`..`), once nothing but empty directories is left to remove; but one a helper refilled after it was
/// walked enters it too, refused as not empty and moved back, and its new entries then pass through the same quarantine
/// in the next round: that is outside the guarantee (a helper reaching the quarantine, ADR-015 decision 2), and every
/// take still checks the entry it opened (M1T07R-101). A mount is left untouched, and so is
/// what is nested deeper than [`MAX_SCHEMA_DEPTH`]. `dir` itself goes last, only while `dir` still names it; a link
/// put in its place is removed the same way.
fn remove_within(
    dir: &Path,
    temp: std::os::fd::BorrowedFd<'_>,
    owned: std::os::fd::BorrowedFd<'_>,
    quarantine: &Quarantine,
    work: &Work,
) -> bool {
    use rustix::fs::fstat;
    // Everything but directories, again while a pass removes something, then the directories; a helper may have put
    // other entries in meanwhile (a directory replaced by a link after it was walked): met by the directories' pass,
    // they start another round, at most [`REMOVE_ROUNDS`]. What cannot be removed, such as a mount, ends the passes.
    for _ in 0..REMOVE_ROUNDS {
        loop {
            match remove_pass(dir, owned, quarantine, work, false) {
                None => return false,
                Some(pass) if pass.removed == 0 => break,
                Some(_) => {}
            }
        }
        match remove_pass(dir, owned, quarantine, work, true) {
            None => return false,
            Some(pass) if pass.others == 0 => break,
            Some(_) => {}
        }
    }
    let (Ok(made), Some(name)) = (fstat(owned), dir.file_name()) else {
        return false;
    };
    // At `dir` now: the directory itself, or a link put in its place, which goes too (removing a link never touches
    // what it names); anything else put there is left.
    if let Ok(at) = open_beneath(
        temp,
        Path::new(name),
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW,
    ) && let Ok(now) = fstat(&at)
    {
        if inode(&now) == inode(&made) {
            quarantine.take(temp, name, inode(&made), true, dir);
        } else if rustix::fs::FileType::from_raw_mode(now.st_mode) == rustix::fs::FileType::Symlink
        {
            quarantine.take(temp, name, inode(&now), false, dir);
        }
    }
    fstat(owned).is_ok_and(|made| made.st_nlink == 0)
}

/// How many rounds [`remove_within`] goes through at most: one more for what a helper put in after its directory was
/// walked. What a helper keeps putting in is left and named; the deadline bounds the passes (M1T07R-100).
const REMOVE_ROUNDS: usize = 2;

/// What one pass of [`remove_pass`] did.
struct Pass {
    /// Entries removed.
    removed: usize,
    /// Entries other than directories met by a directories' pass (left for the next round).
    others: usize,
}

/// One pass of [`remove_within`] over what is under `owned`: the directories (`directories`), children first, or
/// everything else; `None` when the work ran out.
fn remove_pass(
    dir: &Path,
    owned: std::os::fd::BorrowedFd<'_>,
    quarantine: &Quarantine,
    work: &Work,
    directories: bool,
) -> Option<Pass> {
    use rustix::fs::{Dir, FileType, OFlags, fstat};
    let listing = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    // `rel`'s entry taken out of its parent, if it is still `opened`, which is held open meanwhile.
    let take = |rel: &Path, opened: std::os::fd::BorrowedFd<'_>, directory: bool| -> bool {
        let (Some(parent), Some(name), Ok(stat)) = (rel.parent(), rel.file_name(), fstat(opened))
        else {
            return false;
        };
        open_beneath(
            owned,
            parent,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        )
        .is_ok_and(|at| quarantine.take(at.as_fd(), name, inode(&stat), directory, &dir.join(rel)))
    };
    let (mut removed, mut others) = (0, 0);
    let Ok(top) = open_beneath(owned, Path::new(""), listing).and_then(Dir::new) else {
        return None;
    };
    // Each directory being gone through: its listing (which holds it open) and its path under `owned`.
    let mut stack = vec![(top, PathBuf::new())];
    loop {
        if !work.spend(1) {
            return None;
        }
        let within = stack.len() <= MAX_SCHEMA_DEPTH;
        let Some((reader, at)) = stack.last_mut() else {
            return Some(Pass { removed, others });
        };
        let Some(Ok(entry)) = reader.read() else {
            // Gone through: in the directories' pass, taken out of the directory holding it.
            if let Some((reader, rel)) = stack.pop()
                && directories
                && !rel.as_os_str().is_empty()
                && let Ok(opened) = reader.fd()
            {
                removed += usize::from(take(&rel, opened, true));
            }
            continue;
        };
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        let rel = at.join(OsStr::from_bytes(name.to_bytes()));
        between("remove", &dir.join(&rel));
        let Ok(opened) = open_beneath(owned, &rel, OFlags::PATH | OFlags::NOFOLLOW) else {
            // Gone, moved out, or a mount point: a mount is left, and nothing in it is touched (M1T07R-49).
            continue;
        };
        let Ok(stat) = fstat(&opened) else {
            continue;
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            if directories {
                others += 1;
            } else {
                removed += usize::from(take(&rel, opened.as_fd(), false));
            }
        } else if within && let Ok(child) = open_beneath(owned, &rel, listing).and_then(Dir::new) {
            stack.push((child, rel));
        }
    }
}

/// Claude Code: `-p --output-format stream-json`; partial messages and resume from `--help`.
fn probe_claude(
    launcher: &Path,
    env: &ProbeEnv,
    caps: &mut BTreeMap<String, Option<bool>>,
) -> Result<Probed, ProbeError> {
    let help = run_ok(launcher, &["--help"], env)?.stdout;
    // What the adapter needs to start a turn, by its own rule (M1R-03).
    if let Some(detail) = crate::claude::interface_missing(&String::from_utf8_lossy(&help)) {
        return Err(ProbeError::Unsupported {
            agent: Agent::Claude,
            detail,
        });
    }
    set(caps, "structured_events", true);
    set(
        caps,
        "partial_messages",
        crate::adapter::offers(&String::from_utf8_lossy(&help), crate::claude::PARTIAL_FLAG),
    );
    set(
        caps,
        "resume",
        crate::adapter::offers(&String::from_utf8_lossy(&help), "--resume"),
    );
    Ok((Transport::StreamJson, help, None))
}

/// Pi: `--mode rpc`; resume from the session options in `--help`.
fn probe_pi(
    launcher: &Path,
    env: &ProbeEnv,
    caps: &mut BTreeMap<String, Option<bool>>,
) -> Result<Probed, ProbeError> {
    let help = run_ok(launcher, &["--help"], env)?.stdout;
    // What the adapter needs to start a turn, by its own rule (M1R-03).
    if let Some(detail) = crate::pi::interface_missing(&String::from_utf8_lossy(&help)) {
        return Err(ProbeError::Unsupported {
            agent: Agent::Pi,
            detail,
        });
    }
    set(caps, "structured_events", true);
    set(
        caps,
        "resume",
        crate::adapter::offers(&String::from_utf8_lossy(&help), "--session-id")
            || crate::adapter::offers(&String::from_utf8_lossy(&help), "--resume"),
    );
    Ok((Transport::Rpc, help, None))
}

/// First stdout line of `<cli> --version`, verbatim.
fn version_line(launcher: &Path, env: &ProbeEnv) -> Result<String, ProbeError> {
    let out = run_ok(launcher, &["--version"], env)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end_matches('\r')
        .to_owned();
    if line.is_empty() || line.chars().count() > 160 {
        return Err(ProbeError::Failed {
            argv: argv(launcher, &["--version"]),
            detail: format!("unusable version line {line:?}"),
        });
    }
    Ok(line)
}

/// The first executable `name` on `path`.
pub fn find_on_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .map(|found| std::path::absolute(&found).unwrap_or(found))
}

/// The file that actually implements the agent: Codex's npm wrapper runs a vendored native binary, Pi's shell
/// launcher runs `install/releases/<current-version>`, Claude's launcher is a link to its versioned binary. A
/// launcher that is not one of those layouts is its own implementation.
pub fn resolve_implementation(agent: Agent, launcher: &Path) -> Result<PathBuf, ProbeError> {
    let real = std::fs::canonicalize(launcher)?;
    let found = match agent {
        Agent::Codex => codex_native(&real)?,
        Agent::Pi => real.parent().and_then(Path::parent).and_then(pi_release),
        Agent::Claude => None,
    };
    Ok(match found {
        Some(path) => std::fs::canonicalize(path)?,
        None => real,
    })
}

/// The target triple and platform package `codex.js` selects for this host (its `PLATFORM_PACKAGE_BY_TARGET`).
fn codex_target() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux" | "android", "x86_64") => {
            Some(("x86_64-unknown-linux-musl", "@openai/codex-linux-x64"))
        }
        ("linux" | "android", "aarch64") => {
            Some(("aarch64-unknown-linux-musl", "@openai/codex-linux-arm64"))
        }
        ("macos", "x86_64") => Some(("x86_64-apple-darwin", "@openai/codex-darwin-x64")),
        ("macos", "aarch64") => Some(("aarch64-apple-darwin", "@openai/codex-darwin-arm64")),
        _ => None,
    }
}

/// Node's `require.resolve(<package>/package.json)` from `start`: `node_modules/<package>` in `start` and each
/// ancestor (directories that are themselves named node_modules are skipped), returning the package directory.
/// The first package found is the answer: if its `exports` do not export `./package.json`, Node throws
/// (ERR_PACKAGE_PATH_NOT_EXPORTED) instead of looking further, and this returns `Ok(None)` (M1T01R-13). Export
/// forms whose outcome needs Node's full resolver are refused (M1T01R-17).
fn node_resolve(start: &Path, package: &str) -> Result<Option<PathBuf>, String> {
    let Some(found) = start
        .ancestors()
        .filter(|dir| dir.file_name() != Some(OsStr::new("node_modules")))
        .map(|dir| dir.join("node_modules").join(package))
        .find(|candidate| candidate.join("package.json").is_file())
    else {
        return Ok(None);
    };
    let manifest: serde_json::Value = std::fs::read(found.join("package.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| format!("{}: unreadable package.json", found.display()))?;
    match package_json_export(manifest.get("exports")) {
        Export::Resolvable => Ok(Some(found)),
        Export::NotExported => Ok(None),
        Export::Unsupported => Err(format!(
            "{}: its exports need Node's full resolver to tell which codex binary runs",
            found.display()
        )),
    }
}

enum Export {
    Resolvable,
    NotExported,
    Unsupported,
}

/// What `require.resolve("<package>/package.json")` does for an `exports` field, in the forms whose outcome is
/// certain: no field (or null) resolves it; a string or a subpath map without `./package.json` and without
/// patterns exports only other entries; `"./package.json": "./package.json"` resolves it. Conditions, arrays,
/// patterns, redirects and null overrides are left to Node and refused here.
fn package_json_export(exports: Option<&serde_json::Value>) -> Export {
    use serde_json::Value;
    match exports {
        None | Some(Value::Null) => Export::Resolvable,
        Some(Value::String(_)) => Export::NotExported,
        Some(Value::Object(map)) if !map.is_empty() && map.keys().all(|k| k.starts_with('.')) => {
            if map.keys().any(|k| k.contains('*')) {
                return Export::Unsupported;
            }
            match map.get("./package.json") {
                None => Export::NotExported,
                Some(Value::String(target)) if target == "./package.json" => Export::Resolvable,
                Some(_) => Export::Unsupported,
            }
        }
        Some(_) => Export::Unsupported,
    }
}

/// Whether `launcher` (a real path) is the npm wrapper `bin/codex.js` of the `@openai/codex` package; only then does
/// it run a vendored binary. Anything else (a directly installed binary, a fake) is its own implementation.
fn is_codex_npm_wrapper(launcher: &Path) -> bool {
    let package = launcher.parent().and_then(Path::parent);
    let manifest = package
        .and_then(|p| std::fs::read(p.join("package.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    launcher.file_name() == Some(OsStr::new("codex.js"))
        && launcher.parent().and_then(Path::file_name) == Some(OsStr::new("bin"))
        && manifest.is_some_and(|m| m["name"] == "@openai/codex")
}

/// The native binary `codex.js` (at its real path `launcher`) executes: `vendor/<triple>/bin/codex` in the platform
/// package it resolves, else in the codex package's own `vendor` directory (M1T01R-02).
fn codex_native(launcher: &Path) -> Result<Option<PathBuf>, ProbeError> {
    let (Some((triple, package)), Some(start), true) = (
        codex_target(),
        launcher.parent(),
        is_codex_npm_wrapper(launcher),
    ) else {
        return Ok(None);
    };
    let unsupported = |detail: String| ProbeError::Unsupported {
        agent: Agent::Codex,
        detail,
    };
    let vendor = match node_resolve(start, package).map_err(unsupported)? {
        Some(dir) => std::fs::canonicalize(dir)?.join("vendor"),
        None => match start.parent() {
            Some(root) => root.join("vendor"),
            None => return Ok(None),
        },
    };
    let native = vendor.join(triple).join("bin/codex");
    Ok(native.is_file().then_some(native))
}

fn pi_release(agent_dir: &Path) -> Option<PathBuf> {
    let version = std::fs::read_to_string(agent_dir.join("install/current-version")).ok()?;
    let version = version.lines().next()?.trim();
    let valid = !version.is_empty()
        && version != "."
        && version != ".."
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
    let bin = agent_dir
        .join("install/releases")
        .join(version)
        .join("node_modules/.bin/pi");
    (valid && bin.is_file()).then_some(bin)
}

/// The suite's fixture set: the tree hash of `<fixture_root>/<agent>/<x.y.z>` for the probed version, or the hash of
/// the empty set when no fixtures exist for it.
fn fixture_set_sha256(
    env: &ProbeEnv,
    agent: Agent,
    cli_version: &str,
) -> Result<String, ProbeError> {
    let dir = env.fixture_root.as_ref().and_then(|root| {
        let version = cli_version
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .find(|t| t.split('.').count() == 3 && !t.starts_with('.'))?;
        Some(root.join(agent.command()).join(version))
    });
    match dir.filter(|d| d.is_dir()) {
        Some(dir) => Ok(tree_sha256(&dir)?),
        None => Ok(hex(&Sha256::digest(b""))),
    }
}

fn files_under(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

/// sha256 over the sorted lines `<relative path> <sha256 of file>\n` of every file under `root` (the same tree hash
/// tests/fixtures/protocol/record.py uses).
pub fn tree_sha256(root: &Path) -> io::Result<String> {
    let mut digests = Vec::new();
    for file in files_under(root)? {
        let digest = file_sha256(&file)?;
        digests.push((file, digest));
    }
    tree_sha256_from(root, &digests)
}

/// [`tree_sha256`] over files already read (each under `root`); each file's bytes are counted in `work` before it is
/// hashed, so hashing reads the deadline between files (M1T07R-40, -44).
fn tree_sha256_of(root: &Path, files: &[(PathBuf, Vec<u8>)], work: &Work) -> io::Result<String> {
    let mut digests = Vec::new();
    for (file, bytes) in files {
        if !work.spend(bytes.len()) {
            return Err(io::Error::other("not hashed within the time limit"));
        }
        digests.push((file.clone(), hex(&Sha256::digest(bytes))));
    }
    tree_sha256_from(root, &digests)
}

/// sha256 over the sorted lines `<path relative to root> <digest>\n`.
fn tree_sha256_from(root: &Path, digests: &[(PathBuf, String)]) -> io::Result<String> {
    let mut lines = Vec::new();
    for (file, digest) in digests {
        let rel = file.strip_prefix(root).map_err(io::Error::other)?;
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        lines.push(format!("{rel} {digest}\n"));
    }
    lines.sort();
    Ok(hex(&Sha256::digest(lines.concat().as_bytes())))
}

/// The adapters' streaming hash, one helper for both (M1R-10).
fn file_sha256(path: &Path) -> io::Result<String> {
    crate::adapter::sha256_file(path)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The end and output of a command.
#[derive(Debug)]
pub struct Captured {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    /// Empty unless asked for ([`run_status`]).
    pub stderr: Vec<u8>,
}

fn argv(program: &Path, args: &[&str]) -> Vec<String> {
    std::iter::once(program.display().to_string())
        .chain(args.iter().map(|a| (*a).to_owned()))
        .collect()
}

fn run_ok(program: &Path, args: &[&str], env: &ProbeEnv) -> Result<Captured, ProbeError> {
    let out = run(program, args, env)?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(ProbeError::Failed {
            argv: argv(program, args),
            detail: format!("exited with {}", out.status),
        })
    }
}

/// Run a probe command with no stdin in its own process group; kill the group if it outlives the timeout.
fn run(program: &Path, args: &[&str], env: &ProbeEnv) -> Result<Captured, ProbeError> {
    logged(program, args, env, false)
}

/// Run a native status command (no model call) as [`run`] does, keeping stdout and stderr: some CLIs write their
/// conclusion to stderr (`codex login status`).
pub fn run_status(program: &Path, args: &[&str], env: &ProbeEnv) -> Result<Captured, ProbeError> {
    logged(program, args, env, true)
}

/// [`capture`], written down in `env.log`.
fn logged(
    program: &Path,
    args: &[&str],
    env: &ProbeEnv,
    stderr: bool,
) -> Result<Captured, ProbeError> {
    let started = Instant::now();
    let result = capture(program, args, env, stderr);
    if let Some(log) = &env.log
        && let Ok(mut runs) = log.lock()
    {
        runs.push(Run {
            argv: argv(program, args),
            exit_code: result.as_ref().ok().and_then(|c| c.status.code()),
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            timed_out: matches!(result, Err(ProbeError::TimedOut { .. })),
            started: !matches!(result, Err(ProbeError::NotStarted { .. })),
        });
    }
    result
}

fn capture(
    program: &Path,
    args: &[&str],
    env: &ProbeEnv,
    keep_stderr: bool,
) -> Result<Captured, ProbeError> {
    let failed = |detail: String| ProbeError::Failed {
        argv: argv(program, args),
        detail,
    };
    // The limit is known before anything starts: past the shared deadline nothing is run (M1T07R-08).
    let limit = env.deadline.map_or(env.timeout, |shared| {
        env.timeout
            .min(shared.saturating_duration_since(Instant::now()))
    });
    if limit.is_zero() {
        return Err(ProbeError::NotStarted {
            argv: argv(program, args),
        });
    }
    // One absolute end for the retries below and the wait after the start (M1T07R-11).
    let end = Instant::now() + limit;
    let spawn = || {
        between("spawning", program.parent().unwrap_or(program));
        Command::new(program)
            .args(args)
            .env("PATH", &env.path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(if keep_stderr {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .process_group(0)
            .spawn()
    };
    // ETXTBSY is transient: a process forked elsewhere still holds a just-written executable open for writing until
    // it execs (a known Linux race when files are written and executed concurrently). Retry a few times.
    // Retry a few times, but never past the end: then the command is not started.
    // Registered for the stop before it starts: none starts once whoever probes stopped (M2T07R-300, -450).
    let start = || match &env.cancel {
        Some(stop) => stop.start(program.parent().unwrap_or(program), spawn),
        None => Some(spawn().map(|child| Started::new(Slot::default(), child, None))),
    };
    let mut attempt = 0;
    let started = loop {
        match start() {
            None => return Err(failed(STOPPING.to_owned())),
            Some(Err(e)) if e.kind() == io::ErrorKind::ExecutableFileBusy && attempt < 20 => {
                attempt += 1;
                let left = end.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(ProbeError::NotStarted {
                        argv: argv(program, args),
                    });
                }
                std::thread::sleep(Duration::from_millis(10 * attempt).min(left));
                if Instant::now() >= end {
                    return Err(ProbeError::NotStarted {
                        argv: argv(program, args),
                    });
                }
            }
            Some(other) => break other.map_err(|e| failed(format!("cannot start: {e}")))?,
        }
    };
    // From here on every exit, an error or a panic included, ends the command (M1T07R-53).
    let (stdout, stderr) = (started.stdout, started.stderr);
    let mut owned = Owned {
        slot: started.slot,
        pid: started.pid,
    };
    between("started", program.parent().unwrap_or(program));
    // Both outputs are read here, between looks at the command, up to their limit and the end: nothing reads them
    // once the probe returns, so a helper that left the group and holds a pipe open keeps nothing of it (M1T07R-44,
    // -47).
    let mut outputs = [
        Output::new(stdout.map(std::os::fd::OwnedFd::from))?,
        Output::new(stderr.map(std::os::fd::OwnedFd::from))?,
    ];
    let timed_out = || ProbeError::TimedOut {
        argv: argv(program, args),
        after: limit,
        cut_by_deadline: limit < env.timeout,
    };
    let pid = owned.pid;
    let exited = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
    let mut status = None;
    loop {
        if status.is_some() && outputs.iter().all(|o| o.file.is_none()) {
            break;
        }
        let now = Instant::now();
        if now >= end {
            // Output still open at the end is a timeout too, and what comes later is not taken (M1T07R-13).
            owned.end();
            return Err(timed_out());
        }
        // Whoever probes stopped: ended here, its starter, as well as by the stop, so a command that started while the
        // stop found its slot empty, or whose escaped child holds its output, ends at once (decision 10 v5).
        if cancelled(env) {
            owned.end();
            return Err(failed(STOPPING.to_owned()));
        }
        let wait = end
            .saturating_duration_since(now)
            .min(Duration::from_millis(10));
        wait_for_output(&outputs, wait)?;
        for output in &mut outputs {
            output
                .drain()
                .map_err(|e| failed(format!("reading output: {e}")))?;
        }
        // Observe the exit without reaping: while the leader is a zombie its process group id cannot be reused.
        if status.is_none()
            && waitid(WaitId::Pid(pid), exited)
                .map_err(io::Error::from)?
                .is_some()
        {
            // Whatever the command left running in its group goes with it (M1T01R-03), then the leader is reaped.
            status = Some(
                owned
                    .end()
                    .ok_or_else(|| io::Error::other("the command exited but was not reaped"))?,
            );
        }
    }
    let Some(status) = status else {
        return Err(timed_out());
    };
    // Ended while whoever probes stops: by the stop's kill, or at least not a reading kept (M2T07R-300).
    if cancelled(env) {
        return Err(failed(STOPPING.to_owned()));
    }
    if outputs.iter().any(|o| o.over) {
        return Err(failed(format!(
            "more output than the probe reads ({MAX_OUTPUT} bytes)"
        )));
    }
    let [stdout, stderr] = outputs.map(|o| o.bytes);
    Ok(Captured {
        status,
        stdout,
        stderr,
    })
}

/// How long [`Owned::end`] waits for a killed command's leader to be reaped.
const REAP_WAIT: Duration = Duration::from_secs(1);

/// A command the probe started, owned from the moment it started: ended ([`Owned::end`]) when dropped, so no exit
/// of [`capture`] leaves it running (M1T07R-53).
struct Owned {
    /// Its child, `None` once ended; a stop signals it while it is there (M2T07R-300).
    slot: Slot,
    pid: Pid,
}

impl Owned {
    /// Kill its process group and its leader, and reap the leader, once; the exit status when reaped within
    /// [`REAP_WAIT`]. Both are signalled only while the leader is not reaped, so the id is still its own; the leader
    /// is killed on its own too, as it may have moved itself into another group (M1T07R-80). A leader that does not
    /// die by then (a call SIGKILL does not end) is reaped by a thread of its own.
    fn end(&mut self) -> Option<ExitStatus> {
        // Taken out and signalled under the slot's lock: a stop never signals a number once it may be reaped.
        let mut child = {
            let mut held = locked(&self.slot);
            let child = held.take()?;
            let _ = kill_process_group(self.pid, Signal::KILL);
            let _ = kill_process(self.pid, Signal::KILL);
            child
        };
        let until = Instant::now() + REAP_WAIT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(2)),
                _ => break,
            }
        }
        let _ = std::thread::Builder::new()
            .name("hoh-reap".to_owned())
            .spawn(move || {
                let _ = child.wait();
            });
        None
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        self.end();
    }
}

/// One output of a probe command, read without blocking up to [`MAX_OUTPUT`].
struct Output {
    /// `None` once it ended, went past the limit, or was not asked for.
    file: Option<std::fs::File>,
    bytes: Vec<u8>,
    /// More than [`MAX_OUTPUT`] bytes came.
    over: bool,
}

impl Output {
    fn new(fd: Option<std::os::fd::OwnedFd>) -> io::Result<Output> {
        if let Some(fd) = &fd {
            rustix::io::ioctl_fionbio(fd, true)?;
        }
        Ok(Output {
            file: fd.map(std::fs::File::from),
            bytes: Vec::new(),
            over: false,
        })
    }

    /// Take what is there; at its end or past the limit it is closed.
    fn drain(&mut self) -> io::Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        let mut buf = [0u8; 1 << 16];
        loop {
            match file.read(&mut buf) {
                Ok(0) => {
                    self.file = None;
                    return Ok(());
                }
                Ok(n) => {
                    self.bytes.extend_from_slice(&buf[..n]);
                    if u64::try_from(self.bytes.len()).unwrap_or(u64::MAX) > MAX_OUTPUT {
                        self.over = true;
                        self.file = None;
                        return Ok(());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Set by a test: [`Held::add`] on this thread cannot copy the descriptor, as when no descriptor is left.
    static COPY_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set by a test: once this file exists, waits for output on this thread fail as a poll that cannot get memory
    /// would.
    static POLL_FAILS: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Wait at most `wait` for output on any open one.
fn wait_for_output(outputs: &[Output], wait: Duration) -> io::Result<()> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    #[cfg(test)]
    if POLL_FAILS.with(|f| f.borrow().as_ref().is_some_and(|p| p.exists())) {
        return Err(io::Error::from_raw_os_error(12));
    }
    let mut fds: Vec<PollFd<'_>> = outputs
        .iter()
        .filter_map(|o| o.file.as_ref())
        .map(|file| PollFd::new(file, PollFlags::IN))
        .collect();
    if fds.is_empty() {
        std::thread::sleep(wait);
        return Ok(());
    }
    let timeout = Timespec {
        tv_sec: i64::try_from(wait.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(wait.subsec_nanos()),
    };
    match poll(&mut fds, Some(&timeout)) {
        Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dir` held open, as the probe holds the scratch directory it made.
    fn held(dir: &Path) -> std::os::fd::OwnedFd {
        use rustix::fs::{Mode, OFlags};
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        rustix::fs::openat(rustix::fs::CWD, dir, flags, Mode::empty()).expect("open the directory")
    }

    /// [`schema_files`] of `root` as the probe made it.
    fn walk(root: &Path, work: &Work) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
        schema_files(root, held(root).as_fd(), work)
    }

    /// The directory holding `dir`.
    fn parent_of(dir: &Path) -> std::os::fd::OwnedFd {
        held(dir.parent().expect("a parent"))
    }

    /// What `fd` is.
    fn inode_of(fd: &std::os::fd::OwnedFd) -> Inode {
        inode(&rustix::fs::fstat(fd).expect("stat"))
    }

    /// [`remove_within`] of `dir` as the probe made it: true when it and its quarantine are gone.
    fn remove(dir: &Path, work: &Work) -> bool {
        remove_held(dir, &held(dir), work)
    }

    /// [`remove_within`] of the directory `owned`, made as `dir`, through a quarantine beside it, as
    /// [`Scratch::remove`] does it.
    fn remove_held(dir: &Path, owned: &std::os::fd::OwnedFd, work: &Work) -> bool {
        let temp = parent_of(dir);
        let quarantine =
            Quarantine::new(temp.as_fd(), dir, &Held::default()).expect("make a quarantine");
        remove_within(dir, temp.as_fd(), owned.as_fd(), &quarantine, work)
            & quarantine.remove(temp.as_fd())
    }

    /// Whether process `pid` has ended (gone, or a zombie not reaped yet).
    fn ended(pid: &str) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
    }

    #[test]
    fn a_stop_kills_the_commands_a_stalled_probe_started_and_starts_none()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T07R-300 (docs/adr/ADR-022.md decision 10 v4): the stop itself kills every command a probe started, its
        // group included, however late the thread that started it runs (here stalled just after the start); once
        // stopped, no command starts.
        let tmp = tempfile::tempdir()?;
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin)?;
        let (leader, member) = (tmp.path().join("leader"), tmp.path().join("member"));
        let launcher = bin.join("codex");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nsleep 60 &\necho $! > '{member}'\necho $$ > '{leader}'\nwait\n",
                member = member.display(),
                leader = leader.display()
            ),
        )?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        let stall = bin.join("hoh-test-stall-started");
        std::fs::write(&stall, "")?;
        let stop = ProbeStop::default();
        let env = ProbeEnv {
            path: format!("{}:/usr/bin:/bin", bin.display()).into(),
            fixture_root: None,
            timeout: Duration::from_secs(30),
            log: None,
            deadline: None,
            cancel: Some(stop.clone()),
        };
        let probing = {
            let (launcher, env) = (launcher.clone(), env.clone());
            std::thread::spawn(move || run(&launcher, &["--version"], &env).map(|_| ()))
        };
        let until = Instant::now() + Duration::from_secs(10);
        while !(leader.exists() && member.exists()) {
            assert!(Instant::now() < until, "the command never ran");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pids = [
            std::fs::read_to_string(&leader)?.trim().to_owned(),
            std::fs::read_to_string(&member)?.trim().to_owned(),
        ];
        stop.stop();
        // The probe's thread is still stalled: the stop alone ended both.
        let until = Instant::now() + Duration::from_secs(2);
        while !pids.iter().all(|p| ended(p)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let alive: Vec<&String> = pids.iter().filter(|p| !ended(p)).collect();
        for pid in &alive {
            let _ = Command::new("kill").args(["-KILL", pid.as_str()]).status();
        }
        std::fs::remove_file(&stall)?;
        let result = probing.join().map_err(|_| "the probe panicked")?;
        assert!(alive.is_empty(), "alive after the stop: {alive:?}");
        assert!(result.is_err(), "{result:?}");
        // Stopped, nothing starts.
        let ran = tmp.path().join("ran");
        std::fs::write(&launcher, format!("#!/bin/sh\ntouch '{}'\n", ran.display()))?;
        let refused = run(&launcher, &["--version"], &env);
        assert!(
            matches!(&refused, Err(ProbeError::Failed { detail, .. }) if detail == STOPPING),
            "{refused:?}"
        );
        assert!(!ran.exists(), "a command started after the stop");
        Ok(())
    }

    /// Whether this process holds `path` open (a stall in [`between`] holds its entry open while it waits).
    fn held_open(path: &Path) -> bool {
        std::fs::read_dir("/proc/self/fd").is_ok_and(|fds| {
            fds.flatten()
                .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|target| target == path))
        })
    }

    /// This process's children named `comm`, with their state letter.
    fn children_named(comm: &str) -> Vec<(u32, char)> {
        let me = std::process::id().to_string();
        std::fs::read_dir("/proc")
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
                    .filter_map(|pid| {
                        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                        let (head, rest) = stat.rsplit_once(')')?;
                        let mut fields = rest.split_whitespace();
                        let state = fields.next()?.chars().next()?;
                        let parent = fields.next()?;
                        (head.ends_with(&format!("({comm}")) && parent == me)
                            .then_some((pid, state))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Until `done` holds, at most `limit`.
    fn wait_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let until = Instant::now() + limit;
        while !done() {
            if Instant::now() > until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    #[test]
    fn a_stop_does_not_wait_for_a_start_in_progress_and_its_command_is_killed()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T07R-450, -500, -554 (docs/adr/ADR-022.md decision 10 v5): a start held inside its spawn (as an exec that
        // hangs holds it) does not hold up the stop; when its spawn returns after the stop, its starter, putting the
        // child in its slot, finds the stop and kills it there, before it goes on (here held once it started).
        stop_while_the_starter_is_held_at("spawning", "started")
    }

    #[test]
    fn a_command_put_in_its_slot_after_a_stop_is_dead_however_late_its_starter_runs_again()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T07R-600 (docs/adr/ADR-022.md decision 10 v5): the starter puts the child in its slot and looks at the stop
        // in one hold of the slot's lock, so once the child is in its slot it is dead though its starter is held right
        // there and never runs again (a starter that looked after letting the slot go left it alive while held).
        stop_while_the_starter_is_held_at("spawning", "placed")
    }

    #[test]
    fn a_command_whose_spawn_returned_before_a_stop_is_dead_once_put_in_its_slot()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T07R-651 (docs/adr/ADR-022.md decision 10 v5): the stop comes after the spawn returned and before the child
        // is in its slot, so it finds the slot empty; the starter looks at the stop only once the child is in its slot,
        // so it finds it and kills it there (a starter that looked before the put found no stop and left it alive).
        stop_while_the_starter_is_held_at("spawned", "placed")
    }

    /// A stop while a start is held at `stopped_at`; the start then goes on and its starter is held at `moment`, where
    /// its command must already be dead.
    fn stop_while_the_starter_is_held_at(
        stopped_at: &str,
        moment: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin)?;
        // Named for its moments (at most 15 bytes, the kernel's command name), so the tests do not see each other's.
        let name = format!("hs{stopped_at}{}", moment.get(..3).unwrap_or(moment));
        let launcher = bin.join(&name);
        std::fs::write(&launcher, "#!/bin/sh\nwhile :; do sleep 0.1; done\n")?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        let (stop_stall, hold_stall) = (
            bin.join(format!("hoh-test-stall-{stopped_at}")),
            bin.join(format!("hoh-test-stall-{moment}")),
        );
        std::fs::write(&stop_stall, "")?;
        std::fs::write(&hold_stall, "")?;
        let stop = ProbeStop::default();
        let env = ProbeEnv {
            path: "/usr/bin:/bin".into(),
            fixture_root: None,
            timeout: Duration::from_secs(30),
            log: None,
            deadline: None,
            cancel: Some(stop.clone()),
        };
        let probing = std::thread::spawn(move || run(&launcher, &["--version"], &env).map(|_| ()));
        let inside = wait_until(Duration::from_secs(10), || held_open(&stop_stall));
        let stopper = {
            let stop = stop.clone();
            std::thread::spawn(move || stop.stop())
        };
        let stopped_at_once = wait_until(Duration::from_secs(2), || stopper.is_finished());
        // The start goes on; its starter is then held at `moment`.
        std::fs::remove_file(&stop_stall)?;
        let past_its_slot = wait_until(Duration::from_secs(10), || held_open(&hold_stall));
        let found = children_named(&name);
        let killed = wait_until(Duration::from_secs(2), || {
            children_named(&name).iter().all(|(_, state)| *state == 'Z')
        });
        for (pid, _) in children_named(&name) {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        std::fs::remove_file(&hold_stall)?;
        let _ = stopper.join();
        let result = probing.join().map_err(|_| "the probe panicked")?;
        assert!(inside, "the probe never got to {stopped_at}");
        assert!(
            stopped_at_once,
            "the stop waited for the start held at {stopped_at}"
        );
        assert!(past_its_slot, "the starter never got to {moment}");
        assert_eq!(found.len(), 1, "the command started: {found:?}");
        assert!(
            killed,
            "the command started during the stop lives while its starter is held at {moment}"
        );
        assert!(
            matches!(&result, Err(ProbeError::Failed { detail, .. }) if detail == STOPPING),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn a_stopped_probe_does_not_even_try_to_start_a_command() {
        // M2T07R-400, -652 (docs/adr/ADR-022.md decision 10 v5): once stopped, the start does not run the spawn at all
        // (the check and the slot's registration are one step under the stop's lock; the spawn comes after, outside
        // it), whatever a command started and killed at once would have left behind.
        let stop = ProbeStop::default();
        let mut tried = false;
        let before = stop.start(Path::new("/"), || Err(io::Error::other("not started")));
        assert!(
            matches!(before, Some(Err(_))),
            "before the stop the spawn runs"
        );
        stop.stop();
        let after = stop.start(Path::new("/"), || {
            tried = true;
            Err(io::Error::other("not started"))
        });
        assert!(after.is_none());
        assert!(!tried, "a stopped probe ran its spawn");
    }

    #[test]
    fn a_stopped_probe_ends_at_once_though_an_escaped_child_holds_its_output()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T07R-202, -300 (docs/adr/ADR-022.md decision 10 v4): the stop kills the command's group, but a child that
        // left it (setsid) keeps the output open; the probe still ends at once, not at its time limit.
        let tmp = tempfile::tempdir()?;
        let (pid, escaped) = (tmp.path().join("pid"), tmp.path().join("escaped"));
        let launcher = tmp.path().join("codex");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nsetsid sh -c 'echo $$ > \"$1\"; exec sleep 60' sh '{escaped}' &\n\
                 while [ ! -s '{escaped}' ]; do sleep 0.01; done\necho $$ > '{pid}'\nexec sleep 60\n",
                escaped = escaped.display(),
                pid = pid.display()
            ),
        )?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        let stop = ProbeStop::default();
        let env = ProbeEnv {
            path: "/usr/bin:/bin".into(),
            fixture_root: None,
            timeout: Duration::from_secs(30),
            log: None,
            deadline: None,
            cancel: Some(stop.clone()),
        };
        let probing = std::thread::spawn(move || run(&launcher, &["--version"], &env).map(|_| ()));
        let until = Instant::now() + Duration::from_secs(10);
        while !pid.exists() {
            assert!(Instant::now() < until, "the command never ran");
            std::thread::sleep(Duration::from_millis(10));
        }
        let stopping = Instant::now();
        stop.stop();
        let result = probing.join().map_err(|_| "the probe panicked")?;
        let took = stopping.elapsed();
        let child = std::fs::read_to_string(&escaped)?.trim().to_owned();
        let _ = Command::new("kill")
            .args(["-KILL", child.as_str()])
            .status();
        assert!(
            took < Duration::from_secs(2),
            "the stopped probe took {took:?}"
        );
        assert!(
            matches!(&result, Err(ProbeError::Failed { detail, .. }) if detail == STOPPING),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn a_stopped_schema_reading_ends_with_its_output_removed()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T07R-300 (docs/adr/ADR-022.md decision 10 v4): stopped while the schema is generated, the reader is given
        // its cleanup grace: the probe returns its reading's own end (the stop), its output removed, not a reading
        // given up with its directory left behind.
        let tmp = tempfile::tempdir()?;
        let (pid, out) = (tmp.path().join("pid"), tmp.path().join("out"));
        let launcher = tmp.path().join("codex");
        // Its output holds an entry that stalls the removal until the test removes it: the reading ends a while
        // after the stop, within the cleanup grace.
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\n[ \"$2\" = generate-json-schema ] || exit 2\necho \"$4\" > '{out}'\n\
                 touch \"$4/hoh-test-stall-remove\"\necho $$ > '{pid}'\nexec sleep 30\n",
                out = out.display(),
                pid = pid.display()
            ),
        )?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        let stop = ProbeStop::default();
        let env = ProbeEnv {
            path: "/usr/bin:/bin".into(),
            fixture_root: None,
            timeout: Duration::from_secs(30),
            log: None,
            deadline: None,
            cancel: Some(stop.clone()),
        };
        let reading = std::thread::spawn(move || codex_schema(&launcher, &env).map(|_| ()));
        let until = Instant::now() + Duration::from_secs(10);
        while !pid.exists() {
            assert!(Instant::now() < until, "the schema was never generated");
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = PathBuf::from(std::fs::read_to_string(&out)?.trim_end());
        let stopping = Instant::now();
        stop.stop();
        std::thread::sleep(Duration::from_millis(300));
        std::fs::remove_file(output.join("hoh-test-stall-remove"))?;
        let result = reading.join().map_err(|_| "the reading panicked")?;
        assert!(
            stopping.elapsed() < CLEANUP_GRACE + READER_MARGIN,
            "{:?}",
            stopping.elapsed()
        );
        assert!(
            !output.exists(),
            "its output is left at {}",
            output.display()
        );
        assert!(
            matches!(&result, Err(ProbeError::Failed { detail, .. })
                if detail.contains(STOPPING) && !detail.contains("may be left") && !detail.contains("time limit")),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn pi_release_follows_the_launcher_layout() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("hoh-pi-layout-{}", std::process::id()));
        let bin = root.join("install/releases/1.5.0/node_modules/.bin");
        std::fs::create_dir_all(&bin)?;
        std::fs::write(bin.join("pi"), "#!/bin/sh\n")?;
        std::fs::write(root.join("install/current-version"), "1.5.0\n")?;
        assert_eq!(pi_release(&root), Some(bin.join("pi")));
        std::fs::write(root.join("install/current-version"), "../../etc\n")?;
        assert_eq!(pi_release(&root), None);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    fn codex_layout(
        root: &Path,
        platform_dir: Option<&Path>,
    ) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
        // <root>/lib/node_modules/@openai/codex/bin/codex.js, linked from <root>/bin/codex like an npm global install.
        let (triple, _) = codex_target().ok_or("unsupported test platform")?;
        let package = root.join("lib/node_modules/@openai/codex");
        std::fs::create_dir_all(package.join("bin"))?;
        std::fs::write(package.join("bin/codex.js"), "#!/usr/bin/env node\n")?;
        std::fs::write(
            package.join("package.json"),
            r#"{"name": "@openai/codex", "bin": {"codex": "bin/codex.js"}}"#,
        )?;
        std::fs::create_dir_all(root.join("bin"))?;
        let launcher = root.join("bin/codex");
        std::os::unix::fs::symlink(package.join("bin/codex.js"), &launcher)?;
        let vendor_root = match platform_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                std::fs::write(dir.join("package.json"), "{}")?;
                dir.join("vendor")
            }
            None => package.join("vendor"),
        };
        let native = vendor_root.join(triple).join("bin/codex");
        std::fs::create_dir_all(native.parent().ok_or("parent")?)?;
        std::fs::write(&native, b"\x7fELF native")?;
        Ok((launcher, native))
    }

    #[test]
    fn codex_implementation_follows_the_launcher_resolution()
    -> Result<(), Box<dyn std::error::Error>> {
        // M1T01R-02: codex.js resolves @openai/codex-<platform> through Node's node_modules lookup (nested or
        // hoisted next to the codex package) and falls back to the package's own vendor directory.
        let (_, platform) = codex_target().ok_or("unsupported test platform")?;
        for layout in ["nested", "sibling", "vendor"] {
            let root =
                std::env::temp_dir().join(format!("hoh-codex-{layout}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let modules = root.join("lib/node_modules");
            let platform_dir = match layout {
                "nested" => Some(modules.join("@openai/codex/node_modules").join(platform)),
                "sibling" => Some(modules.join(platform)),
                _ => None,
            };
            let (launcher, native) = codex_layout(&root, platform_dir.as_deref())?;
            let resolved = resolve_implementation(Agent::Codex, &launcher)?;
            assert_eq!(resolved, std::fs::canonicalize(&native)?, "{layout}");
            let before = file_sha256(&resolved)?;
            std::fs::write(&native, b"\x7fELF replaced")?;
            assert_ne!(
                file_sha256(&resolve_implementation(Agent::Codex, &launcher)?)?,
                before,
                "{layout}"
            );
            std::fs::remove_dir_all(&root)?;
        }
        Ok(())
    }

    #[test]
    fn codex_resolution_honours_exports_and_needs_the_npm_wrapper()
    -> Result<(), Box<dyn std::error::Error>> {
        // M1T01R-13: require.resolve fails when the platform package does not export package.json, and codex.js
        // then uses its own vendor directory; a directly installed binary is its own implementation.
        let (triple, platform) = codex_target().ok_or("unsupported test platform")?;
        let root = std::env::temp_dir().join(format!("hoh-codex-exports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let platform_dir = root.join("lib/node_modules").join(platform);
        let (launcher, platform_native) = codex_layout(&root, Some(&platform_dir))?;
        std::fs::write(
            platform_dir.join("package.json"),
            r#"{"exports": {".": "./index.js"}}"#,
        )?;
        let own = root
            .join("lib/node_modules/@openai/codex/vendor")
            .join(triple)
            .join("bin/codex");
        std::fs::create_dir_all(own.parent().ok_or("parent")?)?;
        std::fs::write(&own, b"\x7fELF own vendor")?;
        assert_eq!(
            resolve_implementation(Agent::Codex, &launcher)?,
            std::fs::canonicalize(&own)?
        );
        std::fs::write(
            platform_dir.join("package.json"),
            r#"{"exports": {".": "./index.js", "./package.json": "./package.json"}}"#,
        )?;
        assert_eq!(
            resolve_implementation(Agent::Codex, &launcher)?,
            std::fs::canonicalize(&platform_native)?
        );
        // A native codex binary next to an unrelated node_modules tree stays its own implementation.
        let direct = root.join("lib/node_modules/@openai/codex/bin/codex");
        std::fs::write(&direct, b"\x7fELF direct")?;
        assert_eq!(
            resolve_implementation(Agent::Codex, &direct)?,
            std::fs::canonicalize(&direct)?
        );
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn unsupported_exports_forms_are_refused_not_guessed() -> Result<(), Box<dyn std::error::Error>>
    {
        // M1T01R-17: conditions, arrays, patterns, redirects and null overrides need Node's full resolver; the probe
        // refuses them instead of recording a binary the launcher might not run.
        let (_, platform) = codex_target().ok_or("unsupported test platform")?;
        let root = std::env::temp_dir().join(format!("hoh-codex-exports2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let platform_dir = root.join("lib/node_modules").join(platform);
        let (launcher, _) = codex_layout(&root, Some(&platform_dir))?;
        for exports in [
            r#"{"./package.json": {"import": "./package.json"}}"#,
            r#"{"./package.json": []}"#,
            r#"{"./package.json": "./sub/package.json"}"#,
            r#"{"./*": "./*", "./package.json": null}"#,
            r#"{"./*": "./*"}"#,
            r#"{"import": "./index.js"}"#,
        ] {
            std::fs::write(
                platform_dir.join("package.json"),
                format!("{{\"exports\": {exports}}}"),
            )?;
            assert!(
                resolve_implementation(Agent::Codex, &launcher).is_err(),
                "{exports}"
            );
        }
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn tree_hash_is_independent_of_traversal_order() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("hoh-tree-{}", std::process::id()));
        std::fs::create_dir_all(root.join("b/c"))?;
        std::fs::write(root.join("a"), "1")?;
        std::fs::write(root.join("b/c/d"), "2")?;
        let expected = hex(&Sha256::digest(
            format!(
                "a {}\nb/c/d {}\n",
                hex(&Sha256::digest(b"1")),
                hex(&Sha256::digest(b"2"))
            )
            .as_bytes(),
        ));
        assert_eq!(tree_sha256(&root)?, expected);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    /// The validator of the probe's keyword set: every keyword constrains as draft-07 says, `$ref` ignores its
    /// siblings, numbers compare by value, and what it cannot decide stays undecided (M1T07R-33..-35, -38).
    #[test]
    fn the_validator_applies_each_keyword() {
        use serde_json::json;
        let defs = json!({"Id": {"anyOf": [{"type": "string"}, {"type": "integer", "minimum": 0}]},
                          "Loop": {"$ref": "#/definitions/Loop"}});
        let defs = defs.as_object().cloned().unwrap_or_default();
        let truth = |schema: serde_json::Value, instance: serde_json::Value| {
            valid(
                &schema,
                &instance,
                &defs,
                0,
                &Work::new(VALIDATION_BUDGET, None),
            )
        };
        let ok = |schema: serde_json::Value, instance: serde_json::Value| {
            truth(schema, instance) == Truth::Yes
        };
        assert!(ok(json!(true), json!(1)) && !ok(json!(false), json!(1)));
        assert!(
            ok(json!({"type": "integer"}), json!(3)) && ok(json!({"type": "integer"}), json!(3.0))
        );
        assert!(
            !ok(json!({"type": "integer"}), json!(3.5))
                && !ok(json!({"type": "integer"}), json!("3"))
        );
        assert!(
            ok(json!({"type": ["null", "string"]}), json!(null))
                && !ok(json!({"type": ["string"]}), json!(1))
        );
        assert!(ok(json!({"enum": ["a", 1]}), json!(1)) && !ok(json!({"enum": ["a"]}), json!("b")));
        assert!(
            ok(json!({"required": ["a"]}), json!({"a": 1}))
                && !ok(json!({"required": ["a"]}), json!({"b": 1}))
        );
        assert!(ok(json!({"required": ["a"]}), json!("not an object")));
        assert!(!ok(json!({"properties": {"a": false}}), json!({"a": 1})));
        assert!(ok(json!({"properties": {"a": false}}), json!({"b": 1})));
        assert!(!ok(
            json!({"properties": {"a": {}}, "additionalProperties": false}),
            json!({"a": 1, "b": 2})
        ));
        assert!(ok(
            json!({"properties": {"a": {}}, "additionalProperties": false}),
            json!({"a": 1})
        ));
        assert!(
            !ok(json!({"items": {"type": "string"}}), json!(["a", 1]))
                && ok(json!({"items": false}), json!([]))
        );
        assert!(!ok(
            json!({"allOf": [{"type": "integer"}, {"minimum": 5}]}),
            json!(4)
        ));
        assert!(ok(
            json!({"anyOf": [{"type": "string"}, {"minimum": 5}]}),
            json!(6)
        ));
        assert!(!ok(
            json!({"anyOf": [{"type": "string"}, {"minimum": 5}]}),
            json!(4)
        ));
        assert!(!ok(
            json!({"oneOf": [{"type": "integer"}, {"type": "number"}]}),
            json!(1)
        ));
        assert!(ok(
            json!({"oneOf": [{"type": "integer"}, {"type": "string"}]}),
            json!(1)
        ));
        assert!(
            ok(json!({"$ref": "#/definitions/Id"}), json!(0))
                && !ok(json!({"$ref": "#/definitions/Id"}), json!(-1))
        );
        assert!(
            !ok(json!({"$ref": "#/definitions/None"}), json!(0)),
            "an unknown definition"
        );
        assert!(
            !ok(json!({"minLength": 2}), json!("é")) && ok(json!({"minLength": 1}), json!("é"))
        );
        assert!(ok(
            json!({"format": "int64", "default": 0, "title": "t", "description": "d"}),
            json!("x")
        ));
        assert!(
            !ok(json!({"not": {}}), json!(1)),
            "an unknown keyword is not valid"
        );
        // `$ref` ignores its siblings, so both branches take 0 and oneOf refuses it (M1T07R-34).
        assert!(ok(
            json!({"$ref": "#/definitions/Id", "minimum": 1}),
            json!(0)
        ));
        assert_eq!(
            truth(
                json!({"oneOf": [{"$ref": "#/definitions/Id", "minimum": 1}, {"$ref": "#/definitions/Id"}]}),
                json!(0)
            ),
            Truth::No
        );
        // A cycle is undecided, and so is a oneOf that hangs on it (M1T07R-34).
        assert_eq!(
            truth(json!({"$ref": "#/definitions/Loop"}), json!(0)),
            Truth::Unknown
        );
        assert_eq!(
            truth(
                json!({"oneOf": [{"$ref": "#/definitions/Loop"}, {"type": "integer"}]}),
                json!(0)
            ),
            Truth::Unknown
        );
        assert_eq!(
            truth(
                json!({"anyOf": [{"$ref": "#/definitions/Loop"}, {"type": "integer"}]}),
                json!(0)
            ),
            Truth::Yes
        );
        assert_eq!(
            truth(
                json!({"allOf": [{"$ref": "#/definitions/Loop"}, {"type": "string"}]}),
                json!(0)
            ),
            Truth::No
        );
        // Numbers by value (M1T07R-35, -38).
        assert!(
            ok(json!({"enum": [0.0]}), json!(0))
                && ok(json!({"enum": [[1.0, {"a": 2}]]}), json!([1, {"a": 2.0}]))
        );
        assert!(!ok(
            json!({"oneOf": [{"enum": [0.0]}, {"enum": [0]}]}),
            json!(0)
        ));
        assert!(!ok(
            json!({"minimum": 9007199254740993u64}),
            json!(9007199254740992u64)
        ));
        assert!(ok(
            json!({"minimum": 9007199254740993u64}),
            json!(9007199254740993u64)
        ));
        assert!(!ok(json!({"minimum": 0.5}), json!(0)) && ok(json!({"minimum": 0.5}), json!(1)));
        assert!(ok(json!({"minimum": -1}), json!(-1)) && !ok(json!({"minimum": -0.5}), json!(-1)));
    }

    /// An id schema takes every id the adapter numbers only where that can be shown (M1T07R-36).
    #[test]
    fn every_request_id_is_taken_only_where_that_can_be_shown() {
        use serde_json::json;
        let defs = json!({"RequestId": {"anyOf": [{"type": "string"}, {"format": "int64", "type": "integer"}]},
                          "Loop": {"$ref": "#/definitions/Loop"}});
        let defs = defs.as_object().cloned().unwrap_or_default();
        let takes = |schema: serde_json::Value| {
            takes_every_id(&schema, &defs, 0, &Work::new(VALIDATION_BUDGET, None))
        };
        for yes in [
            json!(true),
            json!({"$ref": "#/definitions/RequestId"}),
            json!({"type": "integer"}),
            json!({"type": ["string", "number"], "minimum": 0}),
            json!({"allOf": [{"type": "integer"}, {"minimum": -3}]}),
            json!({"$ref": "#/definitions/RequestId", "minimum": 5}),
        ] {
            assert!(takes(yes.clone()), "{yes}");
        }
        for no in [
            json!(false),
            json!({"type": "string"}),
            json!({"type": "integer", "minimum": 1}),
            json!({"enum": [0, 1, 2, 3]}),
            json!({"oneOf": [{"type": "integer"}, {"type": "string"}]}),
            json!({"anyOf": [{"type": "string"}, {"enum": [0]}]}),
            json!({"$ref": "#/definitions/Loop"}),
            json!({"$ref": "#/definitions/None"}),
        ] {
            assert!(!takes(no.clone()), "{no}");
        }
    }

    /// A number reads back as written or the file is refused; strings are skipped, escaped quotes included (M1T07R-39).
    #[test]
    fn numbers_are_read_exactly_or_refused() {
        for exact in [
            "0",
            "-0",
            "1",
            "-1",
            "9007199254740993",
            "18446744073709551615",
            "0.7",
            "1e3",
            "1E+3",
            "-2.50",
            "1e-300",
        ] {
            assert!(reads_exactly(exact), "{exact}");
        }
        for inexact in [
            "1e-400",
            "-1e-400",
            "0.99999999999999999",
            "18446744073709551616",
            "9007199254740993.5",
            "1e400",
            "9007199254740993.0",
        ] {
            assert!(!reads_exactly(inexact), "{inexact}");
        }
        assert_eq!(
            inexact_number(
                r#"{"a": "1e-400", "b": "x\"1e-400", "c": [0, 0.7, -3]}"#,
                &Work::new(u64::MAX, None)
            ),
            Ok(None)
        );
        assert_eq!(
            inexact_number(
                r#"{"a": "\\", "minimum": 1e-400}"#,
                &Work::new(u64::MAX, None)
            ),
            Ok(Some("1e-400".to_owned()))
        );
    }

    /// The recorded 0.166.0 schema is read well within the validation budget, so the budget only stops schemas that
    /// branch without end (M1T07R-40).
    #[test]
    fn the_recorded_schema_needs_little_of_the_budget() -> Result<(), Box<dyn std::error::Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/protocol/codex/0.166.0/app-server-schema");
        let work = Work::new(VALIDATION_BUDGET, None);
        let mut kept = 0;
        for (name, sent) in [
            ("ClientRequest.json", true),
            ("ClientNotification.json", true),
            ("ServerNotification.json", false),
        ] {
            let text = std::fs::read_to_string(root.join(name))?;
            assert_eq!(
                inexact_number(&text, &Work::new(u64::MAX, None)),
                Ok(None),
                "{name}"
            );
            let value: serde_json::Value = serde_json::from_str(&text)?;
            let mut open = value.clone();
            params_open(&mut open);
            let defs = open["definitions"].as_object().cloned().unwrap_or_default();
            for method in schema_methods(&value, sent, &Work::new(u64::MAX, None))
                .map_err(|()| "out of time")?
                .ok_or("not the known form")?
            {
                let envelope = match name {
                    "ClientRequest.json" => {
                        serde_json::json!({"id": 0, "method": method, "params": {}})
                    }
                    "ClientNotification.json" => serde_json::json!({"method": method}),
                    _ => serde_json::json!({"method": method, "params": {}}),
                };
                assert_eq!(
                    valid(&open, &envelope, &defs, 0, &work),
                    Truth::Yes,
                    "{name} {method}"
                );
                if sent && name == "ClientRequest.json" {
                    let branches = branches_by_method(&open, &Work::new(u64::MAX, None));
                    let id = &branches.get(method.as_str()).ok_or("no branch")?["properties"]["id"];
                    assert!(takes_every_id(id, &defs, 0, &work), "{method}");
                }
                kept += 1;
            }
        }
        assert_eq!(kept, 192);
        let used = VALIDATION_BUDGET - work.left();
        assert!(
            used < VALIDATION_BUDGET / 10,
            "the recorded schema used {used} steps"
        );
        Ok(())
    }

    /// Work stops at doctor's shared deadline as well as at its step budget (M1T07R-40).
    #[test]
    fn work_ends_at_the_deadline() {
        let past = Instant::now() - Duration::from_secs(1);
        let work = Work::new(VALIDATION_BUDGET, Some(past));
        let steps = (0..10_000).take_while(|_| work.spend(1)).count();
        assert!(steps < 4096, "{steps} steps after the deadline");
        assert_eq!(work.left(), 0);
        let work = Work::new(10, None);
        assert!(work.spend(10) && !work.spend(1), "the budget ends");
    }

    /// The checks before validation stop at doctor's deadline too, and their paths are written out only on error, so
    /// a long key under many members costs no more than the file's size (M1T07R-40).
    #[test]
    fn the_checks_before_validation_end_at_the_deadline() {
        let past = Some(Instant::now() - Duration::from_secs(1));
        let members: Vec<serde_json::Value> =
            (0..10_000).map(|_| serde_json::json!(true)).collect();
        let wide = serde_json::json!({"definitions": {"D": {"anyOf": members}}, "oneOf": [true]});
        RENDERED.with(|n| n.set(0));
        assert!(draft7_well_formed(&wide, &Work::new(u64::MAX, None)).is_ok());
        assert_eq!(
            RENDERED.with(std::cell::Cell::get),
            0,
            "a path was written out without an error"
        );
        assert!(
            draft7_well_formed(&wide, &Work::new(u64::MAX, past))
                .is_err_and(|e| e.contains("time limit"))
        );
        let text = "[".to_owned() + &"0, ".repeat(10_000) + "0]";
        assert_eq!(inexact_number(&text, &Work::new(u64::MAX, None)), Ok(None));
        assert_eq!(inexact_number(&text, &Work::new(u64::MAX, past)), Err(()));
        // A text without numbers costs its length too.
        let words = "[\"".to_owned() + &"a".repeat(100_000) + "\"]";
        assert_eq!(inexact_number(&words, &Work::new(u64::MAX, None)), Ok(None));
        assert_eq!(inexact_number(&words, &Work::new(u64::MAX, past)), Err(()));
        // One long number costs its length, so it reaches the deadline too (M1T07R-42).
        let long = "[1".to_owned() + &"0".repeat(100_000) + "]";
        assert_eq!(inexact_number(&long, &Work::new(u64::MAX, past)), Err(()));
        // Without a shared deadline the probe's own command limit bounds the reading; with one, the earlier wins.
        let now = Instant::now();
        let env = |deadline| ProbeEnv {
            path: OsString::new(),
            fixture_root: None,
            timeout: Duration::from_secs(20),
            log: None,
            deadline,
            cancel: None,
        };
        assert_eq!(
            reading_deadline(&env(None), now),
            now + Duration::from_secs(20)
        );
        assert_eq!(
            reading_deadline(&env(Some(now + Duration::from_secs(5))), now),
            now + Duration::from_secs(5)
        );
        assert_eq!(
            reading_deadline(&env(Some(now + Duration::from_secs(50))), now),
            now + Duration::from_secs(20)
        );
        // An error names its position.
        let bad = serde_json::json!({"definitions": {"D": {"anyOf": [true, {"not": {}}]}}});
        assert!(
            draft7_well_formed(&bad, &Work::new(u64::MAX, None))
                .is_err_and(|e| e.starts_with("/definitions/D/anyOf/1/not"))
        );
    }

    /// Every array a check walks costs its length: a large `required` array, a long type list, a method enum or many
    /// branches reach the deadline too (M1T07R-40, round 13).
    #[test]
    fn array_walks_before_validation_reach_the_deadline() {
        let past = Some(Instant::now() - Duration::from_secs(1));
        let names: Vec<String> = (0..100_000).map(|n| format!("n{n}")).collect();
        let required =
            serde_json::json!({"definitions": {"D": {"required": names}}, "oneOf": [true]});
        assert!(draft7_well_formed(&required, &Work::new(u64::MAX, None)).is_ok());
        assert!(
            draft7_well_formed(&required, &Work::new(u64::MAX, past))
                .is_err_and(|e| e.contains("time limit"))
        );
        let types = serde_json::json!({"type": vec!["string"; 100_000]});
        assert!(
            draft7_well_formed(&types, &Work::new(u64::MAX, past))
                .is_err_and(|e| e.contains("time limit"))
        );
        let methods: Vec<String> = (0..100_000).map(|n| format!("m{n}")).collect();
        let file = serde_json::json!({"oneOf": [{"type": "object", "properties": {"method": {"type": "string", "enum": methods}}}]});
        assert!(
            schema_methods(&file, false, &Work::new(u64::MAX, None))
                .is_ok_and(|m| m.is_some_and(|m| m.len() == 100_000))
        );
        assert_eq!(
            schema_methods(&file, false, &Work::new(u64::MAX, past)),
            Err(())
        );
    }

    /// The walk of a schema output counts every entry and reads the deadline (M1T07R-40, round 14).
    #[test]
    fn the_schema_walk_counts_entries_and_reads_the_deadline()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        // Empty, so only the entries cost anything.
        for n in 0..MAX_SCHEMA_FILES {
            std::fs::write(root.path().join(format!("{n}.json")), "")?;
        }
        let past = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(
            walk(root.path(), &Work::new(u64::MAX, None))?.len(),
            MAX_SCHEMA_FILES
        );
        assert!(
            walk(root.path(), &Work::new(u64::MAX, past)).is_err_and(|e| e.contains("time limit"))
        );
        std::fs::create_dir(root.path().join("one-more"))?;
        assert!(
            walk(root.path(), &Work::new(u64::MAX, None)).is_err_and(|e| e.contains("too many"))
        );
        // A file's bytes count before it is read, so one large file reads the deadline too.
        let large = tempfile::tempdir()?;
        std::fs::write(large.path().join("a.json"), vec![b' '; 8192])?;
        assert!(walk(large.path(), &Work::new(u64::MAX, None)).is_ok());
        assert!(
            walk(large.path(), &Work::new(u64::MAX, past)).is_err_and(|e| e.contains("time limit"))
        );
        // The hash reads the deadline between files too.
        let files = walk(large.path(), &Work::new(u64::MAX, None))?;
        assert!(tree_sha256_of(large.path(), &files, &Work::new(u64::MAX, None)).is_ok());
        assert!(tree_sha256_of(large.path(), &files, &Work::new(u64::MAX, past)).is_err());
        Ok(())
    }

    /// Only the directory the probe made is emptied and removed: put back as a link to another, the link is removed
    /// and the other keeps every file; moved away, with or without another directory put in its place, it is emptied
    /// where it is, is not gone, and the other is left whole (M1T07R-44).
    #[test]
    fn only_the_directory_the_probe_made_is_removed() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for case in ["linked", "moved", "moved-away"] {
            let dir = root.path().join(case);
            let other = root.path().join(format!("{case}-other"));
            std::fs::create_dir_all(dir.join("v2"))?;
            std::fs::write(dir.join("v2/a.json"), "{}")?;
            std::fs::create_dir(&other)?;
            std::fs::write(other.join("keep.json"), "{}")?;
            let owned = held(&dir);
            let moved = root.path().join(format!("{case}-moved"));
            if case == "linked" {
                std::fs::remove_dir_all(&dir)?;
                std::os::unix::fs::symlink(&other, &dir)?;
            } else {
                std::fs::rename(&dir, &moved)?;
                if case == "moved" {
                    std::fs::rename(&other, &dir)?;
                }
            }
            let parent = parent_of(&dir);
            let name = dir.file_name().ok_or("a name")?;
            assert!(
                !names(parent.as_fd(), Path::new(name), inode_of(&owned)),
                "{case}"
            );
            let gone = remove_held(&dir, &owned, &Work::new(u64::MAX, None));
            if case == "linked" {
                assert!(gone, "{case}");
                assert!(
                    std::fs::symlink_metadata(&dir).is_err(),
                    "{case}: the link is left"
                );
                assert!(other.join("keep.json").exists(), "{case}");
            } else {
                assert!(!gone, "{case}");
                let kept = if case == "moved" { &dir } else { &other };
                assert!(kept.join("keep.json").exists(), "{case}");
                assert_eq!(std::fs::read_dir(&moved)?.count(), 0, "{case}: not emptied");
            }
        }
        Ok(())
    }

    /// A command's output is kept up to [`MAX_OUTPUT`]; more is a failure, read no further (M1T07R-44).
    #[test]
    fn a_command_with_more_output_than_the_limit_fails() -> Result<(), Box<dyn std::error::Error>> {
        let env = ProbeEnv {
            path: std::env::var_os("PATH").unwrap_or_default(),
            fixture_root: None,
            timeout: Duration::from_secs(30),
            log: None,
            deadline: None,
            cancel: None,
        };
        let sh = Path::new("/bin/sh");
        let within = format!("head -c {MAX_OUTPUT} /dev/zero");
        let beyond = format!("head -c {} /dev/zero", MAX_OUTPUT + 1);
        assert_eq!(
            usize::try_from(MAX_OUTPUT)?,
            run_ok(sh, &["-c", &within], &env)?.stdout.len()
        );
        for keep_stderr in [false, true] {
            let script = if keep_stderr {
                format!("{beyond} >&2")
            } else {
                beyond.clone()
            };
            match logged(sh, &["-c", &script], &env, keep_stderr) {
                Err(ProbeError::Failed { detail, .. }) => {
                    assert!(detail.contains("more output"), "{detail}")
                }
                other => return Err(format!("{keep_stderr}: {other:?}").into()),
            }
        }
        Ok(())
    }

    /// Reading a file stops one byte past what the limits leave, however much more it holds (M1T07R-44).
    #[test]
    fn reading_stops_past_its_room() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(read_bounded(&b"{}"[..], 2)?, Some(b"{}".to_vec()));
        assert_eq!(read_bounded(&b"{} "[..], 2)?, None);
        let done = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done
                .0
                .send(read_bounded(std::io::repeat(b' '), 1 << 20).map(|b| b.is_none()));
        });
        assert!(done.1.recv_timeout(Duration::from_secs(10))??);
        Ok(())
    }

    /// A scratch directory is removed even when the reading panics (M1T07R-40).
    #[test]
    fn scratch_is_removed_when_the_reading_panics() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("scratch");
        let panicked = std::panic::catch_unwind(|| {
            let scratch = Scratch::at(
                dir.clone(),
                Instant::now() + Duration::from_secs(60),
                Default::default(),
                Held::default(),
            )
            .expect("make the scratch directory");
            std::fs::create_dir_all(dir.join("v2")).expect("write the output");
            std::fs::write(dir.join("v2/a.json"), "{}").expect("write the output");
            let _scratch = scratch;
            panic!("a reading that fails");
        });
        assert!(panicked.is_err());
        assert!(!dir.exists(), "the scratch directory was left");
        assert_eq!(
            std::fs::read_dir(root.path())?.count(),
            0,
            "its quarantine was left"
        );
        Ok(())
    }

    /// What a helper the CLI left running does to the output while it is walked or removed never takes the probe
    /// outside it: a directory, or the output directory itself, replaced by a link to another is neither read nor
    /// emptied (M1T07R-44).
    #[test]
    fn an_entry_replaced_by_a_link_is_not_followed() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("kept.json"), "{}")?;
        for moment in ["entry", "walk", "remove"] {
            let dir = root.path().join(moment);
            std::fs::create_dir_all(dir.join("v2"))?;
            std::fs::write(dir.join("v2/a.json"), "{}")?;
            let swapped = if moment == "walk" {
                dir.clone()
            } else {
                dir.join("v2")
            };
            let (to, owned) = (outside.clone(), held(&dir));
            let swaps = std::rc::Rc::new(std::cell::Cell::new(0));
            let counted = swaps.clone();
            BETWEEN.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                    if at == moment && path == swapped {
                        std::fs::remove_dir_all(path).expect("remove the directory");
                        std::os::unix::fs::symlink(&to, path).expect("link it elsewhere");
                        counted.set(counted.get() + 1);
                    }
                }));
            });
            if moment == "remove" {
                assert!(remove_held(&dir, &owned, &Work::new(u64::MAX, None)));
            } else {
                let listed = schema_files(&dir, owned.as_fd(), &Work::new(u64::MAX, None))?;
                assert!(listed.is_empty(), "{moment}: {listed:?}");
            }
            BETWEEN.with(|hook| *hook.borrow_mut() = None);
            // The entry was replaced where the test means it to be, once (M1T07R-48).
            assert_eq!(swaps.get(), 1, "{moment}: replaced");
            assert!(
                outside.join("kept.json").exists(),
                "{moment}: removed outside"
            );
        }
        Ok(())
    }

    /// A directory a helper moves out of the output after the probe listed or opened it takes nothing of the
    /// reading or the removal with it: a file put into it outside is neither read nor removed, whether the move
    /// comes before an entry is opened or between its parent's opening and its removal (M1T07R-51).
    #[test]
    fn a_directory_moved_out_takes_nothing_with_it() -> Result<(), Box<dyn std::error::Error>> {
        for moment in ["entry", "remove", "take"] {
            let root = tempfile::tempdir()?;
            let (out, outside) = (root.path().join("out"), root.path().join("outside"));
            std::fs::create_dir_all(out.join("v2"))?;
            std::fs::create_dir(&outside)?;
            std::fs::write(out.join("v2/a.json"), "{}")?;
            std::fs::write(out.join("b.json"), "{}")?;
            std::fs::write(outside.join("secret.json"), "outside")?;
            let (target, child) = (out.join("v2/a.json"), out.join("v2"));
            let (escaped, secret) = (outside.join("escaped"), outside.join("secret.json"));
            let moves = std::rc::Rc::new(std::cell::Cell::new(0));
            let counted = moves.clone();
            let (to, from) = (escaped.clone(), secret.clone());
            BETWEEN.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                    if at == moment && path == target && counted.get() == 0 {
                        std::fs::rename(&child, &to).expect("move the directory out");
                        std::fs::rename(&from, to.join("a.json"))
                            .expect("put a file outside into it");
                        counted.set(counted.get() + 1);
                    }
                }));
            });
            let (read, gone) = if moment == "entry" {
                (
                    walk(&out, &Work::new(u64::MAX, None))?,
                    remove(&out, &Work::new(u64::MAX, None)),
                )
            } else {
                (Vec::new(), remove(&out, &Work::new(u64::MAX, None)))
            };
            BETWEEN.with(|hook| *hook.borrow_mut() = None);
            assert_eq!(moves.get(), 1, "{moment}: moved");
            assert!(
                read.iter().all(|(_, bytes)| bytes != b"outside"),
                "{moment}: read the file put outside"
            );
            assert_eq!(
                std::fs::read_to_string(escaped.join("a.json"))?,
                "outside",
                "{moment}: the file put outside is not where it was put"
            );
            // What stayed under the output went; what left it is the helper's.
            assert!(gone, "{moment}");
            assert!(!out.exists(), "{moment}");
            assert_eq!(
                std::fs::read_dir(root.path())?.count(),
                1,
                "{moment}: a quarantine was left"
            );
        }
        Ok(())
    }

    /// A directory put in place of the scratch directory between its making and its opening is not taken for it, and
    /// is left whole (M1T07R-51).
    #[test]
    fn a_scratch_directory_replaced_as_it_is_made_is_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let (dir, other, away) = (
            root.path().join("out"),
            root.path().join("other"),
            root.path().join("away"),
        );
        std::fs::create_dir(&other)?;
        std::fs::write(other.join("keep.json"), "{}")?;
        let swaps = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = swaps.clone();
        let (made, from, to) = (dir.clone(), other.clone(), away.clone());
        BETWEEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                if at == "made" && path == made {
                    std::fs::rename(&made, &to).expect("move the new directory away");
                    std::fs::rename(&from, &made).expect("put another in its place");
                    counted.set(counted.get() + 1);
                }
            }));
        });
        let made = Scratch::at(
            dir.clone(),
            Instant::now() + Duration::from_secs(60),
            Default::default(),
            Held::default(),
        );
        BETWEEN.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(swaps.get(), 1);
        assert!(
            made.as_ref()
                .is_err_and(|e| e.to_string().contains("replaced as it was made")),
            "{:?}",
            made.as_ref().err()
        );
        assert!(
            dir.join("keep.json").exists(),
            "the directory put in its place was not left whole"
        );
        Ok(())
    }

    /// An empty directory that existed before, put in place of the scratch directory between its making and its
    /// opening, is not taken for it and is left; so is one put there that cannot be opened as a directory; in neither
    /// case is anything removed by name (M1T07R-57).
    #[test]
    fn a_directory_that_existed_before_is_never_the_scratch()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let (before, away) = (root.path().join("before"), root.path().join("away"));
        std::fs::create_dir(&before)?;
        // Older than the probe's room for coarse clocks.
        std::thread::sleep(Duration::from_millis(1300));
        for case in ["empty", "link"] {
            let dir = root.path().join(format!("out-{case}"));
            let swaps = std::rc::Rc::new(std::cell::Cell::new(0));
            let counted = swaps.clone();
            let (made, from, to) = (dir.clone(), before.clone(), away.join(case));
            std::fs::create_dir_all(&away)?;
            BETWEEN.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                    if at == "made" && path == made {
                        std::fs::rename(&made, &to).expect("move the new directory away");
                        if case == "empty" {
                            std::fs::rename(&from, &made).expect("put the old one in its place");
                        } else {
                            std::os::unix::fs::symlink(&from, &made)
                                .expect("put a link in its place");
                        }
                        counted.set(counted.get() + 1);
                    }
                }));
            });
            let made = Scratch::at(
                dir.clone(),
                Instant::now() + Duration::from_secs(60),
                Default::default(),
                Held::default(),
            );
            BETWEEN.with(|hook| *hook.borrow_mut() = None);
            assert_eq!(swaps.get(), 1, "{case}");
            let refused = made
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_default();
            assert!(refused.contains("may be left"), "{case}: {refused}");
            if case == "empty" {
                assert!(
                    refused.contains("was replaced as it was made"),
                    "{case}: {refused}"
                );
                assert!(
                    dir.is_dir(),
                    "{case}: the directory put in its place was removed"
                );
                std::fs::rename(&dir, &before)?;
            } else {
                assert!(
                    std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()),
                    "{case}"
                );
                assert!(before.is_dir(), "{case}");
            }
        }
        Ok(())
    }

    /// A directory whose own name ends in ` (deleted)` is named where it is, not taken for removed (M1T07R-58).
    #[test]
    fn a_directory_named_deleted_is_still_named() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let scratch = Scratch::at(
            root.path().join("out"),
            Instant::now() + Duration::from_secs(60),
            Default::default(),
            Held::default(),
        )?;
        let renamed = root.path().join("out (deleted)");
        std::fs::rename(root.path().join("out"), &renamed)?;
        assert_eq!(
            scratch.held.may_be_left(),
            format!("it may be left at {}", renamed.display())
        );
        std::fs::remove_dir(&renamed)?;
        assert_eq!(scratch.held.may_be_left(), "nothing of it is left");
        Ok(())
    }

    /// An entry is removed only while the probe holds it open, so a file made in its place after it was unlinked
    /// cannot take its inode and be removed for it; on ext4 (the build directory) a freed inode is reused at once
    /// (M1T07R-51).
    #[test]
    fn a_new_file_with_a_freed_inode_is_not_removed_for_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let build = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&build)?;
        let root = tempfile::tempdir_in(&build)?;
        let (out, outside) = (root.path().join("out"), root.path().join("outside"));
        std::fs::create_dir_all(out.join("d"))?;
        std::fs::create_dir(&outside)?;
        std::fs::write(out.join("d/a.json"), "{}")?;
        let (target, child, escaped) = (out.join("d/a.json"), out.join("d"), outside.join("x"));
        let fresh = std::rc::Rc::new(std::cell::Cell::new(None));
        let (seen, to) = (fresh.clone(), escaped.clone());
        BETWEEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                if at == "take" && path == target && seen.get().is_none() {
                    let before = std::os::unix::fs::MetadataExt::ino(
                        &std::fs::metadata(path).expect("stat"),
                    );
                    std::fs::remove_file(path).expect("unlink it");
                    std::fs::rename(&child, &to).expect("move its directory out");
                    std::fs::write(to.join("a.json"), "outside").expect("make a new file there");
                    let after = std::os::unix::fs::MetadataExt::ino(
                        &std::fs::metadata(to.join("a.json")).expect("stat"),
                    );
                    seen.set(Some(before == after));
                }
            }));
        });
        let gone = remove(&out, &Work::new(u64::MAX, None));
        BETWEEN.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(
            fresh.get(),
            Some(false),
            "the new file took the inode of one the probe held"
        );
        assert_eq!(std::fs::read_to_string(escaped.join("a.json"))?, "outside");
        assert!(gone);
        Ok(())
    }

    /// Every entry but the directories is removed before any directory enters the quarantine (M1T07R-51).
    #[test]
    fn directories_are_removed_after_everything_else() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let out = root.path().join("out");
        for d in ["a", "b/c", "d"] {
            std::fs::create_dir_all(out.join(d))?;
            std::fs::write(out.join(d).join("f.json"), "{}")?;
        }
        std::os::unix::fs::symlink("/", out.join("b/link"))?;
        let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = order.clone();
        BETWEEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                if at == "take" {
                    seen.borrow_mut().push((
                        path.to_path_buf(),
                        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()),
                    ));
                }
            }));
        });
        let gone = remove(&out, &Work::new(u64::MAX, None));
        BETWEEN.with(|hook| *hook.borrow_mut() = None);
        assert!(gone);
        let order = order.borrow();
        let first_directory = order
            .iter()
            .position(|(_, dir)| *dir)
            .ok_or("no directory taken")?;
        assert!(
            order[first_directory..].iter().all(|(_, dir)| *dir),
            "{order:?}"
        );
        assert_eq!(order.len(), 9, "{order:?}");
        Ok(())
    }

    /// What a helper puts into a directory already walked while everything but directories is removed goes in the
    /// next pass of the same kind, so that directory never enters the quarantine with something in it (M1T07R-100).
    #[test]
    fn a_directory_refilled_while_files_are_removed_never_enters_the_quarantine_full()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let out = root.path().join("out");
        for d in ["a", "b"] {
            std::fs::create_dir_all(out.join(d))?;
            std::fs::write(out.join(d).join("f.json"), "{}")?;
        }
        // Into whichever directory was walked first, once the other's file is reached: the first is walked through.
        let first = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let full = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (walked, seen, put) = (
            first.clone(),
            full.clone(),
            std::rc::Rc::new(std::cell::Cell::new(false)),
        );
        let refilled = put.clone();
        BETWEEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                if at == "remove" && path.file_name() == Some(OsStr::new("f.json")) {
                    let dir = path.parent().expect("a parent").to_path_buf();
                    let mut walked = walked.borrow_mut();
                    match walked.as_ref() {
                        None => *walked = Some(dir),
                        Some(first) if *first != dir && !put.get() => {
                            std::fs::write(first.join("late.json"), "{}").expect("refill");
                            put.set(true);
                        }
                        Some(_) => {}
                    }
                }
                if at == "take"
                    && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
                    && std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_some())
                {
                    seen.borrow_mut().push(path.to_path_buf());
                }
            }));
        });
        let gone = remove(&out, &Work::new(u64::MAX, None));
        BETWEEN.with(|hook| *hook.borrow_mut() = None);
        assert!(refilled.get(), "the refill did not happen");
        assert!(gone);
        assert_eq!(*full.borrow(), Vec::<PathBuf>::new());
        Ok(())
    }

    /// A directory held whose copy could not be kept, or kept under a lock a panicking thread poisoned, is never
    /// taken for gone (M1T07R-82, -100).
    #[test]
    fn a_directory_held_without_a_copy_is_not_said_to_be_gone()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        COPY_FAILS.with(|f| f.set(true));
        let scratch = Scratch::at(
            root.path().join("out"),
            Instant::now() + Duration::from_secs(60),
            Default::default(),
            Held::default(),
        );
        COPY_FAILS.with(|f| f.set(false));
        let mut scratch = scratch?;
        assert_eq!(
            scratch.held.may_be_left(),
            "it may be left in a directory whose path cannot be read"
        );
        assert!(scratch.remove());
        let poisoned = Held::default();
        let holder = poisoned.clone();
        let _ = std::thread::spawn(move || {
            let _held = holder.0.lock();
            panic!("poison the lock");
        })
        .join();
        let dir = root.path().join("kept");
        std::fs::create_dir(&dir)?;
        poisoned.add(held(&dir).as_fd());
        assert_eq!(
            poisoned.may_be_left(),
            format!("it may be left at {}", dir.display())
        );
        Ok(())
    }

    /// Names made at the same moment on several threads still differ: scratch directories made by concurrent probes
    /// in one process never collide (EEXIST seen in CI on the lifecycle test, FU-20).
    #[test]
    fn names_made_together_never_repeat() {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| (0..20_000).map(|_| unique_name("x")).collect::<Vec<_>>())
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        for names in threads.into_iter().map(|t| t.join().expect("a thread")) {
            for name in names {
                assert!(seen.insert(name.clone()), "{name} made twice");
            }
        }
    }

    /// Generation ends with the probe's window, however long making the scratch directory took (M1T07R-54).
    #[test]
    fn generation_ends_with_the_window_after_a_slow_scratch()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let launcher = tmp.path().join("codex");
        std::fs::write(&launcher, "#!/bin/sh\nexec sleep 30\n")?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        let stall = tmp.path().join("hoh-test-stall-scratch");
        std::fs::write(&stall, "")?;
        let env = ProbeEnv {
            path: std::env::var_os("PATH").unwrap_or_default(),
            fixture_root: None,
            timeout: Duration::from_millis(1500),
            log: None,
            deadline: None,
            cancel: None,
        };
        let started = Instant::now();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1000));
            std::fs::remove_file(stall)
        });
        let result = codex_schema(&launcher, &env);
        let took = started.elapsed();
        release.join().map_err(|_| "the release panicked")??;
        assert!(
            matches!(result, Err(ProbeError::TimedOut { .. })),
            "{result:?}"
        );
        assert!(
            took < Duration::from_millis(2100),
            "generation ran past the window: {took:?}"
        );
        Ok(())
    }

    /// A directory on the way to an entry, replaced by a link to another after it was listed, is not followed: what
    /// the link names is neither read nor removed (M1T07R-51; `RESOLVE_NO_SYMLINKS`).
    #[test]
    fn a_directory_replaced_by_a_link_on_the_way_is_not_followed()
    -> Result<(), Box<dyn std::error::Error>> {
        for moment in ["entry", "remove", "take"] {
            let root = tempfile::tempdir()?;
            let (out, outside) = (root.path().join("out"), root.path().join("outside"));
            std::fs::create_dir_all(out.join("v2"))?;
            std::fs::create_dir(&outside)?;
            std::fs::write(out.join("v2/a.json"), "{}")?;
            std::fs::write(outside.join("a.json"), "outside")?;
            let (target, child, to) = (out.join("v2/a.json"), out.join("v2"), outside.clone());
            let swaps = std::rc::Rc::new(std::cell::Cell::new(0));
            let counted = swaps.clone();
            BETWEEN.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                    if at == moment && path == target && counted.get() == 0 {
                        std::fs::remove_dir_all(&child).expect("remove the directory");
                        std::os::unix::fs::symlink(&to, &child).expect("link it elsewhere");
                        counted.set(1);
                    }
                }));
            });
            let read = if moment == "entry" {
                walk(&out, &Work::new(u64::MAX, None))?
            } else {
                Vec::new()
            };
            let gone = remove(&out, &Work::new(u64::MAX, None));
            BETWEEN.with(|hook| *hook.borrow_mut() = None);
            assert_eq!(swaps.get(), 1, "{moment}: replaced");
            assert!(
                read.iter().all(|(_, bytes)| bytes != b"outside"),
                "{moment}: read through the link"
            );
            assert_eq!(
                std::fs::read_to_string(outside.join("a.json"))?,
                "outside",
                "{moment}: removed through the link"
            );
            assert!(gone, "{moment}");
        }
        Ok(())
    }

    /// Resolution never leaves the directory it starts from, even by a name the listings would never give (`..`):
    /// `RESOLVE_BENEATH` (M1T07R-51).
    #[test]
    fn resolution_never_leaves_where_it_starts() -> Result<(), Box<dyn std::error::Error>> {
        use rustix::fs::OFlags;
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("out"))?;
        std::fs::write(root.path().join("beside.json"), "{}")?;
        let out = held(&root.path().join("out"));
        assert_eq!(
            open_beneath(out.as_fd(), Path::new("../beside.json"), OFlags::RDONLY).err(),
            Some(rustix::io::Errno::XDEV)
        );
        Ok(())
    }

    /// What cannot be removed is named where it is when that is said, after the scratch directory or the directory
    /// holding it moved (M1T07R-52).
    #[test]
    fn what_is_left_is_named_where_it_is() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let (before, after) = (root.path().join("before"), root.path().join("after"));
        std::fs::create_dir(&before)?;
        let scratch = Scratch::at(
            before.join("out"),
            Instant::now() + Duration::from_secs(60),
            Default::default(),
            Held::default(),
        )?;
        std::fs::rename(&before, &after)?;
        let said = scratch.held.may_be_left();
        assert_eq!(
            said,
            format!("it may be left at {}", after.join("out").display())
        );
        std::fs::rename(after.join("out"), root.path().join("moved"))?;
        assert_eq!(
            scratch.held.may_be_left(),
            format!("it may be left at {}", root.path().join("moved").display())
        );
        drop(scratch);
        Ok(())
    }

    /// A directory still there whose path cannot be read (deeper than PATH_MAX) is never said to be gone: only a link
    /// count of 0 says that (M1T07R-81).
    #[test]
    fn a_directory_whose_path_cannot_be_read_is_not_said_to_be_gone()
    -> Result<(), Box<dyn std::error::Error>> {
        use rustix::fs::{Mode, OFlags, mkdirat, openat, renameat};
        let root = tempfile::tempdir()?;
        let scratch = Scratch::at(
            root.path().join("out"),
            Instant::now() + Duration::from_secs(60),
            Default::default(),
            Held::default(),
        )?;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let top = openat(rustix::fs::CWD, root.path(), flags, Mode::empty())?;
        let mut deep = openat(&top, ".", flags, Mode::empty())?;
        let long = "d".repeat(250);
        for _ in 0..20 {
            mkdirat(&deep, long.as_str(), Mode::from_raw_mode(0o700))?;
            deep = openat(&deep, long.as_str(), flags, Mode::empty())?;
        }
        renameat(&top, "out", &deep, "out")?;
        let said = scratch.held.may_be_left();
        renameat(&deep, "out", &top, "out")?;
        assert_eq!(
            said,
            "it may be left in a directory whose path cannot be read"
        );
        drop(scratch);
        assert!(!root.path().join("out").exists());
        Ok(())
    }

    /// A quarantine replaced as it is made is never removed through: nothing of the output is removed, the directory
    /// put in its place is left whole, and the failure says why and where the quarantine may be left (M1T07R-90).
    #[test]
    fn a_quarantine_replaced_as_it_is_made_is_named() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let (other, away) = (root.path().join("other"), root.path().join("away"));
        std::fs::create_dir(&other)?;
        std::fs::write(other.join("keep.json"), "{}")?;
        let scratch = Scratch::at(
            root.path().join("out"),
            Instant::now() + Duration::from_secs(60),
            Default::default(),
            Held::default(),
        )?;
        std::fs::write(root.path().join("out/a.json"), "{}")?;
        let named = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let (temp, seen) = (root.path().to_path_buf(), named.clone());
        BETWEEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                let Some(name) = path.file_name() else {
                    return;
                };
                if at == "made" && name.to_string_lossy().starts_with("out-q-") {
                    let made = temp.join(name);
                    std::fs::rename(&made, &away).expect("move the quarantine away");
                    std::fs::rename(&other, &made).expect("put another in its place");
                    *seen.borrow_mut() = Some(made);
                }
            }));
        });
        let result = read_and_remove(scratch, Ok(()), &["codex".to_owned()], None);
        BETWEEN.with(|hook| *hook.borrow_mut() = None);
        let quarantine = named.borrow().clone().ok_or("no quarantine was made")?;
        let Err(ProbeError::Failed { detail, .. }) = result else {
            return Err(format!("{result:?}").into());
        };
        assert!(
            detail.contains(&format!(
                "{} was replaced as it was made",
                quarantine.display()
            )),
            "{detail}"
        );
        assert!(
            detail.contains(&format!(
                "it may be left at {}",
                root.path().join("out").display()
            )),
            "{detail}"
        );
        assert!(
            root.path().join("out/a.json").exists(),
            "the output was removed through another's directory"
        );
        assert!(
            quarantine.join("keep.json").exists(),
            "the directory put in its place was not left whole"
        );
        Ok(())
    }

    /// A probe that gives up while its reading is stalled names the scratch directory where it is then, though the
    /// CLI moved it after writing (M1T07R-52).
    #[test]
    fn a_reading_given_up_names_the_output_where_it_is() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (said, launcher) = (tmp.path().join("out-path"), tmp.path().join("codex"));
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\necho \"$4\" > '{}.new' && mv '{0}.new' '{0}'\ntouch \"$4/hoh-test-stall-walk\"\n",
                said.display()
            ),
        )?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        let env = ProbeEnv {
            path: std::env::var_os("PATH").unwrap_or_default(),
            fixture_root: None,
            timeout: Duration::from_millis(500),
            log: None,
            deadline: None,
            cancel: None,
        };
        let caller = std::thread::spawn(move || codex_schema(&launcher, &env));
        let until = Instant::now() + Duration::from_secs(10);
        while !said.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        let dir = PathBuf::from(std::fs::read_to_string(&said)?.trim());
        // Moved while the reading is stalled at the walk.
        std::thread::sleep(Duration::from_millis(200));
        let moved = dir.with_extension("moved");
        std::fs::rename(&dir, &moved)?;
        let result = caller.join().map_err(|_| "the caller panicked")?;
        std::fs::remove_file(moved.join("hoh-test-stall-walk"))?;
        let detail = match result {
            Err(ProbeError::Failed { detail, .. }) => detail,
            other => return Err(format!("{other:?}").into()),
        };
        let until = Instant::now() + Duration::from_secs(10);
        // Released, the reading does nothing more to the output and ends.
        std::thread::sleep(Duration::from_millis(100));
        let _ = until;
        std::fs::remove_dir_all(&moved)?;
        assert!(
            detail.contains(&format!("may be left at {}", moved.display())),
            "{detail}"
        );
        Ok(())
    }

    /// A file with another link, which can be a file outside the output, refuses the output unread (M1T07R-46).
    #[test]
    fn a_file_linked_from_elsewhere_refuses_the_output() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let outside = root.path().join("keep.txt");
        std::fs::write(&outside, "kept")?;
        let dir = root.path().join("out");
        std::fs::create_dir(&dir)?;
        std::fs::write(dir.join("a.json"), "{}")?;
        assert!(walk(&dir, &Work::new(u64::MAX, None)).is_ok());
        std::fs::hard_link(&outside, dir.join("borrowed.bin"))?;
        let listed = walk(&dir, &Work::new(u64::MAX, None));
        assert!(
            listed
                .as_ref()
                .is_err_and(|e| e.contains("borrowed.bin") && e.contains("link")),
            "{listed:?}"
        );
        Ok(())
    }

    /// A file replaced while the output is walked is judged as opened: a link to another file is not read, a FIFO is
    /// skipped without waiting for a writer, and a file that grows past the limits once its size was taken is refused
    /// with no more read than they allow (M1T07R-44).
    #[test]
    fn a_file_replaced_during_the_walk_is_judged_as_opened()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let outside = root.path().join("outside.json");
        std::fs::write(&outside, "{}")?;
        for (moment, change) in [("entry", "link"), ("entry", "fifo"), ("read", "grow")] {
            let dir = root.path().join(change);
            std::fs::create_dir(&dir)?;
            let file = dir.join("a.json");
            std::fs::write(&file, "{}")?;
            let (to, done) = (outside.clone(), std::sync::mpsc::channel());
            std::thread::spawn(move || {
                let target = file.clone();
                let changes = std::rc::Rc::new(std::cell::Cell::new(0));
                let counted = changes.clone();
                BETWEEN.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move |at: &str, path: &Path| {
                        if at != moment || path != target {
                            return;
                        }
                        counted.set(counted.get() + 1);
                        match change {
                            "link" => {
                                std::fs::remove_file(path).expect("remove the file");
                                std::os::unix::fs::symlink(&to, path).expect("link it elsewhere");
                            }
                            "fifo" => {
                                std::fs::remove_file(path).expect("remove the file");
                                rustix::fs::mknodat(
                                    rustix::fs::CWD,
                                    path,
                                    rustix::fs::FileType::Fifo,
                                    rustix::fs::Mode::from_raw_mode(0o600),
                                    0,
                                )
                                .expect("make a FIFO");
                            }
                            _ => {
                                let mut grown = std::fs::OpenOptions::new()
                                    .append(true)
                                    .open(path)
                                    .expect("open");
                                let spaces = vec![b' '; 1 << 20];
                                for _ in 0..=MAX_SCHEMA_FILE >> 20 {
                                    std::io::Write::write_all(&mut grown, &spaces).expect("grow");
                                }
                            }
                        }
                    }));
                });
                let listed = walk(&dir, &Work::new(u64::MAX, None))
                    .map(|files| files.into_iter().map(|(path, _)| path).collect::<Vec<_>>());
                let _ = done.0.send((listed, changes.get()));
            });
            let (listed, changes) = done.1.recv_timeout(Duration::from_secs(10))?;
            // The file was changed where the test means it to be, once (M1T07R-48).
            assert_eq!(changes, 1, "{change}: changed");
            match change {
                "grow" => assert!(
                    listed
                        .as_ref()
                        .is_err_and(|e| e.contains("changed while it was read")),
                    "{change}: {listed:?}"
                ),
                _ => assert_eq!(listed, Ok(Vec::new()), "{change}"),
            }
        }
        Ok(())
    }

    /// Removing an output spends a step for every directory it empties, not only for every entry it lists: a flat
    /// output of empty directories reads the deadline while they are removed (M1T07R-44).
    #[test]
    fn removal_is_charged_after_the_listing_too() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for (budget, gone) in [(400, false), (u64::MAX, true)] {
            let dir = root.path().join(budget.to_string());
            for n in 0..300 {
                std::fs::create_dir_all(dir.join(n.to_string()))?;
            }
            assert_eq!(remove(&dir, &Work::new(budget, None)), gone, "{budget}");
            assert_eq!(dir.exists(), !gone, "{budget}");
        }
        Ok(())
    }

    /// An output nested deeper than [`MAX_SCHEMA_DEPTH`] is refused, and left when removed; one exactly that deep is
    /// read and removed (M1T07R-44).
    #[test]
    fn output_deeper_than_the_limit_is_refused_and_left() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        for (depth, within) in [(MAX_SCHEMA_DEPTH, true), (MAX_SCHEMA_DEPTH + 1, false)] {
            let dir = root.path().join(depth.to_string());
            let bottom = (0..depth).fold(dir.clone(), |path, _| path.join("d"));
            std::fs::create_dir_all(&bottom)?;
            std::fs::write(bottom.join("a.json"), "{}")?;
            let listed = walk(&dir, &Work::new(u64::MAX, None));
            assert_eq!(listed.is_ok(), within, "{depth}: {:?}", listed.err());
            if !within {
                assert!(listed.is_err_and(|e| e.contains("too deep")));
            }
            assert_eq!(remove(&dir, &Work::new(u64::MAX, None)), within, "{depth}");
            assert_eq!(dir.exists(), !within, "{depth}");
        }
        Ok(())
    }

    /// The recorded schema read whole gives its 192 methods with only the three protocol files parsed into values;
    /// cut short by the budget or the deadline, the reading fails instead of reporting fewer methods (M1T07R-40).
    #[test]
    fn a_reading_cut_short_fails_instead_of_dropping_methods()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/protocol/codex/0.166.0/app-server-schema");
        let files = walk(&root, &Work::new(u64::MAX, None))?;
        PARSED.with(|n| n.set(0));
        let whole = read_schema(
            &root,
            &files,
            &Work::new(VALIDATION_BUDGET, None),
            &Work::new(u64::MAX, None),
        )?;
        assert_eq!(PARSED.with(std::cell::Cell::get), 3);
        assert_eq!(
            whole.requests.len() + whole.client_notifications.len() + whole.notifications.len(),
            192
        );
        let past = Some(Instant::now() - Duration::from_secs(1));
        for (name, work) in [
            ("budget", Work::new(1_000, None)),
            ("deadline", Work::new(VALIDATION_BUDGET, past)),
        ] {
            let cut = read_schema(&root, &files, &work, &Work::new(u64::MAX, None));
            assert!(
                cut.as_ref().is_err_and(|e| e.contains("work limits")),
                "{name}: {:?}",
                cut.map(|s| s.requests.len())
            );
        }
        Ok(())
    }

    /// A protocol file that would parse into more than [`MAX_SCHEMA_VALUES`] values is refused before it is parsed;
    /// brackets, commas and colons inside strings are not values (M1T07R-50).
    #[test]
    fn a_protocol_file_past_its_value_limit_is_refused_unparsed()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(json_values(br#"{"a":"[,{\"}:"}"#), 3);
        assert_eq!(json_values(b"[]"), 2);
        let recorded = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/protocol/codex/0.166.0/app-server-schema");
        let root = tempfile::tempdir()?;
        for name in [
            "ClientRequest.json",
            "ClientNotification.json",
            "ServerNotification.json",
        ] {
            std::fs::copy(recorded.join(name), root.path().join(name))?;
        }
        let mut requests: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("ClientRequest.json"))?)?;
        requests["definitions"]["Pad"] = serde_json::Value::Array(
            (0..MAX_SCHEMA_VALUES / 3)
                .map(|_| serde_json::json!({"": 0}))
                .collect(),
        );
        std::fs::write(
            root.path().join("ClientRequest.json"),
            serde_json::to_vec(&requests)?,
        )?;
        let files = walk(root.path(), &Work::new(u64::MAX, None))?;
        PARSED.with(|n| n.set(0));
        let read = read_schema(
            root.path(),
            &files,
            &Work::new(VALIDATION_BUDGET, None),
            &Work::new(u64::MAX, None),
        );
        assert!(
            read.as_ref().is_err_and(|e| e.contains("values")),
            "{:?}",
            read.map(|s| s.requests.len())
        );
        // Only ClientNotification.json, which comes before it, was parsed.
        assert_eq!(PARSED.with(std::cell::Cell::get), 1);
        Ok(())
    }

    /// A mount point is not crossed: `openat2` refuses it (M1T07R-49). `/dev/shm`
    /// is a mount point on Linux; `/dev/null` is on `/dev`'s own file system. Opened as paths, neither is opened.
    #[test]
    fn a_mount_point_is_not_crossed() -> Result<(), Box<dyn std::error::Error>> {
        use rustix::fs::OFlags;
        let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
        assert!(
            mounts
                .lines()
                .any(|l| l.split(' ').nth(4) == Some("/dev/shm")),
            "/dev/shm is not a mount point here"
        );
        let dev = held(Path::new("/dev"));
        let flags = OFlags::PATH | OFlags::NOFOLLOW;
        assert_eq!(
            open_beneath(dev.as_fd(), Path::new("shm"), flags).err(),
            Some(rustix::io::Errno::XDEV)
        );
        assert!(open_beneath(dev.as_fd(), Path::new("null"), flags).is_ok());
        Ok(())
    }

    /// Another file system in the output refuses it unread and is left whole when the output is removed; one mounted
    /// over the output directory itself is not taken for it, and is left (M1T07R-49). `hoh-test-mount` stands for a
    /// mount point ([`open_beneath`] answers `EXDEV` for it in tests).
    #[test]
    fn a_mount_in_or_over_the_output_is_refused_and_left() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("out");
        std::fs::create_dir_all(dir.join("v2"))?;
        std::fs::write(dir.join("v2/a.json"), "{}")?;
        std::fs::create_dir(dir.join("hoh-test-mount"))?;
        std::fs::write(dir.join("hoh-test-mount/inside.json"), "{}")?;
        let listed = walk(&dir, &Work::new(u64::MAX, None));
        assert!(
            listed.as_ref().is_err_and(|e| e.contains("mount point")),
            "{listed:?}"
        );
        assert!(!remove(&dir, &Work::new(u64::MAX, None)));
        assert!(dir.join("hoh-test-mount/inside.json").exists());
        assert!(!dir.join("v2").exists());
        let over = root.path().join("hoh-test-mount");
        std::fs::create_dir(&over)?;
        std::fs::write(over.join("inside.json"), "{}")?;
        let (parent, owned) = (parent_of(&over), held(&over));
        assert!(!names(
            parent.as_fd(),
            Path::new("hoh-test-mount"),
            inode_of(&owned)
        ));
        assert!(!remove_held(&over, &owned, &Work::new(u64::MAX, None)));
        assert!(over.exists());
        Ok(())
    }

    const LIFECYCLE: &str = "HOH_TEST_SCHEMA_LIFECYCLE";

    /// Giving up on the schema reading and the reading's end are told apart (M1T07R-40, -47): run in a process of
    /// its own so its threads and file descriptors can be counted.
    #[test]
    fn a_reading_given_up_stops_and_lets_everything_go() -> Result<(), Box<dyn std::error::Error>> {
        let out = std::process::Command::new(std::env::current_exe()?)
            .args([
                "probe::tests::reading_given_up_child",
                "--exact",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(LIFECYCLE, "1")
            .output()?;
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    }

    /// Whatever part of the reading stalls (as a call that does not return would), each probe returns within its
    /// window, the cleanup grace and the margin; while stalled, each reading given up holds one thread and a bounded
    /// number of file descriptors; released, it does nothing more to the output past the step in progress, and lets
    /// every thread and file descriptor go; a second round leaves no more than the first; and a reading that ends in
    /// time has let everything go when the probe returns.
    #[test]
    #[ignore = "run by a_reading_given_up_stops_and_lets_everything_go in a process of its own"]
    fn reading_given_up_child() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os(LIFECYCLE).is_none() {
            return Ok(());
        }
        let count = |dir: &str| std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0);
        let listing = |dir: &Path| -> std::collections::BTreeSet<PathBuf> {
            let mut seen = std::collections::BTreeSet::new();
            let mut stack = vec![dir.to_path_buf()];
            while let Some(d) = stack.pop() {
                for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push(entry.path());
                    }
                    seen.insert(entry.path());
                }
            }
            seen
        };
        let tmp = tempfile::tempdir()?;
        let schema = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/protocol/codex/0.166.0/app-server-schema");
        let moments = [
            "walk", "entry", "read", "parse", "validate", "hash", "remove",
        ];
        for moment in moments {
            let launcher = tmp.path().join(format!("codex-{moment}"));
            std::fs::write(
                &launcher,
                format!(
                    "#!/bin/sh\n[ \"$1\" = app-server ] && [ \"$2\" = generate-json-schema ] && [ \"$3\" = --out ] \
                     && cp -R '{}/.' \"$4/\" && touch \"$4/hoh-test-stall-{moment}\"\n",
                    schema.display()
                ),
            )?;
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        }
        let env = ProbeEnv {
            path: std::env::var_os("PATH").unwrap_or_default(),
            fixture_root: None,
            // Room for the commands under load: the bound below is measured from this window.
            timeout: Duration::from_secs(3),
            log: None,
            deadline: None,
            cancel: None,
        };
        let baseline = (count("/proc/self/fd"), count("/proc/self/task"));
        for round in 0..2 {
            RELEASE.store(false, std::sync::atomic::Ordering::SeqCst);
            let callers: Vec<_> = moments
                .iter()
                .map(|moment| {
                    let (launcher, env) = (tmp.path().join(format!("codex-{moment}")), env.clone());
                    std::thread::spawn(move || {
                        let started = Instant::now();
                        let result = codex_schema(&launcher, &env);
                        (started.elapsed(), result)
                    })
                })
                .collect();
            let mut left = Vec::new();
            for (moment, caller) in moments.iter().zip(callers) {
                let (took, result) = caller.join().map_err(|_| "a caller panicked")?;
                // On time: the window, the cleanup grace, the margin, and a little for the commands.
                assert!(
                    took < env.timeout + CLEANUP_GRACE + READER_MARGIN + Duration::from_millis(700),
                    "{round} {moment}: returned after {took:?}"
                );
                let detail = match result {
                    Err(ProbeError::Failed { detail, .. }) => detail,
                    other => return Err(format!("{round} {moment}: {other:?}").into()),
                };
                // `it may be left at <scratch>`, with ` (and <quarantine>)` once removal began.
                let at = detail
                    .split("may be left at ")
                    .nth(1)
                    .ok_or_else(|| format!("{round} {moment}: {detail}"))?;
                let (dir, also) = at.split_once(" (and ").unwrap_or((at, ""));
                for dir in std::iter::once(dir).chain(
                    also.strip_suffix(')')
                        .into_iter()
                        .flat_map(|a| a.split(", ")),
                ) {
                    let dir = PathBuf::from(dir);
                    left.push((moment, listing(&dir), dir));
                }
            }
            // Stalled: one thread each, and at most the walk's directories, a file, the scratch and its parent.
            let (fds, threads) = (count("/proc/self/fd"), count("/proc/self/task"));
            assert!(
                threads <= baseline.1 + moments.len(),
                "{round}: {threads} threads"
            );
            assert!(
                fds <= baseline.0 + moments.len() * (MAX_SCHEMA_DEPTH + 4),
                "{round}: {fds} file descriptors"
            );
            RELEASE.store(true, std::sync::atomic::Ordering::SeqCst);
            let released = Instant::now();
            while count("/proc/self/task") > baseline.1
                && released.elapsed() < Duration::from_secs(10)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                (count("/proc/self/fd"), count("/proc/self/task")),
                baseline,
                "{round}: file descriptors and threads after the readings ended"
            );
            for (moment, before, dir) in left {
                let after = listing(&dir);
                // Nothing came, and nothing went but the entry whose removal was in progress.
                assert!(after.is_subset(&before), "{round} {moment}: something came");
                let gone: Vec<_> = before.difference(&after).collect();
                let stalled = dir.join(format!("hoh-test-stall-{moment}"));
                assert!(
                    gone.is_empty() || (*moment == "remove" && gone == [&stalled]),
                    "{round} {moment}: removed after the probe returned: {gone:?}"
                );
                if dir.exists() {
                    std::fs::remove_dir_all(&dir)?;
                }
            }
        }
        let plain = tmp.path().join("codex-plain");
        std::fs::write(
            &plain,
            format!(
                "#!/bin/sh\n[ \"$1\" = app-server ] && [ \"$2\" = generate-json-schema ] && [ \"$3\" = --out ] \
                 && cp -R '{}/.' \"$4/\"\n",
                schema.display()
            ),
        )?;
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755))?;
        // A joined thread's task is released by the kernel just after the join returns (the join wakes on the thread's
        // tid futex, cleared before its task leaves /proc/self/task): a joined reader is let go within this bound, not
        // at the very instant (it showed as a running `hoh-schema` task under load).
        let threads_settled = || {
            let until = Instant::now() + Duration::from_secs(1);
            while count("/proc/self/task") > baseline.1 && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            (count("/proc/self/fd"), count("/proc/self/task"))
        };
        let read = codex_schema(&plain, &env)?;
        assert_eq!(
            read.requests.len() + read.client_notifications.len() + read.notifications.len(),
            192
        );
        assert_eq!(
            threads_settled(),
            baseline,
            "file descriptors and threads as a reading in time returns"
        );
        // The probe returns only once its reading has ended, even when that comes a while after the result.
        HOLD_AFTER_RESULT.store(true, std::sync::atomic::Ordering::SeqCst);
        let started = Instant::now();
        codex_schema(&plain, &env)?;
        let took = started.elapsed();
        HOLD_AFTER_RESULT.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(
            took >= Duration::from_millis(300),
            "returned in {took:?}, before its reading ended"
        );
        assert_eq!(
            threads_settled(),
            baseline,
            "file descriptors and threads once the reading ended"
        );
        // Making the scratch directory stalls (its file system does not answer): the probe returns on time all the
        // same and starts nothing; released, the work ends and lets everything go (M1T07R-54).
        let slow = tmp.path().join("slow");
        std::fs::create_dir(&slow)?;
        let (launcher, ran) = (slow.join("codex"), slow.join("ran"));
        std::fs::write(&launcher, format!("#!/bin/sh\ntouch '{}'\n", ran.display()))?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        let stall = slow.join("hoh-test-stall-scratch");
        std::fs::write(&stall, "")?;
        // Released by removing the entry, not by the rounds' release.
        RELEASE.store(false, std::sync::atomic::Ordering::SeqCst);
        let started = Instant::now();
        let result = codex_schema(&launcher, &env);
        let took = started.elapsed();
        assert!(
            took < env.timeout + CLEANUP_GRACE + READER_MARGIN + Duration::from_millis(700),
            "a stalled scratch: returned after {took:?}"
        );
        assert!(
            matches!(&result, Err(ProbeError::Failed { detail, .. }) if detail.contains("its scratch directory under")),
            "{result:?}"
        );
        assert_eq!(
            count("/proc/self/task"),
            baseline.1 + 1,
            "a stalled scratch: threads"
        );
        std::fs::remove_file(&stall)?;
        let released = Instant::now();
        while count("/proc/self/task") > baseline.1 && released.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            (count("/proc/self/fd"), count("/proc/self/task")),
            baseline,
            "file descriptors and threads once the stalled scratch was released"
        );
        assert!(
            !ran.exists(),
            "the CLI was started for a probe that had given up"
        );
        let mine = format!("hoh-probe-{}-", std::process::id());
        let left: Vec<_> = std::fs::read_dir(std::env::temp_dir())?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&mine))
            .map(|e| e.path())
            .collect();
        assert!(left.is_empty(), "left in the temporary directory: {left:?}");
        Ok(())
    }

    /// A command whose output cannot be waited for is not left running: it is killed with its group and reaped
    /// before the error is returned, whatever the exit (M1T07R-53).
    #[test]
    fn a_command_is_ended_on_every_exit() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let env = ProbeEnv {
            path: std::env::var_os("PATH").unwrap_or_default(),
            fixture_root: None,
            timeout: Duration::from_secs(5),
            log: None,
            deadline: None,
            cancel: None,
        };
        for n in 0..3 {
            let pidfile = tmp.path().join(format!("pid-{n}"));
            // Written whole before it appears.
            let script = format!(
                "echo $$ > '{0}.new' && mv '{0}.new' '{0}'; exec sleep 30",
                pidfile.display()
            );
            // The wait for its output fails once the command has written its pid.
            POLL_FAILS.with(|f| *f.borrow_mut() = Some(pidfile.clone()));
            let started = Instant::now();
            let result = run_status(Path::new("/bin/sh"), &["-c", &script], &env);
            POLL_FAILS.with(|f| *f.borrow_mut() = None);
            assert!(
                matches!(
                    result,
                    Err(ProbeError::Io(_)) | Err(ProbeError::Failed { .. })
                ),
                "{n}: {result:?}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{n}: returned after {:?}",
                started.elapsed()
            );
            let pid = std::fs::read_to_string(&pidfile)?.trim().parse::<i32>()?;
            // Neither running nor a zombie: no /proc entry is left for it.
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "{n}: command {pid} left after the error"
            );
        }
        Ok(())
    }

    /// A command whose leader moves itself into another process group of the session is still ended: the leader is
    /// killed on its own, not only through the group it left (M1T07R-80).
    #[test]
    fn a_leader_that_leaves_its_group_is_ended() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (pidfile, launcher) = (tmp.path().join("pid"), tmp.path().join("cli"));
        std::fs::write(
            &launcher,
            format!(
                "#!/usr/bin/env python3\nimport os, time\nos.setpgid(0, os.getpgid(os.getppid()))\n\
                 open('{0}.new', 'w').write(str(os.getpid()))\nos.rename('{0}.new', '{0}')\ntime.sleep(30)\n",
                pidfile.display()
            ),
        )?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        let env = ProbeEnv {
            path: std::env::var_os("PATH").unwrap_or_default(),
            fixture_root: None,
            timeout: Duration::from_secs(2),
            log: None,
            deadline: None,
            cancel: None,
        };
        let result = run_status(&launcher, &[], &env);
        let pid = std::fs::read_to_string(&pidfile)?.trim().parse::<i32>()?;
        let left = Path::new(&format!("/proc/{pid}")).exists();
        if left && let Some(pid) = Pid::from_raw(pid) {
            let _ = rustix::process::kill_process(pid, Signal::KILL);
        }
        assert!(
            matches!(result, Err(ProbeError::TimedOut { .. })),
            "{result:?}"
        );
        assert!(!left, "the leader {pid} outlived the probe");
        Ok(())
    }

    /// A scratch directory is removed within its deadline, or left (M1T07R-40, round 14).
    #[test]
    fn scratch_is_removed_within_its_deadline() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for (name, until, gone) in [
            ("in-time", Instant::now() + Duration::from_secs(60), true),
            ("late", Instant::now() - Duration::from_secs(1), false),
        ] {
            let dir = root.path().join(name);
            for n in 0..5_000 {
                std::fs::create_dir_all(dir.join(format!("{}/{n}", n % 7)))?;
            }
            assert_eq!(
                remove(&dir, &Work::new(u64::MAX, Some(until))),
                gone,
                "{name}"
            );
            assert_eq!(dir.exists(), !gone, "{name}");
        }
        Ok(())
    }
}
