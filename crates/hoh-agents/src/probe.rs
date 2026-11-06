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

/// The draft-07 `$schema` the App Server schema files declare.
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

/// Draft-07 well-formedness for the keyword set the App Server schema uses (recorded 0.166.0: `title`,
/// `description`, `type`, `enum`, `properties`, `required`, `$ref`, `oneOf`, `anyOf`, `allOf`, `items`,
/// `additionalProperties`, `format`, `minimum`, `minLength`, `default`, and `$schema`/`definitions` at the root).
/// Every schema position is checked against the draft-07 meta-schema rule for its keyword; any other keyword, an
/// ill-typed value, or a `$ref` other than `#/definitions/<name>` naming a root definition refuses the document, so a
/// newer schema form fails the probe instead of being half-read (M1T01R-25, -26, -27). `minLength` must be written as
/// an integer literal (M1T01R-28).
fn draft7_well_formed(root: &serde_json::Value) -> Result<(), String> {
    use serde_json::{Map, Value};
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
    fn schema(v: &Value, defs: &Map<String, Value>, path: &str) -> Result<(), String> {
        let obj = match v {
            Value::Bool(_) => return Ok(()),
            Value::Object(obj) => obj,
            _ => return Err(format!("{path}: a schema must be an object or a boolean")),
        };
        for (key, v) in obj {
            let at = format!("{path}/{key}");
            let ok = match key.as_str() {
                "$schema" => path.is_empty() && v == DRAFT_07,
                "definitions" => path.is_empty(),
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
                            schema(s, defs, &format!("{at}/{name}"))?;
                        }
                        true
                    }
                    None => false,
                },
                "items" | "additionalProperties" => {
                    schema(v, defs, &at)?;
                    true
                }
                "oneOf" | "anyOf" | "allOf" => match v.as_array().filter(|a| !a.is_empty()) {
                    Some(list) => {
                        for (i, s) in list.iter().enumerate() {
                            schema(s, defs, &format!("{at}/{i}"))?;
                        }
                        true
                    }
                    None => false,
                },
                _ => return Err(format!("{at}: unsupported keyword")),
            };
            if !ok {
                return Err(format!("{at}: ill-formed value"));
            }
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
        schema(s, defs, &format!("/definitions/{name}"))?;
    }
    schema(root, defs, "")
}

/// The `method` names a well-formed protocol schema file (see [`draft7_well_formed`]) lists, read only from the form
/// the App Server schema uses: a root `oneOf` (with `$schema`, `definitions`, `properties`, `title`, `description`
/// beside it) whose every branch is `{type: "object", required, properties: {method: {type: "string", enum:
/// [names]}, ...}}` with optional `title`/`description`. Any other shape rejects the whole file, so malformed output
/// never yields a capability (M1T01R-15, -18, -23, -27).
fn schema_methods(value: &serde_json::Value) -> Option<std::collections::BTreeSet<String>> {
    use serde_json::Value;
    let root = value.as_object()?;
    let root_ok = root.keys().all(|k| {
        matches!(
            k.as_str(),
            "$schema" | "definitions" | "properties" | "title" | "description" | "oneOf"
        )
    });
    let variants = root.get("oneOf")?.as_array().filter(|v| !v.is_empty())?;
    let mut methods = std::collections::BTreeSet::new();
    for branch in variants {
        let branch = branch.as_object()?;
        let branch_ok = branch.iter().all(|(key, v)| match key.as_str() {
            "type" => v == "object",
            "required" | "properties" | "title" | "description" => true,
            _ => false,
        });
        let method = branch.get("properties")?.get("method")?.as_object()?;
        let method_ok = method.get("type").is_some_and(|t| t == "string")
            && method
                .keys()
                .all(|k| matches!(k.as_str(), "type" | "enum" | "title" | "description"));
        let names = method.get("enum")?.as_array().filter(|a| !a.is_empty())?;
        if !root_ok || !branch_ok || !method_ok || !names.iter().all(Value::is_string) {
            return None;
        }
        methods.extend(names.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    Some(methods)
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
        let mut parsed = BTreeMap::new();
        for file in files_under(&dir)? {
            if file.extension().is_some_and(|e| e == "json") {
                let value = serde_json::from_slice::<serde_json::Value>(&std::fs::read(&file)?)
                    .map_err(|e| unusable(format!("{} is not JSON: {e}", file.display())))?;
                parsed.insert(file, value);
            }
        }
        let methods = |name: &str| {
            let value = parsed
                .get(&dir.join(name))
                .ok_or_else(|| unusable(format!("{name} is missing")))?;
            draft7_well_formed(value)
                .map_err(|e| unusable(format!("{name} is not a well-formed schema: {e}")))?;
            schema_methods(value).ok_or_else(|| {
                unusable(format!(
                    "{name} does not list its methods in the known form"
                ))
            })
        };
        Ok(CodexSchema {
            requests: methods("ClientRequest.json")?,
            client_notifications: methods("ClientNotification.json")?,
            notifications: methods("ServerNotification.json")?,
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
}
