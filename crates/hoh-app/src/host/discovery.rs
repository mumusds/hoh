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
