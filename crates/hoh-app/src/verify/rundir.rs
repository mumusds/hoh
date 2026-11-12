//! A verifier attempt's run directory (docs/adr/ADR-028.md decisions 6 and 8; REQ-037 behaviours 3 and 5): a checkout
//! of the candidate outside the project tree, so the agent finds no writer's workspace above it, prepared and checked
//! before the process starts, and checked again on every end path before the attempt is settled.

use crate::host::control::{git, workspace};
use hoh_core::verification::{Manifest, Sealed};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Why a run directory could not be prepared: every difference from the candidate found, or what could not be done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unprepared {
    Refused(Vec<String>),
    Io(String),
}

impl std::fmt::Display for Unprepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unprepared::Refused(reasons) => {
                write!(
                    f,
                    "the run directory is not the candidate: {}",
                    reasons.join("; ")
                )
            }
            Unprepared::Io(e) => write!(f, "the run directory could not be prepared: {e}"),
        }
    }
}

/// Where run directories go: beside the Commander's session directories (`<state>/hoh/runs` for
/// `<state>/hoh/commander`, decision 6), else under `$XDG_STATE_HOME/hoh/runs` (`~/.local/state` when unset).
pub fn root(commander_dir: Option<&Path>) -> PathBuf {
    if let Some(parent) = commander_dir.and_then(Path::parent) {
        return parent.join("runs");
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    state.join("hoh/runs")
}

/// Attempt `attempt_id`'s run directory of project `project_id` under `root`.
pub fn dir_of(root: &Path, project_id: &str, attempt_id: &str) -> PathBuf {
    let component = |id: &str| id.replace('%', "%25").replace('/', "%2F");
    root.join(component(project_id)).join(component(attempt_id))
}

/// The candidate `sealed`'s checkout at `dir`, from the project at `project` (its objects hold the candidate's frozen
/// files): a shared clone (no object copied, nothing of the project's repository written) detached at the candidate's
/// commit, its `.hoh/` ignored with the frozen environment written where the manifest names it, then the whole
/// pre-check of ADR-027 decision 6 (commit, tree, clean, inventory, every digest). Its workspace hash, the attempt's
/// `pre_tree`. Blocking.
pub fn prepare(project: &Path, dir: &Path, sealed: &Sealed) -> Result<Value, Unprepared> {
    let io = Unprepared::Io;
    let m = &sealed.manifest;
    if std::fs::symlink_metadata(dir).is_ok() {
        std::fs::remove_dir_all(dir).map_err(|e| io(format!("{}: {e}", dir.display())))?;
    }
    let parent = dir
        .parent()
        .ok_or_else(|| io(format!("{} has no parent", dir.display())))?;
    std::fs::create_dir_all(parent).map_err(|e| io(format!("{}: {e}", parent.display())))?;
    let target = dir.to_string_lossy().into_owned();
    git(
        project,
        &[
            "clone",
            "--shared",
            "--no-checkout",
            "--quiet",
            ".",
            &target,
        ],
    )
    .map_err(io)?;
    git(dir, &["checkout", "--quiet", "--detach", &m.git_commit]).map_err(io)?;
    hoh_core::state_dir::ensure(&dir.join(".hoh"))
        .map_err(|e| io(format!("{}/.hoh: {e}", dir.display())))?;
    // The frozen environment, when it is HoH's ignored file rather than a tracked one.
    if m.environment.path.starts_with(".hoh/") {
        let bytes = hoh_core::objects::get(project, &m.environment.sha256).map_err(io)?;
        let file = dir.join(&m.environment.path);
        std::fs::create_dir_all(file.parent().unwrap_or(dir))
            .and_then(|()| std::fs::write(&file, bytes))
            .map_err(|e| io(format!("{}: {e}", file.display())))?;
    }
    let reasons = crate::verify::check::differences(dir, m)
        .map_err(|e| io(crate::verify::check::described(&e)))?;
    if !reasons.is_empty() {
        return Err(Unprepared::Refused(reasons));
    }
    Ok(workspace(dir))
}

/// The run directory looked at again (decision 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeCheck {
    pub post_tree: Value,
    /// The workspace is as it was prepared and still the candidate's commit and tree.
    pub equal: bool,
    /// `git status --porcelain`'s first 4 KiB, for whoever looks.
    pub status: String,
}

/// The run directory `dir` against its `pre_tree` and the candidate `m`: a workspace that cannot be read is not equal.
/// Blocking.
pub fn post(dir: &Path, pre_tree: &Value, m: &Manifest) -> TreeCheck {
    let post_tree = workspace(dir);
    let candidate = post_tree["head"] == json!(m.git_commit)
        && post_tree["head_tree"] == json!(m.git_tree)
        && post_tree.get("error").is_none();
    let status = git(dir, &["status", "--porcelain"])
        .map(|out| {
            let mut out = out;
            out.truncate(4096);
            String::from_utf8_lossy(&out).into_owned()
        })
        .unwrap_or_else(|e| format!("git status: {e}"));
    TreeCheck {
        equal: candidate && post_tree == *pre_tree,
        post_tree,
        status,
    }
}

/// Candidate `candidate_id`'s sealed manifest as the freeze kept it (an object of the project at `project`) and its run.
/// Blocking.
pub fn sealed_of(
    reader: &hoh_core::persistence::Reader,
    project: &Path,
    candidate_id: &str,
) -> Result<(Sealed, String), String> {
    let row = reader
        .candidate(candidate_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("candidate {candidate_id} is not frozen in this project"))?;
    let export = row.source_path.rsplit('/').next().unwrap_or_default();
    let sealed = hoh_core::objects::get(project, export)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()))
        .and_then(|value| Sealed::from_value(&value))?;
    if sealed.candidate_id != candidate_id {
        return Err(format!(
            "the manifest kept for {candidate_id} computes {}",
            sealed.candidate_id
        ));
    }
    Ok((sealed, row.run_id))
}

/// The check a verifier's attempt-level record names (decision 8).
pub fn check_of(role: &str) -> &'static str {
    if role == "reviewer" {
        "independent-review"
    } else {
        "independent-qa"
    }
}

/// Remove the run directory `dir` (after its attempt is settled); one already gone is fine.
pub fn remove(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(test)]
mod tests;
