//! The check driver's pre-check (REQ-052 behaviour 4, failure modes 3, 4 and 6; docs/adr/ADR-027.md decision 6 and
//! principle two): before a check's command runs, everything it would be judged by is checked against the frozen
//! candidate, each difference named.

use crate::host::control::git;
use hoh_core::persistence::{PersistError, Reader};
use hoh_core::verification::{Planned, Sealed};
use serde_json::Value;
use std::path::Path;

/// A check to run on a frozen candidate, by its producer attempt (a qa or reviewer attempt of the candidate's run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub candidate_id: String,
    pub check_id: String,
    pub argv: Vec<String>,
    pub producer_attempt_id: String,
}

/// A check cleared to run: its candidate, run and frozen plan entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Ready {
    pub sealed: Sealed,
    pub run_id: String,
    pub planned: Planned,
}

/// Why a check did not run.
#[derive(Debug)]
pub enum CheckError {
    /// The pre-check found differences, each named; the command did not run and no record was written.
    Refused(Vec<String>),
    /// The store failed.
    Store(PersistError),
    /// git or a file could not be read.
    Io(String),
}

/// git's `-z` output as its entries.
fn entries(out: &[u8]) -> Vec<String> {
    out.split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| String::from_utf8_lossy(e).into_owned())
        .collect()
}

/// Check `request` against its frozen candidate in the checkout `checkout`, HoH's state in `state` (the project's
/// `.hoh/`, whose parent holds the objects): every difference is named, none hides another; the candidate's export
/// and test plan are read from the objects the freeze kept, never from working files that may have changed
/// (decision 4, M2T10D-104).
pub fn pre_check(
    reader: &Reader,
    checkout: &Path,
    state: &Path,
    request: &Request,
) -> Result<Ready, CheckError> {
    let io = CheckError::Io;
    let refused = |why: String| CheckError::Refused(vec![why]);
    let id = &request.candidate_id;
    let row = reader
        .candidate(id)
        .map_err(CheckError::Store)?
        .ok_or_else(|| refused(format!("candidate {id} is not frozen in this project")))?;
    let project = state
        .parent()
        .ok_or_else(|| io(format!("{} has no parent", state.display())))?;
    let object = |sha256: &str| hoh_core::objects::get(project, sha256);
    let export = row.source_path.rsplit('/').next().unwrap_or_default();
    let sealed = object(export)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()))
        .and_then(|value| Sealed::from_value(&value))
        .map_err(|e| {
            refused(format!(
                "the manifest of candidate {id} cannot be read as frozen: {e}"
            ))
        })?;
    if &sealed.candidate_id != id {
        return Err(refused(format!(
            "manifest identity: the manifest kept for {id} computes {}",
            sealed.candidate_id
        )));
    }
    let m = &sealed.manifest;
    let mut reasons = Vec::new();
    // The producer: a qa or reviewer attempt of the candidate's run (principle two), checked before anything runs.
    let attempt = &request.producer_attempt_id;
    match reader.producer(attempt).map_err(CheckError::Store)? {
        None => reasons.push(format!("PRODUCER_MISSING: there is no attempt {attempt}")),
        Some(p) => {
            if p.run_id != row.run_id {
                reasons.push(format!(
                    "PRODUCER_MISMATCH: attempt {attempt} is of run {}, candidate {id} of run {}",
                    p.run_id, row.run_id
                ));
            }
            if p.role != "qa" && p.role != "reviewer" {
                reasons.push(format!(
                    "PRODUCER_NOT_INDEPENDENT: attempt {attempt} is a {} attempt; only qa and reviewer attempts \
                     produce evidence",
                    p.role
                ));
            }
        }
    }
    // The checkout: the candidate's commit, nothing uncommitted, every source as frozen (failure modes 1 and 4).
    let git = |args: &[&str]| git(checkout, args).map_err(io);
    let rev = |r: &str| -> Result<String, CheckError> {
        Ok(String::from_utf8_lossy(&git(&["rev-parse", r])?)
            .trim()
            .to_owned())
    };
    let (head, tree) = (rev("HEAD")?, rev("HEAD^{tree}")?);
    if head != m.git_commit || tree != m.git_tree {
        reasons.push(format!(
            "checkout changed: HEAD is {head} (tree {tree}), the candidate's {} (tree {})",
            m.git_commit, m.git_tree
        ));
    }
    for entry in entries(&git(&[
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
    ])?) {
        if let Some(path) = entry.get(3..) {
            reasons.push(format!(
                "worktree dirty: {path} ({})",
                entry.get(..2).unwrap_or("")
            ));
        }
    }
    let tracked: std::collections::BTreeSet<String> =
        entries(&git(&["ls-files", "-z"])?).into_iter().collect();
    let frozen: std::collections::BTreeSet<String> =
        m.source_files.iter().map(|f| f.path.clone()).collect();
    for path in tracked.difference(&frozen) {
        reasons.push(format!(
            "source inventory changed: {path} is tracked but not in the candidate"
        ));
    }
    for path in frozen.difference(&tracked) {
        reasons.push(format!(
            "source inventory changed: {path} is in the candidate but no longer tracked"
        ));
    }
    let now = |path: &str| hoh_core::objects::hash_file(&checkout.join(path)).map(|(sha, _)| sha);
    for f in &m.source_files {
        if now(&f.path).ok().as_deref() != Some(f.sha256.as_str()) {
            reasons.push(format!("source changed: {}", f.path));
        }
    }
    // The specification, environment and test plan as frozen (failure mode 3; the test plan named on its own,
    // AC-052-03 variant 1).
    for (name, file) in [
        ("spec", &m.spec),
        ("environment", &m.environment),
        ("test_plan", &m.test_plan),
    ] {
        if now(&file.path).ok().as_deref() != Some(file.sha256.as_str()) {
            reasons.push(format!("{name}_sha256 mismatch: {}", file.path));
        }
    }
    // The check as the frozen plan has it (failure mode 6), read from the plan object.
    let plan: Value = object(&m.test_plan.sha256)
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
        .map_err(|e| refused(format!("the frozen test plan of {id} cannot be read: {e}")))?;
    let check = &request.check_id;
    let planned = match plan["checks"].get(check.as_str()) {
        None => {
            reasons.push(format!("check_id {check} is not in the frozen plan"));
            None
        }
        Some(entry) => {
            let strings = |v: &Value| -> Vec<String> {
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let planned = Planned {
                argv: strings(&entry["argv"]),
                mode: entry["mode"].as_str().unwrap_or_default().to_owned(),
                cwd: entry["cwd"].as_str().unwrap_or(".").to_owned(),
                artifacts: strings(&entry["artifacts"]),
            };
            if request.argv != planned.argv {
                reasons.push(format!(
                    "argv differs from the frozen plan: {:?} is not {:?}",
                    request.argv, planned.argv
                ));
            }
            if !["mock", "live", "product"].contains(&planned.mode.as_str()) {
                reasons.push(format!(
                    "check {check} is planned in mode {}: the product runs mock, live and product checks only \
                     (a bundle check is the bootstrap tools')",
                    planned.mode
                ));
            }
            Some(planned)
        }
    };
    match planned {
        Some(planned) if reasons.is_empty() => Ok(Ready {
            sealed,
            run_id: row.run_id,
            planned,
        }),
        _ => Err(CheckError::Refused(reasons)),
    }
}
