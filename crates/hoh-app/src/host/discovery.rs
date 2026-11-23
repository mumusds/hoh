//! Project discovery (REQ-028; docs/adr/ADR-039.md decision 4): the host's read-only look at a project for a run in
//! DISCOVERING, written as its `run.baseline`. git runs only through the control loop's read-only helper (no optional
//! lock, no fsmonitor, its own process group, a time limit); files are hashed only when regular; nothing in the
//! project is written, stashed, reset or committed, and no project command runs.

use hoh_core::baseline::{self, Conventions};
use serde_json::{Value, json};
use std::path::Path;

/// The text of `project`'s regular file `name`, if it has one.
fn text(project: &Path, name: &str) -> Option<String> {
    hoh_core::objects::read_regular(&project.join(name))
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// `{path, sha256}` of each of `paths` (sha256 null when it is not a regular file, as a deleted one).
fn hashed(project: &Path, paths: &[String]) -> Vec<Value> {
    paths
        .iter()
        .map(|p| {
            let sha = hoh_core::objects::hash_file(&project.join(p))
                .ok()
                .map(|(sha, _)| sha);
            json!({"path": p, "sha256": sha})
        })
        .collect()
}

/// The baseline of the project at `project` (blocking: git and file reads): `head` and `tree` (null outside a git
/// work tree or before its first commit), its uncommitted tracked and untracked files with their digests, the build
/// and test commands its conventional files name, and its documents. An error when git fails inside a work tree.
pub(crate) fn discover(project: &Path) -> Result<Value, String> {
    let git = |args: &[&str]| super::control::git(project, args);
    // Only `rev-parse` asks whether this is a work tree: it creates nothing where there is none.
    let inside = super::control::git_run(project, &["rev-parse", "--is-inside-work-tree"])
        .is_ok_and(|(status, out)| status.success() && out.starts_with(b"true"));
    let (head, tree, status) = if inside {
        let name = |args: &[&str]| -> Option<String> {
            let (status, out) = super::control::git_run(project, args).ok()?;
            let out = String::from_utf8_lossy(&out).trim().to_owned();
            (status.success() && !out.is_empty()).then_some(out)
        };
        (
            name(&["rev-parse", "--verify", "-q", "HEAD"]),
            name(&["rev-parse", "--verify", "-q", "HEAD^{tree}"]),
            baseline::status(&git(&[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
            ])?),
        )
    } else {
        (None, None, baseline::Status::default())
    };
    let (build, test) = baseline::commands(&Conventions {
        cargo_toml: text(project, "Cargo.toml"),
        package_json: text(project, "package.json"),
        makefile: text(project, "Makefile"),
        pyproject_toml: text(project, "pyproject.toml"),
    });
    let root: Vec<String> = std::fs::read_dir(project)
        .map_err(|e| format!("{}: {e}", project.display()))?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let dirs: Vec<&str> = ["docs", "docs/adr", "adr"]
        .into_iter()
        .filter(|d| {
            std::fs::symlink_metadata(project.join(d)).is_ok_and(|m| m.file_type().is_dir())
        })
        .collect();
    Ok(json!({
        "head": head,
        "tree": tree,
        "dirty": hashed(project, &status.dirty),
        "untracked": hashed(project, &status.untracked),
        "build_commands": build,
        "test_commands": test,
        "docs": baseline::docs(&root, &dirs),
    }))
}

/// How long one of the baseline's test commands may run (docs/adr/ADR-039.md decision 8).
const TEST_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How often a running test command is looked at (its end, its limit, its owner's stop).
const TEST_LOOK: std::time::Duration = std::time::Duration::from_millis(100);

/// How long a test command's group may take to empty once its leader is reaped (orphans reaped by their new parent).
const GROUP_EMPTY: std::time::Duration = std::time::Duration::from_secs(5);

/// The start gate of a baseline test command (docs/adr/ADR-039.md decision 8, DC-T07 (1)): `sh -c GATE hoh-baseline
/// <command>` reads one line from its stdin and only on `go` runs the command in its place (same pid, same group),
/// its stdin then empty; any other line or a closed stdin ends it with nothing of the project run.
const GATE: &str =
    r#"IFS= read -r go && [ "$go" = go ] || exit 125; exec </dev/null; exec sh -c "$1""#;

/// The owner of a run's baseline test commands while they run (DC-T07): the host's control loop for that run.
pub(crate) trait TestOwner {
    /// Whether the commands must stop now (the host stops, or the run is cancelled or paused).
    fn stopping(&self) -> bool;
    /// Record command `index`'s process (its gate still closed) with its log, project-relative: its record's id, none
    /// when the run no longer owns its tests (the command must not run), an error when it could not be recorded.
    fn started(
        &self,
        index: usize,
        nonce: &str,
        command: &str,
        identity: &hoh_agents::identity::Identity,
        log: &str,
    ) -> Result<Option<String>, String>;
    /// Record that the process of record `process` was reaped and its group seen empty.
    fn ended(&self, process: &str);
}

/// Run each of `commands` once in `project` (blocking), as the user confirmed (REQ-028 behaviour 3; docs/adr/ADR-039.md
/// decision 8, DC-T07): `sh -c <command>` behind a start gate, in a process group of its own, its output to a new file
/// under `log_dir` (project-relative, HoH's state, never the project's tree; never an existing file). Its identity is
/// recorded through `owner` before the gate opens; past [`TEST_LIMIT`], or when the owner stops it, its group is killed
/// and reaped and its group seen empty. Each command's `{command, exit_code, timed_out, incomplete, status: pass|fail,
/// failed_tests, log}`; an error when the owner stopped it, the run no longer owns it, or a command could not be
/// started, and then nothing is recorded as the run's tests.
pub(crate) fn run_tests(
    project: &Path,
    log_dir: &Path,
    commands: &[String],
    owner: &dyn TestOwner,
) -> Result<Value, String> {
    use rustix::process::{WaitId, WaitIdOptions};
    use std::io::Write as _;
    use std::os::unix::process::CommandExt as _;
    let dir = project.join(log_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut ran = Vec::new();
    for (n, command) in commands.iter().enumerate() {
        let nonce = super::new_id();
        let log_rel = log_dir.join(format!("{n}-{nonce}.log"));
        let log = project.join(&log_rel);
        let log_text = log_rel.to_string_lossy().into_owned();
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log)
            .map_err(|e| format!("{}: {e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("{}: {e}", log.display()))?;
        let mut child = std::process::Command::new("sh")
            .args(["-c", GATE, "hoh-baseline", command])
            .current_dir(project)
            .stdin(std::process::Stdio::piped())
            .stdout(out)
            .stderr(err)
            .process_group(0)
            .spawn()
            .map_err(|e| format!("{command}: {e}"))?;
        let group = rustix::process::Pid::from_child(&child);
        let gate = child.stdin.take();
        // Held at its gate: closed unopened, it ends without running anything of the project.
        let closed = |mut child: std::process::Child, why: String| -> Result<Value, String> {
            let _ = child.wait();
            Err(why)
        };
        let recorded = match hoh_agents::identity::of(child.id()) {
            Ok(Some(identity)) => owner.started(n, &nonce, command, &identity, &log_text),
            Ok(None) => Err(format!("{command}: its process has no identity to record")),
            Err(e) => Err(format!("{command}: its identity cannot be read: {e}")),
        };
        let process = match recorded {
            Ok(Some(process)) => process,
            Ok(None) => {
                drop(gate);
                return closed(
                    child,
                    format!(
                        "{command}: not run: the run no longer owns its baseline tests (paused, cancelled, ended, or \
                         its tests recorded)"
                    ),
                );
            }
            Err(e) => {
                drop(gate);
                return closed(child, e);
            }
        };
        let opened = gate
            .ok_or_else(|| "no gate".to_owned())
            .and_then(|mut g| g.write_all(b"go\n").map_err(|e| e.to_string()));
        let started = std::time::Instant::now();
        // Its end is seen without reaping it, so its pid still names its group for the kill below (as `git_run`).
        let ended: Result<bool, String> = match opened {
            Err(e) => Err(format!("{command}: its gate did not open: {e}")),
            Ok(()) => loop {
                match rustix::process::waitid(
                    WaitId::Pid(group),
                    WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                ) {
                    Ok(Some(_)) => break Ok(true),
                    Ok(None) if owner.stopping() => {
                        break Err(format!("{command}: stopped by its run or the host"));
                    }
                    Ok(None) if started.elapsed() > TEST_LIMIT => break Ok(false),
                    Ok(None) => std::thread::sleep(TEST_LOOK),
                    Err(e) => break Err(format!("{command}: {e}")),
                }
            },
        };
        // What the command started and left in its group ends with it (and the command itself past its limit or when
        // stopped); then it is reaped, and its group must be seen empty before its end is recorded (DC-T07 (2)).
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        if !matches!(ended, Ok(true)) {
            let _ = child.kill();
        }
        let waited = child.wait();
        let deadline = std::time::Instant::now() + GROUP_EMPTY;
        let empty = loop {
            let raw = u32::try_from(group.as_raw_nonzero().get()).unwrap_or_default();
            if hoh_agents::identity::group_empty(raw) {
                break true;
            }
            if std::time::Instant::now() > deadline {
                break false;
            }
            // Members left hold the group's number: it names no other group meanwhile.
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        if empty {
            owner.ended(&process);
        }
        let finished = ended?;
        if !empty {
            return Err(format!(
                "{command}: its process group was not seen empty; its record stays open"
            ));
        }
        let status = waited.map_err(|e| format!("{command}: {e}"))?;
        let status = finished.then_some(status);
        let output = hoh_core::objects::read_regular(&log);
        let incomplete = status.is_none() || output.is_err();
        let output = output.unwrap_or_default();
        let output = String::from_utf8_lossy(&output);
        let passed = status.is_some_and(|s| s.success());
        ran.push(json!({
            "command": command,
            "exit_code": status.and_then(|s| s.code()),
            "timed_out": status.is_none(),
            "incomplete": incomplete,
            "status": if passed { "pass" } else { "fail" },
            "failed_tests": baseline::failed_tests(&output),
            "log": log_text,
        }));
    }
    Ok(json!({"commands": ran}))
}

#[cfg(test)]
mod tests;
