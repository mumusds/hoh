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
}

/// Why a probe failed.
#[derive(Debug)]
pub enum ProbeError {
    /// The CLI is not on PATH.
    NotFound {
        agent: Agent,
    },
    /// A probe command could not be run, failed, or timed out.
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

/// Probe one agent: the entry it would get in the compat manifest (suite not run).
pub fn probe(agent: Agent, env: &ProbeEnv) -> Result<AgentEntry, ProbeError> {
    let launcher =
        find_on_path(agent.command(), &env.path).ok_or(ProbeError::NotFound { agent })?;
    let implementation = resolve_implementation(agent, &launcher)?;
    let cli_version = version_line(&launcher, env)?;
    let mut capabilities: BTreeMap<String, Option<bool>> = CAPABILITIES
        .iter()
        .map(|c| ((*c).to_owned(), None))
        .collect();
    let (transport, help, schema_sha256) = match agent {
        Agent::Codex => probe_codex(&launcher, env, &mut capabilities)?,
        Agent::Claude => probe_claude(&launcher, env, &mut capabilities)?,
        Agent::Pi => probe_pi(&launcher, env, &mut capabilities)?,
    };
    Ok(AgentEntry {
        launcher_sha256: file_sha256(&launcher)?,
        launcher_path: launcher.display().to_string(),
        implementation_sha256: file_sha256(&implementation)?,
        implementation_path: implementation.display().to_string(),
        suite: Suite {
            status: SuiteStatus::NotRun,
            fixture_set_sha256: fixture_set_sha256(env, agent, &cli_version)?,
            run_at: None,
            cases: None,
        },
        cli_version,
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

/// Codex: App Server when `codex app-server --help` works (its schema decides steer/resume/deltas), else `exec --json`.
fn probe_codex(
    launcher: &Path,
    env: &ProbeEnv,
    caps: &mut BTreeMap<String, Option<bool>>,
) -> Result<Probed, ProbeError> {
    let exec_help = run_ok(launcher, &["exec", "--help"], env)?.stdout;
    let app = run(launcher, &["app-server", "--help"], env)?;
    let mut help = exec_help.clone();
    if !app.status.success() {
        if !contains(&exec_help, "--json") {
            return Err(ProbeError::Unsupported {
                agent: Agent::Codex,
                detail: "neither app-server nor exec --json".to_owned(),
            });
        }
        set(caps, "structured_events", true);
        set(caps, "resume", contains(&exec_help, "resume"));
        set(caps, "mid_turn_steer", false);
        set(caps, "partial_messages", false);
        return Ok((Transport::Exec, help, None));
    }
    help.extend_from_slice(&app.stdout);
    let (schema_hash, schema_text) = codex_schema(launcher, env)?;
    set(caps, "structured_events", true);
    set(caps, "resume", contains(&schema_text, "\"thread/resume\""));
    set(
        caps,
        "mid_turn_steer",
        contains(&schema_text, "\"turn/steer\""),
    );
    set(
        caps,
        "partial_messages",
        contains(&schema_text, "item/agentMessage/delta"),
    );
    set(
        caps,
        "token_usage",
        contains(&schema_text, "thread/tokenUsage/updated"),
    );
    Ok((Transport::AppServer, help, Some(schema_hash)))
}

/// `codex app-server generate-json-schema --out <dir>`: the tree hash of the output and its concatenated text.
fn codex_schema(launcher: &Path, env: &ProbeEnv) -> Result<(String, Vec<u8>), ProbeError> {
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
        // A command that exits 0 without writing a usable schema has not provided one (M1T01R-08).
        let unusable = |detail: String| ProbeError::Failed {
            argv: argv(launcher, &["app-server", "generate-json-schema"]),
            detail,
        };
        let files = files_under(&dir)?;
        if !dir.join("ClientRequest.json").is_file() {
            return Err(unusable(
                "no ClientRequest.json in the schema output".to_owned(),
            ));
        }
        let mut text = Vec::new();
        for file in &files {
            let bytes = std::fs::read(file)?;
            if file.extension().is_some_and(|e| e == "json")
                && serde_json::from_slice::<serde_json::Value>(&bytes).is_err()
            {
                return Err(unusable(format!("{} is not JSON", file.display())));
            }
            text.extend(bytes);
        }
        Ok((tree_sha256(&dir)?, text))
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
    if !contains(&help, "stream-json") {
        return Err(ProbeError::Unsupported {
            agent: Agent::Claude,
            detail: "no stream-json output format".to_owned(),
        });
    }
    set(caps, "structured_events", true);
    set(
        caps,
        "partial_messages",
        contains(&help, "--include-partial-messages"),
    );
    set(caps, "resume", contains(&help, "--resume"));
    Ok((Transport::StreamJson, help, None))
}

/// Pi: `--mode rpc`; resume from the session options in `--help`.
fn probe_pi(
    launcher: &Path,
    env: &ProbeEnv,
    caps: &mut BTreeMap<String, Option<bool>>,
) -> Result<Probed, ProbeError> {
    let help = run_ok(launcher, &["--help"], env)?.stdout;
    if !(contains(&help, "--mode") && contains(&help, "rpc")) {
        return Err(ProbeError::Unsupported {
            agent: Agent::Pi,
            detail: "no --mode rpc".to_owned(),
        });
    }
    set(caps, "structured_events", true);
    set(
        caps,
        "resume",
        contains(&help, "--session-id") || contains(&help, "--resume"),
    );
    Ok((Transport::Rpc, help, None))
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
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
        Agent::Codex => codex_native(&real),
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
fn node_resolve(start: &Path, package: &str) -> Option<PathBuf> {
    start
        .ancestors()
        .filter(|dir| dir.file_name() != Some(OsStr::new("node_modules")))
        .map(|dir| dir.join("node_modules").join(package))
        .find(|candidate| candidate.join("package.json").is_file())
}

/// The native binary `codex.js` (at its real path `launcher`) executes: `vendor/<triple>/bin/codex` in the platform
/// package it resolves, else in the codex package's own `vendor` directory (M1T01R-02).
fn codex_native(launcher: &Path) -> Option<PathBuf> {
    let (triple, package) = codex_target()?;
    let start = launcher.parent()?;
    let vendor = match node_resolve(start, package) {
        Some(dir) => std::fs::canonicalize(dir).ok()?.join("vendor"),
        None => start.parent()?.join("vendor"),
    };
    let native = vendor.join(triple).join("bin/codex");
    native.is_file().then_some(native)
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

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
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
    let failed = |detail: String| ProbeError::Failed {
        argv: argv(program, args),
        detail,
    };
    let mut child = Command::new(program)
        .args(args)
        .env("PATH", &env.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| failed(format!("cannot start: {e}")))?;
    let (tx, rx) = mpsc::channel();
    if let Some(mut stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = tx.send(stdout.read_to_end(&mut buf).map(|_| buf));
        });
    }
    let deadline = Instant::now() + env.timeout;
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
            return Err(failed(format!("timed out after {:?}", env.timeout)));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Whatever the command left running in its group goes with it (M1T01R-03), then the leader is reaped.
    let _ = kill_process_group(pid, Signal::KILL);
    let status = child.wait()?;
    // A descendant that keeps the pipe open must not stall the probe either.
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(500));
    let stdout = match rx.recv_timeout(remaining) {
        Ok(read) => read.map_err(|e| failed(format!("reading output: {e}")))?,
        Err(_) => {
            let _ = kill_process_group(Pid::from_child(&child), Signal::KILL);
            return Err(failed(
                "output stayed open after the command exited".to_owned(),
            ));
        }
    };
    Ok(Captured { status, stdout })
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
