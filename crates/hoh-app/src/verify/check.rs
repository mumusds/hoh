//! The check driver's pre-check (REQ-052 behaviour 4, failure modes 3, 4 and 6; docs/adr/ADR-027.md decision 6 and
//! principle two): before a check's command runs, everything it would be judged by is checked against the frozen
//! candidate, each difference named.

use crate::host::control::git;
use hoh_core::persistence::{
    AcceptanceRow, ArtifactRow, EvidenceHandle, PersistError, Reader, Recording,
};
use hoh_core::verification::{
    AcSpec, Evidence, FileRef, Manifest, NamedRef, Planned, Record, Runner, Sealed, outcome,
    plan_shape, rows, valid,
};
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

/// Every way the checkout `checkout` differs from the candidate manifest `m` (REQ-052 behaviours 4 and 5, failure
/// modes 1, 3 and 4): its commit and tree, anything uncommitted, the tracked inventory, each source's bytes, and the
/// specification, environment and test plan digests. The pre-check and the post-check are this one function.
fn differences(checkout: &Path, m: &Manifest) -> Result<Vec<String>, CheckError> {
    // The checkout: the candidate's commit, nothing uncommitted, every source as frozen (failure modes 1 and 4).
    let io = CheckError::Io;
    let mut reasons = Vec::new();
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
    Ok(reasons)
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
    let differing = differences(checkout, m)?;
    // The frozen spec and release-policy objects a judgement reads, there before the command runs: one lost is put
    // back from the checkout just found unchanged, or the check is refused (M2T10R-352).
    if differing.is_empty() {
        let policy = m
            .source_files
            .iter()
            .find(|f| f.path == crate::verify::freeze::RELEASE_POLICY);
        for kept in std::iter::once(&m.spec).chain(policy) {
            if !hoh_core::objects::present(project, &kept.sha256)
                && hoh_core::objects::put_file(project, &checkout.join(&kept.path))
                    .map(|(sha, _)| sha)
                    .as_deref()
                    != Ok(kept.sha256.as_str())
            {
                reasons.push(format!(
                    "the frozen object of {} is lost and the checkout does not give it back",
                    kept.path
                ));
            }
        }
    }
    reasons.extend(differing);
    // HoH's own state still ignored and untracked, as the freeze required (decision 3, M2T10R-06).
    let record = format!("evidence/{id}/{}/RECORD", request.check_id);
    let writes = [
        format!("{record}/execution.json"),
        "pending/RECORD.json".to_owned(),
        format!("{record}/stdout.log"),
        format!("{record}/artifacts/test-results.jsonl"),
    ];
    reasons.extend(crate::verify::freeze::state_guard(checkout, state, &writes).map_err(io)?);
    // The check as the frozen plan has it (failure mode 6), read from the plan object.
    let plan: Value = object(&m.test_plan.sha256)
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
        .map_err(|e| refused(format!("the frozen test plan of {id} cannot be read: {e}")))?;
    // The plan of the locked check-plan schema's shape (principle one rule 1, M2T10R-01).
    reasons.extend(plan_shape(&plan));
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
            let inside = std::fs::canonicalize(checkout.join(&planned.cwd))
                .ok()
                .zip(std::fs::canonicalize(checkout).ok())
                .is_some_and(|(cwd, root)| cwd.starts_with(&root) && cwd.is_dir());
            if !inside {
                reasons.push(format!(
                    "cwd {} is not a directory inside the checkout",
                    planned.cwd
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

/// What a check's run recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    pub record_id: String,
    /// The record's own directory, `.hoh/evidence/<candidate>/<check>/<record>/`.
    pub record_dir: std::path::PathBuf,
    pub outcome: hoh_core::verification::Outcome,
    /// The validity reasons, empty when valid.
    pub reasons: Vec<&'static str>,
}

/// The six variables a check's command gets, as tools/run_check.py sets them (docs/adr/ADR-027.md decision 6).
fn check_env(
    artifacts: &Path,
    request: &Request,
    sealed: &Sealed,
    export: &Path,
    record_id: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("HOH_EVIDENCE_DIR", artifacts.to_string_lossy().into_owned()),
        ("HOH_CHECK_ID", request.check_id.clone()),
        ("HOH_CANDIDATE_ID", sealed.candidate_id.clone()),
        ("HOH_SPEC_SHA256", sealed.manifest.spec.sha256.clone()),
        ("HOH_CANDIDATE_FILE", export.to_string_lossy().into_owned()),
        ("HOH_CHECK_RECORD_ID", record_id.to_owned()),
    ]
}

/// A fresh record id, a random UUID (as tools/run_check.py's `uuid.uuid4()`).
fn new_record_id() -> String {
    let mut bytes = [0u8; 16];
    let _ = rustix::rand::getrandom(&mut bytes, rustix::rand::GetRandomFlags::empty());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A run's exit code and `timed_out` as the record writes them, from the substrate's two dimensions
/// (docs/adr/ADR-027.md decision 6, M2T10D-700): a cancelled or abandoned run is 130 (run_check's interrupt), whatever
/// the child exited with, so it never claims success; a run past its deadline is timed out; a signal is its negative
/// number (Python's convention, as run_check records it).
fn recorded_exit(
    status: Option<hoh_agents::process::Termination>,
    cause: hoh_agents::process::Cause,
) -> (i64, bool) {
    use hoh_agents::process::{Cause, Termination};
    let code = match status {
        Some(Termination::Code(code)) => i64::from(code),
        Some(Termination::Signal(signal)) => -i64::from(signal),
        None => -9,
    };
    match cause {
        Cause::Cancelled | Cause::Abandoned => (130, false),
        Cause::DeadlineExceeded => (code, true),
        Cause::Exited => (code, false),
    }
}

/// `path` relative to the project root `project`, as records name files.
fn relative_to(project: &Path, path: &Path) -> String {
    path.strip_prefix(project)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// A run's facts, which its judgement cannot get again from the files (docs/adr/ADR-027.md decision 8, recovery
/// protocol): its record and the objects kept of it, the trees before and after, the post-check's differences and the
/// clocks. They are the record's pending file (`.hoh/pending/`) until its evidence is committed; the judgement is never kept
/// with them, it is made on the files as they are when the record is committed (M2T10R-103).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Facts {
    record: Record,
    record_object: ArtifactRow,
    objects: Vec<ArtifactRow>,
    tree_before: Value,
    tree_after: Value,
    integrity: Vec<String>,
    clock: Value,
}

/// The world a record is judged in (docs/adr/ADR-027.md principle one): the store's attempts and candidates, the
/// project's files, the frozen plan and the candidate's release policy.
struct World<'a> {
    reader: &'a Reader,
    project: &'a Path,
    evidence: &'a Path,
    plan: &'a Value,
    policy: Option<Value>,
}

impl hoh_core::verification::Lookup for World<'_> {
    fn producer(&self, attempt_id: &str) -> Option<hoh_core::verification::Producer> {
        self.reader.producer(attempt_id).ok().flatten()
    }

    fn run_of(&self, candidate_id: &str) -> Option<String> {
        self.reader
            .candidate(candidate_id)
            .ok()
            .flatten()
            .map(|c| c.run_id)
    }

    fn sha256_of(&self, path: &str) -> Option<String> {
        hoh_core::objects::hash_file(&self.project.join(path))
            .ok()
            .map(|(sha, _)| sha)
    }

    fn candidate_at(&self, path: &str) -> Option<String> {
        let bytes = hoh_core::objects::read_regular(&self.project.join(path)).ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        Sealed::from_value(&value).ok().map(|s| s.candidate_id)
    }

    fn inside(&self, path: &str, dir: &str) -> bool {
        let real = |p: &str| std::fs::canonicalize(self.project.join(p)).ok();
        real(path)
            .zip(real(dir))
            .is_some_and(|(p, d)| p.starts_with(d))
    }

    fn record_id_reused(&self, record_id: &str, record_dir: &str) -> bool {
        // A record directory is named by its record id: another one of the same name is a reuse (rule 19).
        let own = self.project.join(record_dir);
        let mut reused = false;
        for candidate in std::fs::read_dir(self.evidence)
            .into_iter()
            .flatten()
            .flatten()
        {
            for check in std::fs::read_dir(candidate.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let other = check.path().join(record_id);
                reused |= other != own && other.is_dir();
            }
        }
        reused
    }

    fn overlaps(&self, dir: &str) -> bool {
        // Every record directory is `.hoh/evidence/<candidate>/<check>/<record>/`, made new by its run.
        let dir = self.project.join(dir);
        let mut overlapping = false;
        for candidate in std::fs::read_dir(self.evidence)
            .into_iter()
            .flatten()
            .flatten()
        {
            for check in std::fs::read_dir(candidate.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                for record in std::fs::read_dir(check.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let other = record.path();
                    overlapping |=
                        other != dir && (other.starts_with(&dir) || dir.starts_with(&other));
                }
            }
        }
        overlapping
    }

    fn planned(&self, check_id: &str) -> Option<Planned> {
        self.plan["checks"].get(check_id).map(planned_of)
    }

    fn live_sessions(&self, path: &str) -> Option<Value> {
        let bytes = hoh_core::objects::read_regular(&self.project.join(path)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn policy(&self, check_id: &str) -> hoh_core::verification::PolicyCheck {
        use hoh_core::verification::{Policy, PolicyCheck};
        match &self.policy {
            None => PolicyCheck::NoPolicy,
            Some(policy) => match policy["checks"].get(check_id) {
                None => PolicyCheck::Missing,
                Some(c) => PolicyCheck::Check(Policy {
                    mode: c["mode"].as_str().unwrap_or_default().to_owned(),
                    degradable: c["degradable"] == true,
                    requires_plugins: c["requires_plugins"] == true,
                }),
            },
        }
    }
}

/// A check plan entry as [`Planned`].
fn planned_of(entry: &Value) -> Planned {
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
    Planned {
        argv: strings(&entry["argv"]),
        mode: entry["mode"].as_str().unwrap_or_default().to_owned(),
        cwd: entry["cwd"].as_str().unwrap_or(".").to_owned(),
        artifacts: strings(&entry["artifacts"]),
    }
}

/// Run `request`'s check on its frozen candidate (REQ-052 behaviours 4 to 8; docs/adr/ADR-027.md decisions 6 to 9):
/// pre-checked (refused before anything runs, no record), run on the process substrate with the six variables and the
/// plan's timeout, post-checked with the same comparison, recorded under `.hoh/evidence/` (a new directory, never
/// overwritten), its outputs kept as objects, judged by principle one and stored as evidence.
pub async fn run(
    store: &hoh_core::persistence::Store,
    checkout: &Path,
    state: &Path,
    request: &Request,
) -> Result<Ran, CheckError> {
    let blocking = |e: tokio::task::JoinError| CheckError::Io(format!("the check stopped: {e}"));
    let io = CheckError::Io;
    let handle = store.evidence_handle();
    let project = state
        .parent()
        .ok_or_else(|| io(format!("{} has no parent", state.display())))?
        .to_owned();
    // Before: the pre-check (refused, nothing has run and nothing is left) and the workspace observed.
    let before = {
        let reader = handle.reader().map_err(CheckError::Store)?;
        let (checkout, state, request, project) = (
            checkout.to_owned(),
            state.to_owned(),
            request.clone(),
            project.clone(),
        );
        tokio::task::spawn_blocking(move || -> Result<_, CheckError> {
            let ready = pre_check(&reader, &checkout, &state, &request)?;
            let row = reader
                .candidate(&request.candidate_id)
                .map_err(CheckError::Store)?
                .ok_or_else(|| CheckError::Refused(vec!["the candidate went away".to_owned()]))?;
            let plan: Value =
                hoh_core::objects::get(&project, &ready.sealed.manifest.test_plan.sha256)
                    .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
                    .map_err(CheckError::Io)?;
            let timeout = plan["checks"][&request.check_id]["timeout_seconds"]
                .as_f64()
                .unwrap_or(3600.0);
            let tree_before = crate::host::control::workspace(&checkout);
            Ok((ready, tree_before, timeout, project.join(&row.source_path)))
        })
        .await
        .map_err(blocking)??
    };
    let (ready, tree_before, timeout, export) = before;
    // Then the records a crash or a failed commit left without their evidence: a later run never takes their place in
    // the history (M2T10R-05). After the pre-check, which has put back any frozen object a judgement reads (M2T10R-352).
    reconcile(&handle, state, &request.candidate_id).await?;
    // The record's own new directory, and its lock.
    let (record_id, record_dir, lock) = {
        let (state, request) = (state.to_owned(), request.clone());
        tokio::task::spawn_blocking(move || -> Result<_, CheckError> {
            let record_id = new_record_id();
            let dir = state
                .join("evidence")
                .join(&request.candidate_id)
                .join(&request.check_id);
            std::fs::create_dir_all(&dir)
                .map_err(|e| CheckError::Io(format!("{}: {e}", dir.display())))?;
            let record_dir = dir.join(&record_id);
            // Append-only (behaviour 8): a record directory is made new, never reused.
            std::fs::create_dir(&record_dir)
                .and_then(|()| std::fs::create_dir(record_dir.join("artifacts")))
                .map_err(|e| CheckError::Io(format!("{}: {e}", record_dir.display())))?;
            // The record's lock, taken before the command runs and held to the commit: the finishing task never opens
            // a path the command could have written to lock its record (M2T10R-350).
            let lock = lock_record(&state, &record_id, true)?
                .ok_or_else(|| CheckError::Io("the record's lock was not granted".to_owned()))?;
            Ok((record_id, record_dir, lock))
        })
        .await
        .map_err(blocking)??
    };
    // The run, its post-check and its record are a task of their own: dropping this future cancels the command, never
    // its record (docs/adr/ADR-027.md decision 6, M2T10R-03).
    let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
    let finishing = tokio::spawn(finish(
        handle,
        Finish {
            checkout: checkout.to_owned(),
            state: state.to_owned(),
            project,
            request: request.clone(),
            ready,
            tree_before,
            record_id,
            record_dir,
            lock,
            timeout,
            export,
        },
        cancelled,
    ));
    let ran = finishing.await.map_err(blocking)?;
    drop(cancel);
    ran
}

/// What the pre-phase hands the finishing task.
struct Finish {
    checkout: std::path::PathBuf,
    state: std::path::PathBuf,
    project: std::path::PathBuf,
    request: Request,
    ready: Ready,
    tree_before: Value,
    record_id: String,
    record_dir: std::path::PathBuf,
    /// The record's lock, held from before the command to the commit.
    lock: std::fs::File,
    timeout: f64,
    export: std::path::PathBuf,
}

/// The command on the process substrate, then the post-check, the record, the objects, the judgement and the evidence.
/// Once the command has started this always ends in a record: `cancelled` resolving (its sender dropped) cancels the
/// command and the run is recorded as interrupted; a post-check that fails is recorded as a change it cannot rule out;
/// what the command did to its record directory is recorded as changes (`reclaim`); the run's facts are kept as
/// pending under `.hoh/pending/` before the commit, so a failed commit is recorded by the next run's [`reconcile`]
/// (M2T10R-02, -03, -05, -450).
async fn finish(
    handle: EvidenceHandle,
    f: Finish,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
) -> Result<Ran, CheckError> {
    use hoh_agents::process::{Config, Escalation, Event, Process};
    let blocking = |e: tokio::task::JoinError| CheckError::Io(format!("the check stopped: {e}"));
    let io = CheckError::Io;
    let Finish {
        checkout,
        state,
        project,
        request,
        ready,
        tree_before,
        record_id,
        record_dir,
        lock,
        timeout,
        export,
    } = f;
    let request = &request;
    // The command, on the process substrate (argv only, its own process group, the plan's timeout).
    let artifacts_dir = record_dir.join("artifacts");
    let (stdout, stderr) = (record_dir.join("stdout.log"), record_dir.join("stderr.log"));
    // Started on the wall clock, measured on the monotonic clock (docs/05; docs/adr/ADR-027.md decision 6).
    let started = std::time::SystemTime::now();
    let began = std::time::Instant::now();
    let mut command = std::process::Command::new(&ready.planned.argv[0]);
    command
        .args(&ready.planned.argv[1..])
        .current_dir(checkout.join(&ready.planned.cwd))
        .envs(check_env(
            &artifacts_dir,
            request,
            &ready.sealed,
            &export,
            &record_id,
        ));
    let config = Config {
        deadline: Some(std::time::Duration::from_secs_f64(timeout)),
        stdout_copy: Some(stdout.clone()),
        stderr_copy: Some(stderr.clone()),
        ..Config::default()
    };
    // What HoH made for the run, by identity, before the command can touch it (M2T10R-501).
    let mut owned = Owned {
        dir: identity(&record_dir),
        stdout: None,
        stderr: None,
    };
    let (exit_code, timed_out) = match Process::spawn(command, config) {
        Ok(mut process) => {
            // The copies exist before the child does.
            (owned.stdout, owned.stderr) = (identity(&stdout), identity(&stderr));
            let mut report = None;
            let mut cancelling = false;
            loop {
                tokio::select! {
                    event = process.next() => match event {
                        Some(Event::Exit(r)) => report = Some(r),
                        Some(_) => {}
                        None => break,
                    },
                    _ = &mut cancelled, if !cancelling => {
                        cancelling = true;
                        process.cancel(Escalation::default());
                    }
                }
            }
            let report = report
                .ok_or_else(|| io("the check's process ended without its report".to_owned()))?;
            recorded_exit(report.status, report.cause)
        }
        Err(e) => {
            // As run_check does when the command cannot start: 127, the error as its stderr.
            std::fs::write(&stdout, b"").map_err(|e| io(e.to_string()))?;
            std::fs::write(&stderr, e.to_string()).map_err(|e| io(e.to_string()))?;
            (owned.stdout, owned.stderr) = (identity(&stdout), identity(&stderr));
            (127, false)
        }
    };
    let elapsed = began.elapsed();
    let wall_finished = std::time::SystemTime::now();
    // The record's finish is its start plus the monotonic time: interval and duration agree by construction and never
    // run backwards, the duration is the time that passed (a jumped or stepped-back wall clock moves neither); the
    // wall clock's own finish goes to the evidence event (M2T10D-701, -750).
    let finished = started + elapsed;
    // After: the post-check, the record, the objects.
    let facts = {
        let (checkout, request, project, ready) = (
            checkout.to_owned(),
            request.clone(),
            project.clone(),
            ready.clone(),
        );
        let (record_dir, record_id, export, state) = (
            record_dir.clone(),
            record_id.clone(),
            export.clone(),
            state.clone(),
        );
        tokio::task::spawn_blocking(move || -> Result<Facts, CheckError> {
            let io = |e: String| CheckError::Io(e);
            // A post-check that cannot run cannot show the source unchanged: that is recorded, never lost (M2T10R-02).
            let mut integrity =
                differences(&checkout, &ready.sealed.manifest).unwrap_or_else(|e| {
                    vec![format!(
                        "post-check failed, so the source cannot be shown unchanged: {}",
                        described(&e)
                    )]
                });
            // The record directory is the command's to write beside its artifacts; once it has ended, the finishing
            // task takes the reserved names back: whatever the command did to them is a change recorded, never an
            // abort (M2T10R-450).
            integrity.extend(reclaim(&record_dir, &owned).map_err(io)?);
            // The record id is new: a pending file there already is one the command wrote (M2T10R-552).
            let planted = pending_path(&state, &record_id);
            if std::fs::symlink_metadata(&planted).is_ok() {
                remove_tree(&planted).map_err(io)?;
                integrity.push("the command wrote HoH's pending file for its record".to_owned());
            }
            let tree_after = crate::host::control::workspace(&checkout);
            let rel = |p: &Path| relative_to(&project, p);
            let file = |p: &Path| -> Result<FileRef, CheckError> {
                Ok(FileRef {
                    path: rel(p),
                    sha256: hoh_core::objects::hash_file(p).map_err(io)?.0,
                })
            };
            let mut artifacts = Vec::new();
            let mut missing = Vec::new();
            for name in &ready.planned.artifacts {
                let f = record_dir.join("artifacts").join(name);
                match std::fs::symlink_metadata(&f) {
                    // An artifact the command left unreadable is not there to be bound (M2T10R-500).
                    Ok(m) if m.file_type().is_file() => match file(&f) {
                        Ok(r) => artifacts.push(NamedRef {
                            name: name.clone(),
                            path: r.path,
                            sha256: r.sha256,
                        }),
                        Err(_) => missing.push(name.clone()),
                    },
                    _ => missing.push(name.clone()),
                }
            }
            let export_ref = file(&export)?;
            let instant = |t| hoh_core::time::rfc3339_millis_utc(t);
            let duration = elapsed.as_secs_f64();
            let m = &ready.sealed.manifest;
            let record = Record {
                schema_version: "1.1".to_owned(),
                record_type: "command_execution".to_owned(),
                record_id: record_id.clone(),
                check_id: request.check_id.clone(),
                candidate_id: ready.sealed.candidate_id.clone(),
                spec_sha256: m.spec.sha256.clone(),
                mode: ready.planned.mode.clone(),
                command: ready.planned.argv.clone(),
                cwd: ready.planned.cwd.clone(),
                started_at: instant(started),
                finished_at: instant(finished),
                duration_seconds: duration,
                exit_code,
                timed_out,
                stdout: file(&record_dir.join("stdout.log"))?,
                stderr: file(&record_dir.join("stderr.log"))?,
                candidate_manifest: export_ref,
                source_unchanged: integrity.is_empty(),
                environment_sha256: m.environment.sha256.clone(),
                test_plan_sha256: m.test_plan.sha256.clone(),
                artifacts,
                missing_artifacts: missing,
                producer_attempt_id: request.producer_attempt_id.clone(),
                runner: Runner {
                    tool: "hoh".to_owned(),
                    euid: i64::from(rustix::process::geteuid().as_raw()),
                    boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                        .ok()
                        .map(|b| b.trim().to_owned()),
                },
            };
            let bytes = serde_json::to_vec_pretty(&record).map_err(|e| io(e.to_string()))?;
            // The record's files kept as objects (decision 8).
            let keep = |p: &Path, kind: &str, name: &str| -> Result<ArtifactRow, CheckError> {
                let (sha256, bytes) = hoh_core::objects::put_file(&project, p).map_err(io)?;
                Ok(ArtifactRow {
                    path: hoh_core::objects::relative_path(&sha256),
                    sha256,
                    bytes,
                    mime: "application/octet-stream".to_owned(),
                    kind: kind.to_owned(),
                    name: name.to_owned(),
                })
            };
            let mut objects = vec![
                keep(&record_dir.join("stdout.log"), "stdout", "stdout.log")?,
                keep(&record_dir.join("stderr.log"), "stderr", "stderr.log")?,
            ];
            for a in &record.artifacts {
                objects.push(keep(&project.join(&a.path), "artifact", &a.name)?);
            }
            let record_sha = hoh_core::objects::put(&project, &bytes).map_err(io)?;
            let record_object = ArtifactRow {
                path: hoh_core::objects::relative_path(&record_sha),
                sha256: record_sha,
                bytes: bytes.len() as u64,
                mime: "application/octet-stream".to_owned(),
                kind: "execution".to_owned(),
                name: "execution.json".to_owned(),
            };
            Ok(Facts {
                record,
                record_object,
                objects,
                tree_before,
                tree_after,
                integrity,
                clock: serde_json::json!({
                    "wall_finished_at": hoh_core::time::rfc3339_millis_utc(wall_finished),
                    "monotonic_seconds": duration,
                }),
            })
        })
        .await
        .map_err(blocking)??
    };
    // Still under the record's lock, taken before the command (M2T10R-150, -350): the pending facts first, then the
    // record file, then the judgement on the files as they are and the commit; from the pending facts on, a crash or a
    // failed commit is finished by the candidate's next run (M2T10R-05).
    let (recording, ran) = {
        let reader = handle.reader().map_err(CheckError::Store)?;
        let (project, state, ready, record_dir) = (
            project.clone(),
            state.clone(),
            ready.clone(),
            record_dir.clone(),
        );
        tokio::task::spawn_blocking(move || -> Result<_, CheckError> {
            let text =
                serde_json::to_vec_pretty(&facts).map_err(|e| CheckError::Io(e.to_string()))?;
            write_new(&pending_path(&state, &facts.record.record_id), &text)?;
            let bytes = serde_json::to_vec_pretty(&facts.record)
                .map_err(|e| CheckError::Io(e.to_string()))?;
            write_new(&record_dir.join("execution.json"), &bytes)?;
            let (recording, ran) = judge(
                &reader,
                &Judged {
                    project: &project,
                    state: &state,
                    sealed: &ready.sealed,
                    run_id: &ready.run_id,
                    record_dir: &record_dir,
                },
                &facts,
            )?;
            Ok((recording, ran))
        })
        .await
        .map_err(blocking)??
    };
    handle
        .record_evidence(recording)
        .await
        .map_err(CheckError::Store)?;
    settle(&state, &record_id, lock);
    Ok(ran)
}

/// After record `record_id`'s commit: its pending facts and lock file are no longer kept (M2T10R-554). Safe for a
/// reconciliation meanwhile, which passes over a recorded record before reading and again after locking.
fn settle(state: &Path, record_id: &str, lock: std::fs::File) {
    let _ = std::fs::remove_file(pending_path(state, record_id));
    let _ = std::fs::remove_file(lock_path(state, record_id));
    drop(lock);
}

/// Where record `record_id`'s pending facts are kept until its evidence is committed (M2T10R-05, -103):
/// `.hoh/pending/<record_id>.json`, HoH's own state outside the record directory the command writes (M2T10R-450); one
/// the command wrote there for its own record is removed as a change (M2T10R-552), and the file goes after the commit
/// (M2T10R-554).
fn pending_path(state: &Path, record_id: &str) -> std::path::PathBuf {
    state.join("pending").join(format!("{record_id}.json"))
}

/// Remove `p`, whatever it is: a file, a link (never followed), or a directory tree the command may have closed, which
/// is opened up for its owner first (M2T10R-550). Nothing there is nothing to do.
fn remove_tree(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let at = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    fn open_up(p: &Path) {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
            for entry in std::fs::read_dir(p).into_iter().flatten().flatten() {
                open_up(&entry.path());
            }
        }
    }
    match std::fs::symlink_metadata(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(at(p, e)),
        Ok(m) if m.is_dir() => {
            open_up(p);
            std::fs::remove_dir_all(p).map_err(|e| at(p, e))
        }
        Ok(_) => std::fs::remove_file(p).map_err(|e| at(p, e)),
    }
}

/// A file's or directory's identity: its device and inode.
fn identity(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(p)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

/// What HoH made for a run before its command could touch it: the record directory and the output copies, by identity.
struct Owned {
    dir: Option<(u64, u64)>,
    stdout: Option<(u64, u64)>,
    stderr: Option<(u64, u64)>,
}

/// Take the record directory `dir` back from the command after it ended (M2T10R-450, -500, -501): the directory HoH
/// made, with its owner's access, holding `artifacts/` (the command's own to rebuild), the `stdout.log` and `stderr.log`
/// HoH made, readable, and no `execution.json` but the one the finishing task writes. Whatever the command removed,
/// replaced (also by another of the same kind), locked away or wrote at these names is put right and named as a
/// change. What the command writes into HoH's files in place counts as its output, as it does for run_check.
fn reclaim(dir: &Path, owned: &Owned) -> Result<Vec<String>, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let at = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let mut changes = Vec::new();
    let remove = remove_tree;
    // Its owner's access back where the command took it, as a change.
    let grant =
        |p: &Path, needed: u32, what: &str, changes: &mut Vec<String>| -> Result<(), String> {
            let mode = std::fs::symlink_metadata(p)
                .map_err(|e| at(p, e))?
                .permissions()
                .mode();
            if mode & needed != needed {
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode | needed))
                    .map_err(|e| at(p, e))?;
                changes.push(format!("the command changed the permissions of {what}"));
            }
            Ok(())
        };
    let is_dir = std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir());
    if is_dir && identity(dir) == owned.dir {
        grant(dir, 0o700, "its record directory", &mut changes)?;
    } else {
        remove(dir)?;
        std::fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
        changes.push("the command removed or replaced its record directory".to_owned());
    }
    let artifacts = dir.join("artifacts");
    match std::fs::symlink_metadata(&artifacts) {
        Ok(m) if m.is_dir() => grant(&artifacts, 0o700, "its artifacts directory", &mut changes)?,
        Ok(_) => {
            remove(&artifacts)?;
            changes.push("the command replaced its artifacts directory".to_owned());
        }
        Err(_) => {}
    }
    std::fs::create_dir_all(&artifacts).map_err(|e| at(&artifacts, e))?;
    for (name, made) in [("stdout.log", owned.stdout), ("stderr.log", owned.stderr)] {
        let p = dir.join(name);
        let file = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file());
        if file && identity(&p) == made {
            grant(&p, 0o600, name, &mut changes)?;
        } else {
            remove(&p)?;
            std::fs::write(&p, b"").map_err(|e| at(&p, e))?;
            changes.push(format!(
                "the command removed or replaced {name} in its record directory"
            ));
        }
    }
    let record = dir.join("execution.json");
    if std::fs::symlink_metadata(&record).is_ok() {
        remove(&record)?;
        changes.push("the command wrote execution.json in its record directory".to_owned());
    }
    Ok(changes)
}

/// What a [`CheckError`] says.
fn described(e: &CheckError) -> String {
    match e {
        CheckError::Refused(reasons) => reasons.join("; "),
        CheckError::Store(e) => e.to_string(),
        CheckError::Io(why) => why.clone(),
    }
}

/// `bytes` as the new file `path`, whole or not at all: written to a staged name and synced, then linked (never over an
/// existing file, the evidence is append-only), the directory synced (M2T10R-56).
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), CheckError> {
    use std::io::Write as _;
    let at = |e: std::io::Error| CheckError::Io(format!("{}: {e}", path.display()));
    let dir = path
        .parent()
        .ok_or_else(|| CheckError::Io(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(dir).map_err(at)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A staged copy of this writer's own, named at random: two writers of one record, in any process or host, never
    // share one, and a writer removes only the copy it made (M2T10R-100, -253).
    let mut random = [0u8; 16];
    rustix::rand::getrandom(&mut random, rustix::rand::GetRandomFlags::empty())
        .map_err(|e| at(e.into()))?;
    let tag: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let staged = dir.join(format!(".{name}.{tag}.staged"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .map_err(at)?;
    let written = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        match std::fs::hard_link(&staged, path) {
            Ok(()) => std::fs::File::open(dir)?.sync_all(),
            // Already published: the same bytes are no error, others are.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if hoh_core::objects::read_regular(path).map_err(std::io::Error::other)? == bytes {
                    Ok(())
                } else {
                    Err(std::io::Error::other("it exists with other bytes"))
                }
            }
            Err(e) => Err(e),
        }
    })();
    let _ = std::fs::remove_file(&staged);
    written.map_err(at)
}

/// Where a record is judged: the project, its state, the candidate and its run, the record's directory. No checkout: a
/// judgement reads only bytes bound to the candidate and the record's own files (M2T10R-200, -250).
struct Judged<'a> {
    project: &'a Path,
    state: &'a Path,
    sealed: &'a Sealed,
    run_id: &'a str,
    record_dir: &'a Path,
}

/// The judgement of a run's facts on its files as they are now (principle one; decisions 8 and 9): its observations
/// read now, its validity, outcome and acceptance rows. The finishing task and the reconciliation both make it, so a
/// record is never recorded with a judgement older than its files (M2T10R-103).
fn judge(reader: &Reader, at: &Judged<'_>, facts: &Facts) -> Result<(Recording, Ran), CheckError> {
    let (project, sealed) = (at.project, at.sealed);
    let record = &facts.record;
    // Observations (decision 8): the result lines; without any, the declared artifacts found with the record's own
    // conclusion. As bytes, line by line, as the gate and the replica read them: a line that is not JSON (not UTF-8,
    // say) stays a line, and is not a result line (rule 1).
    let mut observations = Vec::new();
    if let Some(results) = record
        .artifacts
        .iter()
        .find(|a| a.name == "test-results.jsonl")
    {
        let bytes =
            hoh_core::objects::read_regular(&project.join(&results.path)).unwrap_or_default();
        let blank = |l: &[u8]| l.iter().all(|b| b" \t\n\r\x0b\x0c".contains(b));
        for line in bytes.split(|b| *b == b'\n').filter(|l| !blank(l)) {
            observations.push(
                serde_json::from_slice(line)
                    .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(line).into_owned())),
            );
        }
    }
    if observations.is_empty() {
        observations = hoh_core::verification::fallback(record);
    }
    let e = &Evidence {
        record: record.clone(),
        git_tree: facts.tree_after["head_tree"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        record_dir: relative_to(project, at.record_dir),
        observations,
    };
    let m = &sealed.manifest;
    let plan: Value = hoh_core::objects::get(project, &m.test_plan.sha256)
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        .map_err(CheckError::Io)?;
    // The release policy the candidate tracks, as frozen: an object by its digest, never a checkout; one that cannot be
    // read is an error, never no policy (M2T10R-200, -250).
    let policy = match m
        .source_files
        .iter()
        .find(|f| f.path == crate::verify::freeze::RELEASE_POLICY)
    {
        None => None,
        Some(f) => Some(
            hoh_core::objects::get(project, &f.sha256)
                .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()))
                .map_err(|e| {
                    CheckError::Io(format!(
                        "the candidate's release policy cannot be read as frozen: {e}"
                    ))
                })?,
        ),
    };
    let world = World {
        reader,
        project,
        evidence: &at.state.join("evidence"),
        plan: &plan,
        policy,
    };
    let verdict = valid(e, sealed, &m.spec.sha256, &world);
    let outcome = outcome(e, &verdict, &world);
    let reasons: Vec<&'static str> = verdict
        .as_ref()
        .err()
        .map(|i| i.codes())
        .unwrap_or_default();
    let check_degradable = matches!(
        hoh_core::verification::Lookup::policy(&world, &e.record.check_id),
        hoh_core::verification::PolicyCheck::Check(ref p) if p.degradable
    );
    // The rows need the run's spec to be the candidate's; each AC's TEST is the one the frozen spec declares.
    let mut rows_out = Vec::new();
    if let Some(acceptance) = reader.acceptance(at.run_id).map_err(CheckError::Store)?
        && acceptance.spec_sha256 == m.spec.sha256
    {
        let spec: Value = hoh_core::objects::get(project, &m.spec.sha256)
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
            .map_err(CheckError::Io)?;
        let declared: std::collections::BTreeMap<String, String> = spec["requirements"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|r| r["acceptance"].as_array().into_iter().flatten())
            .filter_map(|a| {
                Some((
                    a["id"].as_str()?.to_owned(),
                    a["test_id"].as_str()?.to_owned(),
                ))
            })
            .collect();
        let acs: Vec<AcSpec> = acceptance
            .criteria
            .iter()
            .filter_map(|(ac, check, degradable)| {
                Some(AcSpec {
                    ac_id: ac.clone(),
                    test_id: declared.get(ac)?.clone(),
                    check_id: check.clone(),
                    degradable: *degradable,
                })
            })
            .collect();
        for (ac_id, status) in rows(e, &outcome, &acs, check_degradable) {
            let test = acs
                .iter()
                .find(|a| a.ac_id == ac_id)
                .map(|a| a.test_id.clone())
                .unwrap_or_default();
            rows_out.push(AcceptanceRow {
                ac_id,
                spec_id: acceptance.spec_id.clone(),
                status: status.code(),
                reason: if reasons.is_empty() {
                    format!("{test} in check {}", e.record.check_id)
                } else {
                    format!(
                        "{test} in check {}: {}",
                        e.record.check_id,
                        reasons.join(", ")
                    )
                },
            });
        }
    }
    let ran = Ran {
        record_id: e.record.record_id.clone(),
        record_dir: at.record_dir.to_owned(),
        outcome,
        reasons: reasons.clone(),
    };
    Ok((
        Recording {
            record: e.record.clone(),
            record_object: facts.record_object.clone(),
            objects: facts.objects.clone(),
            outcome,
            reasons,
            tree_before: facts.tree_before.clone(),
            tree_after: facts.tree_after.clone(),
            clock: facts.clock.clone(),
            observations: e.observations.clone(),
            rows: rows_out,
            integrity: facts.integrity.clone(),
        },
        ran,
    ))
}

/// Where record `record_id`'s lock file is: `.hoh/locks/<record_id>.lock`.
fn lock_path(state: &Path, record_id: &str) -> std::path::PathBuf {
    state.join("locks").join(format!("{record_id}.lock"))
}

/// The lock of record `record_id`, `.hoh/locks/<record_id>.lock`: its run holds it from before its command to its
/// commit, so a reconciliation never takes a record a live run is still recording (M2T10R-150, -350, -400); the lock
/// goes with its holder, also when the process dies. Without `wait`, `None` when another holds it.
fn lock_record(
    state: &Path,
    record_id: &str,
    wait: bool,
) -> Result<Option<std::fs::File>, CheckError> {
    use rustix::fs::{FlockOperation, Mode, OFlags};
    // HoH's own state, outside the record directory the command writes beside: a command clearing what lies by its
    // artifacts cannot remove it (M2T10R-400).
    let locks = state.join("locks");
    std::fs::create_dir_all(&locks)
        .map_err(|e| CheckError::Io(format!("{}: {e}", locks.display())))?;
    let path = lock_path(state, record_id);
    let at = |e: std::io::Error| CheckError::Io(format!("{}: {e}", path.display()));
    // Opened without blocking and kept only as a regular file, before any lock is asked for (M2T10R-301).
    let fd = rustix::fs::open(
        &path,
        OFlags::CREATE
            | OFlags::WRONLY
            | OFlags::APPEND
            | OFlags::NONBLOCK
            | OFlags::NOFOLLOW
            | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    )
    .map_err(|e| at(e.into()))?;
    let file = std::fs::File::from(fd);
    if !file.metadata().map_err(at)?.file_type().is_file() {
        return Err(CheckError::Io(format!(
            "{}: not a regular file",
            path.display()
        )));
    }
    let operation = if wait {
        FlockOperation::LockExclusive
    } else {
        FlockOperation::NonBlockingLockExclusive
    };
    match rustix::fs::flock(&file, operation) {
        Ok(()) => Ok(Some(file)),
        Err(e) if !wait && e == rustix::io::Errno::WOULDBLOCK => Ok(None),
        Err(e) => Err(at(e.into())),
    }
}

/// The directories in `dir`, sorted; none when `dir` does not exist, any other failure an error (M2T10R-102).
fn subdirectories(dir: &Path) -> Result<Vec<std::path::PathBuf>, CheckError> {
    let at = |e: std::io::Error| CheckError::Io(format!("{}: {e}", dir.display()));
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(at(e)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(at)?;
        if entry.file_type().map_err(at)?.is_dir() {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// Record the evidence of each record of candidate `candidate_id` whose pending facts were never committed (a crash or
/// a failed commit; docs/adr/ADR-027.md decision 8, recovery protocol), judged on its files as they are now. A record
/// whose evidence is recorded is passed over without reading its files (M2T10R-151); one whose lock another holds (a
/// live run, another reconciliation) is left to it (M2T10R-150); a pending file or directory that cannot be read is an
/// error, so the candidate's next run waits rather than take its place in the history (M2T10R-102, -203). The record
/// file a crash left unwritten is written from the record object's bytes, and every object the evidence names is put
/// back when its bytes are still to be had; one that cannot be is left out only from a record judged INVALID
/// (M2T10R-202). It reads no checkout (M2T10R-250). The work is a task of its own that keeps each record's lock until
/// that record's transaction has ended: a caller that stops waiting cancels nothing (M2T10R-201). A check's run does
/// this for its candidate after its pre-check and before its command (M2T10R-352); M2-T16 does it for each candidate
/// when the host starts. Returns how many it recorded.
pub async fn reconcile(
    handle: &EvidenceHandle,
    state: &Path,
    candidate_id: &str,
) -> Result<usize, CheckError> {
    let (h, state, candidate_id) = (handle.clone(), state.to_owned(), candidate_id.to_owned());
    tokio::spawn(async move {
        let reader = h.reader().map_err(CheckError::Store)?;
        let settled = state.clone();
        let found = tokio::task::spawn_blocking(move || pending(&reader, &state, &candidate_id))
            .await
            .map_err(|e| CheckError::Io(format!("the reconciliation stopped: {e}")))??;
        let recorded = found.len();
        for (lock, recording) in found {
            let record_id = recording.record.record_id.clone();
            h.record_evidence(recording)
                .await
                .map_err(CheckError::Store)?;
            settle(&settled, &record_id, lock);
        }
        Ok(recorded)
    })
    .await
    .map_err(|e| CheckError::Io(format!("the reconciliation stopped: {e}")))?
}

/// The uncommitted records of candidate `candidate_id`, each locked and judged, ready to commit (see [`reconcile`]).
fn pending(
    reader: &Reader,
    state: &Path,
    candidate_id: &str,
) -> Result<Vec<(std::fs::File, Recording)>, CheckError> {
    let project = state
        .parent()
        .ok_or_else(|| CheckError::Io(format!("{} has no parent", state.display())))?;
    let mut candidate = None;
    let mut found = Vec::new();
    for check in subdirectories(&state.join("evidence").join(candidate_id))? {
        for record_dir in subdirectories(&check)? {
            let record_id = record_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let recorded = || {
                reader
                    .evidence_outcome(&record_id)
                    .map(|o| o.is_some())
                    .map_err(CheckError::Store)
            };
            if recorded()? {
                continue;
            }
            let file = pending_path(state, &record_id);
            let unreadable = |e: String| CheckError::Io(format!("{}: {e}", file.display()));
            match std::fs::symlink_metadata(&file) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(unreadable(e.to_string())),
                Ok(_) => {}
            }
            let Some(lock) = lock_record(state, &record_id, false)? else {
                continue;
            };
            if recorded()? {
                continue;
            }
            let text = hoh_core::objects::read_regular(&file).map_err(unreadable)?;
            let mut facts: Facts =
                serde_json::from_slice(&text).map_err(|e| unreadable(e.to_string()))?;
            // Bound to its record directory by candidate and check as well as by record id (M2T10R-551).
            let check_id = check
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let r = &facts.record;
            if (
                r.record_id.as_str(),
                r.candidate_id.as_str(),
                r.check_id.as_str(),
            ) != (record_id.as_str(), candidate_id, check_id.as_str())
            {
                return Err(unreadable(format!(
                    "it holds record {} of candidate {} and check {}, not the record directory's {record_id} of \
                     {candidate_id} and {check_id}",
                    r.record_id, r.candidate_id, r.check_id
                )));
            }
            let lost = restore(project, &record_dir, &mut facts)?;
            if candidate.is_none() {
                let row = reader
                    .candidate(candidate_id)
                    .map_err(CheckError::Store)?
                    .ok_or_else(|| {
                        CheckError::Io(format!("candidate {candidate_id} is not frozen here"))
                    })?;
                let export = row.source_path.rsplit('/').next().unwrap_or_default();
                let sealed = hoh_core::objects::get(project, export)
                    .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()))
                    .and_then(|v| Sealed::from_value(&v))
                    .map_err(CheckError::Io)?;
                candidate = Some((sealed, row.run_id));
            }
            let Some((sealed, run_id)) = candidate.as_ref() else {
                continue;
            };
            let (recording, _) = judge(
                reader,
                &Judged {
                    project,
                    state,
                    sealed,
                    run_id,
                    record_dir: &record_dir,
                },
                &facts,
            )?;
            if !lost.is_empty()
                && recording.outcome.status != hoh_core::verification::Status::Invalid
            {
                return Err(unreadable(format!(
                    "objects {} are lost, yet the record still holds",
                    lost.join(", ")
                )));
            }
            found.push((lock, recording));
        }
    }
    Ok(found)
}

/// Put back what a record's evidence names and a crash or a loss took (M2T10R-202): the record file from the record
/// object's bytes (or the record itself, if they hash to it), every object from the record file it was kept of while
/// that still matches its digest. An object that cannot be put back is left out of `facts` and named in the result.
fn restore(
    project: &Path,
    record_dir: &Path,
    facts: &mut Facts,
) -> Result<Vec<String>, CheckError> {
    let io = CheckError::Io;
    let sha = &facts.record_object.sha256;
    let record_bytes = match hoh_core::objects::get(project, sha) {
        Ok(bytes) => bytes,
        Err(_) => {
            let bytes = serde_json::to_vec_pretty(&facts.record).map_err(|e| io(e.to_string()))?;
            if &hoh_core::objects::put(project, &bytes).map_err(io)? != sha {
                return Err(io(format!(
                    "{}: the record object {sha} is lost and the record does not give its bytes",
                    record_dir.display()
                )));
            }
            bytes
        }
    };
    // Written where a crash left none; one there must be those bytes, as a regular file, or the record is not the
    // one its evidence would name and the reconciliation refuses it (M2T10R-300).
    let execution = record_dir.join("execution.json");
    match std::fs::symlink_metadata(&execution) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => write_new(&execution, &record_bytes)?,
        _ => {
            let published = hoh_core::objects::read_regular(&execution).map_err(io)?;
            if published != record_bytes {
                return Err(io(format!(
                    "{}: the record file is not the record {sha} its evidence names",
                    execution.display()
                )));
            }
        }
    }
    let record = &facts.record;
    let source = |o: &ArtifactRow| -> Option<std::path::PathBuf> {
        match o.kind.as_str() {
            "stdout" => Some(record_dir.join("stdout.log")),
            "stderr" => Some(record_dir.join("stderr.log")),
            "artifact" => record
                .artifacts
                .iter()
                .find(|a| a.name == o.name)
                .map(|a| project.join(&a.path)),
            _ => None,
        }
    };
    let mut lost = Vec::new();
    let mut kept = Vec::new();
    for o in &facts.objects {
        let back = hoh_core::objects::present(project, &o.sha256)
            || source(o).is_some_and(|f| {
                hoh_core::objects::hash_file(&f).is_ok_and(|(sha, _)| sha == o.sha256)
                    && hoh_core::objects::put_file(project, &f)
                        .is_ok_and(|(sha, _)| sha == o.sha256)
            });
        if back {
            kept.push(o.clone());
        } else {
            lost.push(o.name.clone());
        }
    }
    facts.objects = kept;
    Ok(lost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoh_agents::process::{Cause, Termination};

    #[test]
    fn a_file_is_published_whole_once_and_again_only_as_the_same_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        // docs/adr/ADR-027.md decision 8, recovery protocol (M2T10R-100): writers of the same record at once each
        // stage their own copy; the file appears whole, the same bytes again are no error, other bytes are refused, and
        // no staged copy is left.
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("execution.json");
        let bytes = vec![b'x'; 1 << 20];
        std::thread::scope(|s| {
            let writers: Vec<_> = (0..8)
                .map(|_| s.spawn(|| write_new(&target, &bytes)))
                .collect();
            for w in writers {
                assert!(matches!(w.join(), Ok(Ok(()))));
            }
        });
        assert_eq!(std::fs::read(&target)?, bytes);
        assert!(write_new(&target, b"other").is_err());
        assert_eq!(std::fs::read(&target)?, bytes);
        let names: Vec<_> = std::fs::read_dir(dir.path())?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<_, _>>()?;
        assert_eq!(names, ["execution.json"]);
        Ok(())
    }

    #[test]
    fn a_cancelled_run_never_claims_success_and_a_late_one_is_timed_out() {
        // docs/adr/ADR-027.md decision 6 (M2T10D-700): the substrate's cause and termination are two dimensions.
        assert_eq!(
            recorded_exit(Some(Termination::Code(0)), Cause::Exited),
            (0, false)
        );
        assert_eq!(
            recorded_exit(Some(Termination::Code(101)), Cause::Exited),
            (101, false)
        );
        assert_eq!(
            recorded_exit(Some(Termination::Signal(15)), Cause::Exited),
            (-15, false)
        );
        // Cancelled, even when the child exits 0 on its SIGTERM: an interrupt.
        assert_eq!(
            recorded_exit(Some(Termination::Code(0)), Cause::Cancelled),
            (130, false)
        );
        assert_eq!(
            recorded_exit(Some(Termination::Signal(9)), Cause::Abandoned),
            (130, false)
        );
        assert_eq!(recorded_exit(None, Cause::Cancelled), (130, false));
        assert_eq!(
            recorded_exit(Some(Termination::Signal(15)), Cause::DeadlineExceeded),
            (-15, true)
        );
        assert_eq!(
            recorded_exit(Some(Termination::Code(0)), Cause::DeadlineExceeded),
            (0, true)
        );
    }
}
