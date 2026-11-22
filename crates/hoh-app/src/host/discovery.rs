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

/// How often a running test command is looked at (its end, its limit, the host's stop).
const TEST_LOOK: std::time::Duration = std::time::Duration::from_millis(100);

/// Run each of `commands` once in `project` (blocking), as the user confirmed (REQ-028 behaviour 3): `sh -c <command>`
/// with no input, in a process group of its own, its output to a file under `log_dir` (HoH's state, never the
/// project's tree); past [`TEST_LIMIT`], or when `stopping` says the host stops, the group is killed and reaped. Each
/// command's `{command, exit_code, status: pass|fail, failed_tests}`; an error when the host stops or a command cannot
/// be started, and then nothing is recorded (the next tick runs them again).
pub(crate) fn run_tests(
    project: &Path,
    log_dir: &Path,
    commands: &[String],
    stopping: &dyn Fn() -> bool,
) -> Result<Value, String> {
    use rustix::process::{WaitId, WaitIdOptions};
    use std::os::unix::process::CommandExt as _;
    std::fs::create_dir_all(log_dir).map_err(|e| format!("{}: {e}", log_dir.display()))?;
    let mut ran = Vec::new();
    for (n, command) in commands.iter().enumerate() {
        let log = log_dir.join(format!("{n}.log"));
        let out = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("{}: {e}", log.display()))?;
        let mut child = std::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(project)
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0)
            .spawn()
            .map_err(|e| format!("{command}: {e}"))?;
        let group = rustix::process::Pid::from_child(&child);
        let started = std::time::Instant::now();
        // Its end is seen without reaping it, so its pid still names its group for the kill below (as `git_run`).
        let ended: Result<bool, String> = loop {
            match rustix::process::waitid(
                WaitId::Pid(group),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(Some(_)) => break Ok(true),
                Ok(None) if stopping() => break Err(format!("{command}: the host stops")),
                Ok(None) if started.elapsed() > TEST_LIMIT => break Ok(false),
                Ok(None) => std::thread::sleep(TEST_LOOK),
                Err(e) => break Err(format!("{command}: {e}")),
            }
        };
        // What the command started and left in its group ends with it (and the command itself past its limit or on
        // the host's stop); then it is reaped.
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        if !matches!(ended, Ok(true)) {
            let _ = child.kill();
        }
        let waited = child.wait();
        let finished = ended?;
        let status = waited.map_err(|e| format!("{command}: {e}"))?;
        let status = finished.then_some(status);
        let output = hoh_core::objects::read_regular(&log).unwrap_or_default();
        let output = String::from_utf8_lossy(&output);
        let passed = status.is_some_and(|s| s.success());
        ran.push(json!({
            "command": command,
            "exit_code": status.and_then(|s| s.code()),
            "timed_out": status.is_none(),
            "status": if passed { "pass" } else { "fail" },
            "failed_tests": baseline::failed_tests(&output),
        }));
    }
    Ok(json!({"commands": ran}))
}

#[cfg(test)]
mod tests;
