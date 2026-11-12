//! The freeze driver (REQ-052 behaviours 1 to 3, failure modes 1 and 5; docs/adr/ADR-027.md decisions 3 and 4).

use crate::host::control::{git, git_run};
use hoh_core::persistence::{ArtifactRow, Freeze, Frozen, PersistError, Store};
use hoh_core::verification::{FileRef, Manifest};
use std::path::{Path, PathBuf};

/// The files a candidate binds besides its sources, relative to the checkout: the specification and the test plan are
/// tracked files of it; the environment is a tracked file or a regular file under `.hoh/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    pub spec: PathBuf,
    pub environment: PathBuf,
    pub test_plan: PathBuf,
}

/// Why a freeze did not happen.
#[derive(Debug)]
pub enum FreezeError {
    /// The checkout cannot be frozen as it is; every reason found, each naming its path.
    Refused(Vec<String>),
    /// The store refused or failed.
    Store(PersistError),
    /// git or a file could not be read.
    Io(String),
}

impl std::fmt::Display for FreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreezeError::Refused(reasons) => {
                write!(f, "the checkout cannot be frozen: {}", reasons.join("; "))
            }
            FreezeError::Store(e) => write!(f, "{e}"),
            FreezeError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FreezeError {}

/// The release policy a candidate may track (docs/adr/ADR-027.md principle one).
pub(crate) const RELEASE_POLICY: &str = "contracts/release-policy.json";

/// The paths HoH writes under `.hoh/` (docs/adr/ADR-027.md decision 3): each must be ignored and untracked.
const STATE_PATHS: &[&str] = &[
    "state.db",
    "state.db-wal",
    "state.db-shm",
    "objects",
    "evidence",
    "locks",
    "pending",
    "attempts",
    "sessions",
    "first-inputs",
    "host.json",
    "host.lock",
    "host.log",
    "host.sock",
    "input-history.jsonl",
    // A verification's writes (docs/adr/ADR-028.md decision 17).
    "environment.json",
    "self-tests",
];

/// What a freeze keeps before its one store transaction.
struct Prepared {
    sealed: hoh_core::verification::Sealed,
    export: ArtifactRow,
    inputs: Vec<ArtifactRow>,
}

/// Freeze the checkout `checkout` (the top level of a git work tree) for run `run_id`, HoH's state in `state` (the
/// project's `.hoh/`, whose parent the objects go under): refused, every reason named, when it is not committed as it
/// is (an uncommitted or untracked file, an index flag hiding a change, a tracked link or submodule, a tracked path
/// that is not a regular file), when HoH's own state is tracked or not ignored, or when an input is not what it must
/// be; otherwise sealed, its export and inputs kept as objects, and frozen in one store transaction.
pub async fn freeze(
    store: &Store,
    checkout: &Path,
    state: &Path,
    run_id: &str,
    inputs: &Inputs,
) -> Result<Frozen, FreezeError> {
    let (checkout, state, inputs, run) = (
        checkout.to_owned(),
        state.to_owned(),
        inputs.clone(),
        run_id.to_owned(),
    );
    let job = tokio::task::spawn_blocking(move || prepare_job(&checkout, &state, &run, &inputs))
        .await
        .map_err(|e| FreezeError::Io(format!("the freeze stopped: {e}")))??;
    store
        .freeze_candidate(job)
        .await
        .map_err(FreezeError::Store)
}

/// [`freeze`]'s file side for a caller that commits its store side in a transaction of its own (a writer attempt's
/// completion, docs/adr/ADR-028.md decision 4): the same refusals, the export and inputs kept as objects, the job its
/// transaction freezes. Blocking: run it off the async threads.
pub fn prepare_job(
    checkout: &Path,
    state: &Path,
    run_id: &str,
    inputs: &Inputs,
) -> Result<Freeze, FreezeError> {
    let prepared = prepare(checkout, state, inputs)?;
    Ok(Freeze {
        run_id: run_id.to_owned(),
        sealed: prepared.sealed,
        export: prepared.export,
        inputs: prepared.inputs,
    })
}

/// git's `-z` output as its entries.
fn entries(out: &[u8]) -> Vec<String> {
    out.split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| String::from_utf8_lossy(e).into_owned())
        .collect()
}

/// A path the caller gave, relative to the checkout, as the manifest writes it; `None` when it leaves the checkout.
fn relative(path: &Path) -> Option<String> {
    use std::path::Component;
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str()?.to_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Files HoH writes inside its state directories, as samples of each (a pattern may re-include what lies below a
/// directory without the directory itself).
const NESTED: &[&str] = &[
    "objects/sha256/00/0000000000000000000000000000000000000000000000000000000000000000",
    "evidence/CANDIDATE/CHECK/RECORD/execution.json",
    "evidence/CANDIDATE/CHECK/RECORD/artifacts/test-results.jsonl",
    "locks/RECORD.lock",
    "pending/RECORD.json",
    "attempts/ATTEMPT/stdout.jsonl",
    "sessions/SESSION/notes.md",
    "first-inputs/INPUT.json",
    "self-tests/ATTEMPT/report.txt",
    "attempts/ATTEMPT/requests/1.json",
    "attempts/ATTEMPT/run-check",
    "attempts/ATTEMPT/ask",
];

/// Every way HoH's own state under `state` is visible to git in the checkout `checkout` (docs/adr/ADR-027.md decision 3):
/// each path HoH writes that is tracked or not ignored, by name: its fixed names, files inside its directories, and
/// `writes`, the paths (relative to `state`) the caller is about to write. The freeze and every check's pre-check run
/// it (M2T10R-06): a `.hoh/.gitignore` changed after the freeze would otherwise let the check's own record dirty the
/// checkout and veto a clean candidate.
pub(crate) fn state_guard(
    checkout: &Path,
    state: &Path,
    writes: &[String],
) -> Result<Vec<String>, String> {
    let canonical =
        |p: &Path| std::fs::canonicalize(p).map_err(|e| format!("{}: {e}", p.display()));
    let git = |args: &[&str]| git(checkout, args);
    let mut reasons = Vec::new();
    // HoH's own state: ignored and untracked, each path by name (decision 3).
    if let Ok(inside) = canonical(state)?.strip_prefix(canonical(checkout)?) {
        let paths: Vec<String> = STATE_PATHS
            .iter()
            .chain(NESTED)
            .map(|p| (*p).to_owned())
            .chain(writes.iter().cloned())
            .map(|p| format!("{}/{p}", inside.to_string_lossy()))
            .collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut ls = vec!["ls-files", "-z", "--"];
        ls.extend(&refs);
        for tracked in entries(&git(&ls)?) {
            reasons.push(format!("{tracked} is tracked: HoH's own state must not be"));
        }
        // `-z` needs `--stdin`; these paths are HoH's own fixed names, one per line.
        let mut check = vec!["check-ignore", "--"];
        check.extend(&refs);
        let (status, out) = git_run(checkout, &check)?;
        if status.code() != Some(0) && status.code() != Some(1) {
            return Err(format!("git check-ignore: {status}"));
        }
        let ignored: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_owned)
            .collect();
        for p in &paths {
            if !ignored.contains(p) {
                reasons.push(format!(
                    "{p} is not ignored by git: HoH's own state must be"
                ));
            }
        }
    }
    Ok(reasons)
}

fn prepare(checkout: &Path, state: &Path, inputs: &Inputs) -> Result<Prepared, FreezeError> {
    let io = |e: String| FreezeError::Io(e);
    hoh_core::state_dir::ensure(state).map_err(|e| io(format!("{}: {e}", state.display())))?;
    let git = |args: &[&str]| git(checkout, args).map_err(io);
    let top = String::from_utf8_lossy(&git(&["rev-parse", "--show-toplevel"])?)
        .trim()
        .to_owned();
    let canonical =
        |p: &Path| std::fs::canonicalize(p).map_err(|e| io(format!("{}: {e}", p.display())));
    if canonical(Path::new(&top))? != canonical(checkout)? {
        return Err(FreezeError::Refused(vec![format!(
            "{} is not the top level of its git work tree ({top})",
            checkout.display()
        )]));
    }
    let mut reasons = state_guard(checkout, state, &[]).map_err(io)?;
    // Committed as it is (REQ-052 behaviour 1, failure modes 1 and 5; decision 4).
    let status = entries(&git(&[
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
    ])?);
    for entry in &status {
        if let Some(path) = entry.get(3..) {
            reasons.push(format!(
                "{path} is not committed as it is ({})",
                entry.get(..2).unwrap_or("")
            ));
        }
    }
    for entry in entries(&git(&["ls-files", "-v", "-z"])?) {
        let (tag, path) = entry.split_at(entry.find(' ').unwrap_or(0));
        let path = path.trim_start();
        if tag == "S" {
            reasons.push(format!(
                "{path} is marked skip-worktree: a change to it would be hidden"
            ));
        } else if tag.chars().all(|c| c.is_ascii_lowercase()) && !tag.is_empty() {
            reasons.push(format!(
                "{path} is marked assume-unchanged: a change to it would be hidden"
            ));
        }
    }
    let mut tracked = Vec::new();
    for entry in entries(&git(&["ls-files", "-s", "-z"])?) {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        match meta.split(' ').next() {
            Some("120000") => reasons.push(format!(
                "{path} is a symbolic link: links are not frozen (REQ-052 decision 2)"
            )),
            Some("160000") => reasons.push(format!(
                "{path} is a submodule: submodules are not frozen (REQ-052 decision 2)"
            )),
            _ => match std::fs::symlink_metadata(checkout.join(path)) {
                Ok(m) if m.file_type().is_file() => tracked.push(path.to_owned()),
                Ok(_) => reasons.push(format!("{path} is not a regular file")),
                // A deleted tracked file is in the status already.
                Err(_) => {}
            },
        }
    }
    tracked.sort();
    tracked.dedup();
    if tracked.is_empty() && reasons.is_empty() {
        reasons.push("the checkout tracks no file".to_owned());
    }
    // The inputs: the specification and test plan tracked, the environment tracked or a regular file under `.hoh/`.
    let state_inside = canonical(state)?
        .strip_prefix(canonical(checkout)?)
        .ok()
        .map(Path::to_path_buf);
    let mut named = Vec::new();
    for (what, path, tracked_only) in [
        ("spec", &inputs.spec, true),
        ("environment", &inputs.environment, false),
        ("test plan", &inputs.test_plan, true),
    ] {
        let Some(rel) = relative(path) else {
            reasons.push(format!(
                "the {what} {} is not a path inside the checkout",
                path.display()
            ));
            continue;
        };
        let regular =
            std::fs::symlink_metadata(checkout.join(&rel)).is_ok_and(|m| m.file_type().is_file());
        let ours = state_inside
            .as_ref()
            .is_some_and(|s| Path::new(&rel).starts_with(s));
        if tracked.binary_search(&rel).is_ok() || (!tracked_only && regular && ours) {
            named.push((what, rel));
        } else if tracked_only {
            reasons.push(format!(
                "the {what} {rel} is not a tracked file of the candidate"
            ));
        } else {
            reasons.push(format!(
                "the {what} {rel} is neither a tracked file nor a regular file under HoH's state"
            ));
        }
    }
    if !reasons.is_empty() {
        return Err(FreezeError::Refused(reasons));
    }
    let head = |rev: &str| -> Result<String, FreezeError> {
        Ok(String::from_utf8_lossy(&git(&["rev-parse", rev])?)
            .trim()
            .to_owned())
    };
    let (commit, tree) = (head("HEAD")?, head("HEAD^{tree}")?);
    let digest = |rel: &str| -> Result<String, FreezeError> {
        hoh_core::objects::hash_file(&checkout.join(rel))
            .map(|(sha, _)| sha)
            .map_err(io)
    };
    let mut source_files = Vec::with_capacity(tracked.len());
    for path in &tracked {
        source_files.push(FileRef {
            path: path.clone(),
            sha256: digest(path)?,
        });
    }
    let file = |rel: &str| -> Result<FileRef, FreezeError> {
        Ok(FileRef {
            path: rel.to_owned(),
            sha256: digest(rel)?,
        })
    };
    let sealed = Manifest {
        git_commit: commit.clone(),
        git_tree: tree.clone(),
        spec: file(&named[0].1)?,
        environment: file(&named[1].1)?,
        test_plan: file(&named[2].1)?,
        source_files,
    }
    .seal();
    // Nothing moved while the manifest was taken (decision 4, M2T10D-67).
    if (head("HEAD")?, head("HEAD^{tree}")?) != (commit, tree)
        || !entries(&git(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ])?)
        .is_empty()
    {
        return Err(FreezeError::Refused(vec![
            "the checkout changed during the freeze".to_owned(),
        ]));
    }
    // The inputs' bytes and the export kept as objects (decision 4): the later checks read these.
    let project = state
        .parent()
        .ok_or_else(|| io(format!("{} has no parent", state.display())))?;
    let mime = |rel: &str| {
        if rel.ends_with(".json") {
            "application/json"
        } else {
            "application/octet-stream"
        }
    };
    let mut kept = Vec::new();
    for ((what, rel), expected) in named.iter().zip([
        &sealed.manifest.spec.sha256,
        &sealed.manifest.environment.sha256,
        &sealed.manifest.test_plan.sha256,
    ]) {
        let (sha256, bytes) =
            hoh_core::objects::put_file(project, &checkout.join(rel)).map_err(io)?;
        if &sha256 != expected {
            return Err(FreezeError::Refused(vec![format!(
                "the {what} {rel} changed during the freeze"
            )]));
        }
        kept.push(ArtifactRow {
            path: hoh_core::objects::relative_path(&sha256),
            sha256,
            bytes,
            mime: mime(rel).to_owned(),
            kind: what.replace(' ', "_"),
            name: rel.clone(),
        });
    }
    // The release policy the candidate tracks, kept as its other inputs are: judgements read it by its digest, never a
    // later checkout (docs/adr/ADR-027.md decision 8, recovery protocol v2; M2T10R-200, -250).
    if let Some(policy) = sealed
        .manifest
        .source_files
        .iter()
        .find(|f| f.path == RELEASE_POLICY)
    {
        let (sha256, bytes) =
            hoh_core::objects::put_file(project, &checkout.join(RELEASE_POLICY)).map_err(io)?;
        if sha256 != policy.sha256 {
            return Err(FreezeError::Refused(vec![format!(
                "the release policy {RELEASE_POLICY} changed during the freeze"
            )]));
        }
        kept.push(ArtifactRow {
            path: hoh_core::objects::relative_path(&sha256),
            sha256,
            bytes,
            mime: "application/json".to_owned(),
            kind: "release_policy".to_owned(),
            name: RELEASE_POLICY.to_owned(),
        });
    }
    let bytes = serde_json::to_vec_pretty(&sealed.to_value())
        .map_err(|e| io(format!("the candidate export: {e}")))?;
    let sha256 = hoh_core::objects::put(project, &bytes).map_err(io)?;
    let export = ArtifactRow {
        path: hoh_core::objects::relative_path(&sha256),
        sha256,
        bytes: bytes.len() as u64,
        mime: "application/json".to_owned(),
        kind: "candidate".to_owned(),
        name: "candidate.json".to_owned(),
    };
    Ok(Prepared {
        sealed,
        export,
        inputs: kept,
    })
}
