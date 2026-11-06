//! `probe()`: what an installed agent CLI is and what it can do, from its launcher, `--version`, `--help` and native
//! schema (REQ-027, docs/13-adapters.md). Capabilities come from what was probed, never from the brand; what the
//! probe cannot establish stays `None` for the compat suite to decide. Probe commands are short, run without stdin,
//! with a timeout, in their own process group, which is killed on timeout (the full process substrate is M1-T02).

use crate::compat::{Agent, AgentEntry, Suite, SuiteStatus, Transport};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
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
}

/// The longest definition name (and so `$ref` target) the probe reads: resolving a reference then costs a bounded
/// amount, however often it is used (M1T07R-40). The recorded 0.166.0 schema's longest is 54 bytes.
const MAX_NAME: usize = 256;

/// The largest App Server schema output the probe reads: a file, all files, the number of files (the recorded 0.166.0
/// output is 321 files, 3.7 MB in all, the largest 750 KB); beyond them it is refused before parsing (M1T07R-40).
const MAX_SCHEMA_FILE: u64 = 16 * 1024 * 1024;
const MAX_SCHEMA_TOTAL: u64 = 64 * 1024 * 1024;
const MAX_SCHEMA_FILES: usize = 4096;

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

/// A copy of a protocol schema file whose branches accept any `params`: the probe checks a method's envelope, not its
/// parameters, which the compat suite verifies against recorded traffic (ADR-015 decision 2, M1T07R-33).
fn params_open(file: &serde_json::Value) -> serde_json::Value {
    let mut open = file.clone();
    // The root's `properties` (ServerNotification.json) and every branch's (M1T07R-37).
    if let Some(params) = open.get_mut("properties").and_then(|p| p.get_mut("params")) {
        *params = serde_json::Value::Bool(true);
    }
    for branch in open["oneOf"].as_array_mut().into_iter().flatten() {
        if let Some(params) = branch["properties"].get_mut("params") {
            *params = serde_json::Value::Bool(true);
        }
    }
    open
}

/// How deep schemas may nest while validating; deeper is undecided, so a cyclic definition cannot loop.
const REF_DEPTH: usize = 64;

/// How many steps one reading of the App Server schema may take in all: every schema node visited and every element it
/// examines (members of `allOf`/`anyOf`/`oneOf`, values of `enum`, names of `required`, properties, an instance's
/// keys) is a step. Depth alone does not bound the work of a definition naming itself twice, or of wide members and
/// long enums reused many times; past the budget a validation is undecided, so the method is absent and the probe
/// ends within a second (M1T07R-40). The recorded 0.166.0 schema needs about 150,000.
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
}

impl Work {
    fn new(steps: u64, until: Option<Instant>) -> Work {
        Work {
            steps: std::cell::Cell::new(steps),
            until,
        }
    }

    /// Take `n` steps; false once the budget or the deadline is used up.
    fn spend(&self, n: usize) -> bool {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        let left = self.steps.get();
        if left < n {
            self.steps.set(0);
            return false;
        }
        let after = left - n;
        self.steps.set(after);
        if left / 4096 != after / 4096 && self.until.is_some_and(|until| Instant::now() >= until) {
            self.steps.set(0);
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
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("hoh-probe-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir)?;
    let result = (|| {
        let out = dir.to_string_lossy().into_owned();
        run_ok(
            launcher,
            &["app-server", "generate-json-schema", "--out", &out],
            env,
        )?;
        let unusable = |detail: String| ProbeError::Failed {
            argv: argv(launcher, &["app-server", "generate-json-schema"]),
            detail,
        };
        // Reading the schema is bounded by doctor's shared deadline, or without one by the probe's own command limit
        // (M1T07R-40).
        let until = Some(reading_deadline(env, Instant::now()));
        let work = Work::new(VALIDATION_BUDGET, until);
        // The checks before validation are linear in the files; only the deadline bounds them, each file's bytes
        // counted before it is parsed (M1T07R-40, -42).
        let checks = Work::new(u64::MAX, until);
        // Bounded before anything is read, so parsing and hashing are bounded too (M1T07R-40).
        let files = files_under(&dir)?;
        if files.len() > MAX_SCHEMA_FILES {
            return Err(unusable(format!(
                "too many schema files: {} (at most {MAX_SCHEMA_FILES})",
                files.len()
            )));
        }
        let mut total = 0u64;
        for file in &files {
            let size = std::fs::metadata(file)?.len();
            total += size;
            if size > MAX_SCHEMA_FILE || total > MAX_SCHEMA_TOTAL {
                return Err(unusable(format!(
                    "the schema output is too large: {} is {size} bytes, {total} in all (at most {MAX_SCHEMA_FILE} a file, {MAX_SCHEMA_TOTAL} in all)",
                    file.display()
                )));
            }
        }
        let mut parsed = BTreeMap::new();
        let mut texts = BTreeMap::new();
        for file in files {
            if file.extension().is_some_and(|e| e == "json") {
                let bytes = std::fs::read(&file)?;
                if !checks.spend(bytes.len()) {
                    return Err(unusable(format!(
                        "{} was not read within the time limit",
                        file.display()
                    )));
                }
                let value = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map_err(|e| unusable(format!("{} is not JSON: {e}", file.display())))?;
                parsed.insert(file.clone(), value);
                texts.insert(file, String::from_utf8_lossy(&bytes).into_owned());
            }
        }
        // The methods a file lists in the known form whose message can be valid: `envelope` is the message for a
        // method, checked against the file with the params left to the compat suite (M1T07R-33).
        let methods = |name: &str, sent: bool, envelope: &dyn Fn(&str) -> serde_json::Value| {
            let value = parsed
                .get(&dir.join(name))
                .ok_or_else(|| unusable(format!("{name} is missing")))?;
            draft7_well_formed(value, &checks)
                .map_err(|e| unusable(format!("{name} is not a well-formed schema: {e}")))?;
            match texts
                .get(&dir.join(name))
                .map(|t| inexact_number(t, &checks))
            {
                Some(Ok(Some(number))) => {
                    return Err(unusable(format!(
                        "{name} has a number the probe cannot read exactly: {number}"
                    )));
                }
                Some(Err(())) => {
                    return Err(unusable(format!(
                        "{name} was not read within the time limit"
                    )));
                }
                _ => {}
            }
            let listed = schema_methods(value, sent, &checks)
                .map_err(|()| unusable(format!("{name} was not read within the time limit")))?
                .ok_or_else(|| {
                    unusable(format!(
                        "{name} does not list its methods in the known form"
                    ))
                })?;
            let open = params_open(value);
            let empty = serde_json::Map::new();
            let defs = open
                .get("definitions")
                .and_then(serde_json::Value::as_object)
                .unwrap_or(&empty);
            let branches = branches_by_method(&open, &checks);
            Ok::<_, ProbeError>(
                listed
                    .into_iter()
                    .filter(|m| valid(&open, &envelope(m), defs, 0, &work) == Truth::Yes)
                    .filter(|m| {
                        // A request also needs an id schema that takes every id the adapter numbers (M1T07R-36).
                        !name.starts_with("ClientRequest")
                            || branches.get(m.as_str()).is_some_and(|branch| {
                                branch["properties"]
                                    .get("id")
                                    .is_none_or(|id| takes_every_id(id, defs, 0, &work))
                            })
                    })
                    .collect(),
            )
        };
        // HoH's requests (any id will do here: whether the id schema takes every id is checked apart), its only
        // notification `initialized` without params, and the server's notifications with `method` and `params`.
        let request = |m: &str| serde_json::json!({"id": 0, "method": m, "params": {}});
        Ok(CodexSchema {
            requests: methods("ClientRequest.json", true, &request)?,
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
            hash: tree_sha256(&dir)?,
        })
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
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
    let mut lines = Vec::new();
    for file in files_under(root)? {
        let rel = file.strip_prefix(root).map_err(io::Error::other)?;
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        lines.push(format!("{rel} {}\n", file_sha256(&file)?));
    }
    lines.sort();
    Ok(hex(&Sha256::digest(lines.concat().as_bytes())))
}

fn file_sha256(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
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
    let mut attempt = 0;
    let mut child = loop {
        match spawn() {
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy && attempt < 20 => {
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
            other => break other.map_err(|e| failed(format!("cannot start: {e}")))?,
        }
    };
    let (tx, rx) = mpsc::channel();
    if let Some(mut stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = tx.send(stdout.read_to_end(&mut buf).map(|_| buf));
        });
    }
    let (etx, erx) = mpsc::channel();
    if let Some(mut stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = etx.send(stderr.read_to_end(&mut buf).map(|_| buf));
        });
    } else {
        let _ = etx.send(Ok(Vec::new()));
    }
    let timed_out = || ProbeError::TimedOut {
        argv: argv(program, args),
        after: limit,
        cut_by_deadline: limit < env.timeout,
    };
    let deadline = end;
    let pid = Pid::from_child(&child);
    let exited = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
    loop {
        // Observe the exit without reaping: while the leader is a zombie its process group id cannot be reused.
        if waitid(WaitId::Pid(pid), exited)
            .map_err(io::Error::from)?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = kill_process_group(pid, Signal::KILL);
            let _ = child.wait();
            return Err(timed_out());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Whatever the command left running in its group goes with it (M1T01R-03), then the leader is reaped.
    let _ = kill_process_group(pid, Signal::KILL);
    let status = child.wait()?;
    // A descendant that left the group and keeps a pipe open must not stall the probe either. Both reads end at the
    // same absolute end as the wait above, each waiting only for what is left of it, so the two never add up past the
    // limit; output still open then is a timeout and what comes later is not taken (M1T07R-13). The group is not
    // signalled again: it was killed before the leader was reaped, and its id may since belong to another process.
    let read = |rx: mpsc::Receiver<io::Result<Vec<u8>>>| match rx
        .recv_timeout(end.saturating_duration_since(Instant::now()))
    {
        Ok(read) => read.map_err(|e| failed(format!("reading output: {e}"))),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(timed_out()),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(failed(
            "the output reader ended without a result".to_owned(),
        )),
    };
    let stdout = read(rx)?;
    let stderr = read(erx)?;
    Ok(Captured {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let open = params_open(&value);
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
}
