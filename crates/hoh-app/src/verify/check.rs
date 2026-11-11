//! The check driver's pre-check (REQ-052 behaviour 4, failure modes 3, 4 and 6; docs/adr/ADR-027.md decision 6 and
//! principle two): before a check's command runs, everything it would be judged by is checked against the frozen
//! candidate, each difference named.

use crate::host::control::git;
use hoh_core::persistence::{AcceptanceRow, ArtifactRow, PersistError, Reader, Recording};
use hoh_core::verification::{
    AcSpec, Evidence, FileRef, Manifest, NamedRef, Planned, Record, Runner, Sealed, outcome, rows,
    valid,
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
    reasons.extend(differences(checkout, m)?);
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

/// What a run leaves for the judge: the record and the observations, the trees before and after, and the post-check's
/// differences.
struct Done {
    evidence: Evidence,
    clock: Value,
    tree_before: Value,
    tree_after: Value,
    integrity: Vec<String>,
    objects: Vec<ArtifactRow>,
    record_object: ArtifactRow,
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
        let bytes = std::fs::read(self.project.join(path)).ok()?;
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
        let bytes = std::fs::read(self.project.join(path)).ok()?;
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
    use hoh_agents::process::{Config, Event, Process};
    let blocking = |e: tokio::task::JoinError| CheckError::Io(format!("the check stopped: {e}"));
    let io = CheckError::Io;
    let project = state
        .parent()
        .ok_or_else(|| io(format!("{} has no parent", state.display())))?
        .to_owned();
    // Before: the pre-check, the workspace observed, the record's own new directory.
    let (ready, tree_before, record_id, record_dir, timeout, export) = {
        let reader = store.reader().map_err(CheckError::Store)?;
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
            Ok((
                ready,
                tree_before,
                record_id,
                record_dir,
                timeout,
                project.join(&row.source_path),
            ))
        })
        .await
        .map_err(blocking)??
    };
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
    let (exit_code, timed_out) = match Process::spawn(command, config) {
        Ok(mut process) => {
            let mut report = None;
            while let Some(event) = process.next().await {
                if let Event::Exit(r) = event {
                    report = Some(r);
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
    let done = {
        let (checkout, request, project, ready) = (
            checkout.to_owned(),
            request.clone(),
            project.clone(),
            ready.clone(),
        );
        let (record_dir, record_id, export) =
            (record_dir.clone(), record_id.clone(), export.clone());
        tokio::task::spawn_blocking(move || -> Result<Done, CheckError> {
            let io = |e: String| CheckError::Io(e);
            let integrity = differences(&checkout, &ready.sealed.manifest)?;
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
                    Ok(m) if m.file_type().is_file() => {
                        let r = file(&f)?;
                        artifacts.push(NamedRef {
                            name: name.clone(),
                            path: r.path,
                            sha256: r.sha256,
                        });
                    }
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
            let execution = record_dir.join("execution.json");
            let bytes = serde_json::to_vec_pretty(&record).map_err(|e| io(e.to_string()))?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&execution)
                .and_then(|mut f| {
                    std::io::Write::write_all(&mut f, &bytes).and_then(|()| f.sync_all())
                })
                .map_err(|e| io(format!("{}: {e}", execution.display())))?;
            // Observations (decision 8): the result lines; without any, the declared artifacts found with the
            // record's own conclusion.
            let mut observations = Vec::new();
            if let Some(results) = record
                .artifacts
                .iter()
                .find(|a| a.name == "test-results.jsonl")
            {
                // As bytes, line by line, as the gate and the replica read it: a line that is not JSON (not UTF-8,
                // say) stays a line, and is not a result line (rule 1).
                let bytes = std::fs::read(project.join(&results.path)).unwrap_or_default();
                let blank = |l: &[u8]| l.iter().all(|b| b" \t\n\r\x0b\x0c".contains(b));
                for line in bytes.split(|b| *b == b'\n').filter(|l| !blank(l)) {
                    observations.push(serde_json::from_slice(line).unwrap_or_else(|_| {
                        Value::String(String::from_utf8_lossy(line).into_owned())
                    }));
                }
            }
            if observations.is_empty() {
                observations = hoh_core::verification::fallback(&record);
            }
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
            let record_object = keep(&execution, "execution", "execution.json")?;
            let git_tree = tree_after["head_tree"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            Ok(Done {
                clock: serde_json::json!({
                    "wall_finished_at": hoh_core::time::rfc3339_millis_utc(wall_finished),
                    "monotonic_seconds": duration,
                }),
                evidence: Evidence {
                    record,
                    git_tree,
                    record_dir: rel(&record_dir),
                    observations,
                },
                tree_before,
                tree_after,
                integrity,
                objects,
                record_object,
            })
        })
        .await
        .map_err(blocking)??
    };
    // The judgement (principle one) and its rows (decision 9).
    let (recording, ran) = {
        let reader = store.reader().map_err(CheckError::Store)?;
        let (project, state, ready, checkout) = (
            project.clone(),
            state.to_owned(),
            ready.clone(),
            checkout.to_owned(),
        );
        let record_dir = record_dir.clone();
        tokio::task::spawn_blocking(move || -> Result<(Recording, Ran), CheckError> {
            let m = &ready.sealed.manifest;
            let plan: Value = hoh_core::objects::get(&project, &m.test_plan.sha256)
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
                .map_err(CheckError::Io)?;
            let policy = m
                .source_files
                .iter()
                .any(|f| f.path == "contracts/release-policy.json")
                .then(|| std::fs::read(checkout.join("contracts/release-policy.json")).ok())
                .flatten()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            let world = World {
                reader: &reader,
                project: &project,
                evidence: &state.join("evidence"),
                plan: &plan,
                policy,
            };
            let e = &done.evidence;
            let verdict = valid(e, &ready.sealed, &m.spec.sha256, &world);
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
            if let Some(acceptance) = reader
                .acceptance(&ready.run_id)
                .map_err(CheckError::Store)?
                && acceptance.spec_sha256 == m.spec.sha256
            {
                let spec: Value = hoh_core::objects::get(&project, &m.spec.sha256)
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
                record_dir,
                outcome,
                reasons: reasons.clone(),
            };
            Ok((
                Recording {
                    record: e.record.clone(),
                    record_object: done.record_object.clone(),
                    objects: done.objects.clone(),
                    outcome,
                    reasons,
                    tree_before: done.tree_before.clone(),
                    tree_after: done.tree_after.clone(),
                    clock: done.clock.clone(),
                    observations: e.observations.clone(),
                    rows: rows_out,
                    integrity: done.integrity.clone(),
                },
                ran,
            ))
        })
        .await
        .map_err(blocking)??
    };
    store
        .record_evidence(recording)
        .await
        .map_err(CheckError::Store)?;
    Ok(ran)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoh_agents::process::{Cause, Termination};

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
