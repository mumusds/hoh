//! Frozen candidates and evidence validity (REQ-052, docs/adr/ADR-027.md): the candidate manifest and its canonical
//! identity, byte-equal to tools/candidate.py's, so a product export passes the bootstrap tools and the release gate.
//! Pure: no I/O.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A file of the candidate by its path relative to the repository and the sha256 of its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    pub path: String,
    pub sha256: String,
}

/// What a candidate binds before it is sealed: the commit and its tree, the specification, environment and test plan
/// files, and every tracked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub git_commit: String,
    pub git_tree: String,
    pub spec: FileRef,
    pub environment: FileRef,
    pub test_plan: FileRef,
    pub source_files: Vec<FileRef>,
}

/// A sealed candidate: its manifest and its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub candidate_id: String,
    pub manifest: Manifest,
}

impl Manifest {
    /// Sealed with its identity (decision 2): its source files sorted by path.
    pub fn seal(mut self) -> Sealed {
        self.source_files.sort_by(|a, b| a.path.cmp(&b.path));
        let candidate_id = identity(&body(&self));
        Sealed {
            candidate_id,
            manifest: self,
        }
    }
}

/// The export without its id: what the identity hashes.
fn body(m: &Manifest) -> Value {
    json!({
        "schema_version": "1.0",
        "git_commit": m.git_commit,
        "git_tree": m.git_tree,
        "spec": m.spec,
        "environment": m.environment,
        "test_plan": m.test_plan,
        "spec_sha256": m.spec.sha256,
        "environment_sha256": m.environment.sha256,
        "test_plan_sha256": m.test_plan.sha256,
        "source_files": m.source_files,
    })
}

/// Whether `value` has candidate.schema.json's shape: the in-crate validator cannot read the schema's `^(?!/)` path
/// pattern (no lookaround), so the export's shape is checked here, and an integration test validates exports with
/// Python's jsonschema against the locked schema itself.
fn shape(value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("a candidate export is an object")?;
    let keys = [
        "schema_version",
        "candidate_id",
        "git_commit",
        "git_tree",
        "spec_sha256",
        "environment_sha256",
        "test_plan_sha256",
        "spec",
        "environment",
        "test_plan",
        "source_files",
    ];
    if object.len() != keys.len() || keys.iter().any(|k| !object.contains_key(*k)) {
        return Err(format!(
            "a candidate export has exactly the fields {keys:?}"
        ));
    }
    if value["schema_version"] != "1.0" {
        return Err("schema_version is not 1.0".to_owned());
    }
    let hex = |v: &Value, lengths: &[usize]| {
        v.as_str().is_some_and(|s| {
            lengths.contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    };
    let id = value["candidate_id"].as_str().unwrap_or_default();
    if !id
        .strip_prefix("CAND-")
        .is_some_and(|h| hex(&json!(h), &[64]))
    {
        return Err(format!("{id:?} is not a candidate id"));
    }
    for key in ["git_commit", "git_tree"] {
        if !hex(&value[key], &[40, 64]) {
            return Err(format!("{key} is not a git object id"));
        }
    }
    for key in ["spec_sha256", "environment_sha256", "test_plan_sha256"] {
        if !hex(&value[key], &[64]) {
            return Err(format!("{key} is not a sha256"));
        }
    }
    let file = |v: &Value| {
        v.as_object().is_some_and(|o| o.len() == 2)
            && v["path"]
                .as_str()
                .is_some_and(|p| !p.is_empty() && !p.starts_with('/'))
            && hex(&v["sha256"], &[64])
    };
    for key in ["spec", "environment", "test_plan"] {
        if !file(&value[key]) {
            return Err(format!("{key} is not a file reference"));
        }
    }
    let files = value["source_files"]
        .as_array()
        .ok_or("source_files is not a list")?;
    if files.is_empty() || !files.iter().all(file) {
        return Err("source_files is empty or holds what is not a file reference".to_owned());
    }
    Ok(())
}

/// `CAND-` and the sha256 of `body`'s canonical JSON: keys sorted (serde_json's maps are ordered), no whitespace,
/// non-ASCII as it is: what tools/candidate.py's `json.dumps(sort_keys=True, separators=(',',':'),
/// ensure_ascii=False)` writes.
pub fn identity(body: &Value) -> String {
    let mut body = body.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove("candidate_id");
    }
    let canonical = body.to_string();
    let digest: String = Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("CAND-{digest}")
}

impl Sealed {
    /// As candidate.schema.json exports it.
    pub fn to_value(&self) -> Value {
        let mut value = body(&self.manifest);
        if let Some(object) = value.as_object_mut() {
            object.insert("candidate_id".to_owned(), json!(self.candidate_id));
        }
        value
    }

    /// One read back from its export, refused when it is not a candidate export, its digests disagree with its files,
    /// or its id does not match its content.
    pub fn from_value(value: &Value) -> Result<Sealed, String> {
        shape(value)?;
        let field = |k: &str| -> Result<FileRef, String> {
            serde_json::from_value(value[k].clone()).map_err(|e| format!("{k}: {e}"))
        };
        let manifest = Manifest {
            git_commit: value["git_commit"].as_str().unwrap_or_default().to_owned(),
            git_tree: value["git_tree"].as_str().unwrap_or_default().to_owned(),
            spec: field("spec")?,
            environment: field("environment")?,
            test_plan: field("test_plan")?,
            source_files: serde_json::from_value(value["source_files"].clone())
                .map_err(|e| e.to_string())?,
        };
        for key in ["spec", "environment", "test_plan"] {
            if value[key]["sha256"] != value[format!("{key}_sha256")] {
                return Err(format!("candidate {key} hash mismatch"));
            }
        }
        let candidate_id = value["candidate_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if candidate_id != identity(value) {
            return Err("candidate identity mismatch".to_owned());
        }
        Ok(Sealed {
            candidate_id,
            manifest,
        })
    }
}

/// A file a record names by its role in the check (contracts/execution.schema.json `artifacts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedRef {
    pub name: String,
    pub path: String,
    pub sha256: String,
}

/// Who ran a check (contracts/execution.schema.json `runner`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runner {
    pub tool: String,
    pub euid: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
}

/// A check's execution record exactly as the locked contracts/execution.schema.json 1.1 has it (no field added,
/// docs/adr/ADR-027.md decision 8): what the product writes and what tools/release_gate.py reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: String,
    pub record_type: String,
    pub record_id: String,
    pub check_id: String,
    pub candidate_id: String,
    pub spec_sha256: String,
    pub mode: String,
    pub command: Vec<String>,
    pub cwd: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_seconds: f64,
    pub exit_code: i64,
    pub timed_out: bool,
    pub stdout: FileRef,
    pub stderr: FileRef,
    pub candidate_manifest: FileRef,
    pub source_unchanged: bool,
    pub environment_sha256: String,
    pub test_plan_sha256: String,
    pub artifacts: Vec<NamedRef>,
    pub missing_artifacts: Vec<String>,
    pub producer_attempt_id: String,
    pub runner: Runner,
}

impl Record {
    /// One read from its JSON, refused unless it has the schema's shape: the in-crate validator cannot read the
    /// schema's `^(?!/)` patterns (no lookaround), so the shape is checked here and an integration test validates
    /// records with Python's jsonschema against the locked schema itself.
    pub fn from_value(value: &Value) -> Result<Record, String> {
        let record: Record = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        let hex64 = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let relative = |p: &str| !p.is_empty() && !p.starts_with('/');
        let bounded = |s: &str| (1..=160).contains(&s.len());
        let checks: [(bool, &str); 17] = [
            (record.schema_version == "1.1", "schema_version is not 1.1"),
            (
                record.record_type == "command_execution",
                "record_type is not command_execution",
            ),
            (bounded(&record.record_id), "record_id is empty or too long"),
            (bounded(&record.check_id), "check_id is empty or too long"),
            (
                record.candidate_id.strip_prefix("CAND-").is_some_and(hex64),
                "candidate_id is not a candidate id",
            ),
            (
                [
                    &record.spec_sha256,
                    &record.environment_sha256,
                    &record.test_plan_sha256,
                ]
                .iter()
                .all(|s| hex64(s)),
                "a digest is not a sha256",
            ),
            (
                ["mock", "live", "product", "bundle"].contains(&record.mode.as_str()),
                "mode is not one of the schema's",
            ),
            (!record.command.is_empty(), "command is empty"),
            (relative(&record.cwd), "cwd is not relative"),
            (
                crate::schema::is_date_time(&record.started_at)
                    && crate::schema::is_date_time(&record.finished_at),
                "a time is not a date-time",
            ),
            (
                record.duration_seconds.is_finite() && record.duration_seconds >= 0.0,
                "duration_seconds is negative",
            ),
            (
                [&record.stdout, &record.stderr, &record.candidate_manifest]
                    .iter()
                    .all(|r| relative(&r.path) && hex64(&r.sha256)),
                "a file reference is not relative or its digest not a sha256",
            ),
            (
                record
                    .artifacts
                    .iter()
                    .all(|a| !a.name.is_empty() && relative(&a.path) && hex64(&a.sha256)),
                "an artifact reference is malformed",
            ),
            (
                record.missing_artifacts.iter().all(|m| !m.is_empty()),
                "a missing artifact has no name",
            ),
            (
                bounded(&record.producer_attempt_id),
                "producer_attempt_id is empty or too long",
            ),
            (!record.runner.tool.is_empty(), "runner.tool is empty"),
            (
                record.runner.boot_id.is_some() || value["runner"].get("boot_id").is_none(),
                "runner.boot_id is not a string",
            ),
        ];
        match checks.iter().find(|(ok, _)| !ok) {
            Some((_, why)) => Err((*why).to_owned()),
            None => Ok(record),
        }
    }
}

/// A check's result as evidence (REQ-052 behaviour 6; docs/adr/ADR-027.md decision 8): its execution record, the git
/// tree the check observed, the record's own directory and its observations (test-result lines, or the declared
/// artifacts and the record's own conclusion for a check whose plan declares no test results).
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub record: Record,
    pub git_tree: String,
    /// Relative to the checkout root; every file the record names is inside it.
    pub record_dir: String,
    pub observations: Vec<Value>,
}

/// The frozen check plan's entry for one check (contracts/check-plan.schema.json).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub argv: Vec<String>,
    pub mode: String,
    pub cwd: String,
    pub artifacts: Vec<String>,
}

/// The attempt that produced evidence: its run and its task's role (principle two).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Producer {
    pub run_id: String,
    pub role: String,
}

/// A check's entry in the candidate's release policy (contracts/release-policy.json `checks`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub mode: String,
    pub degradable: bool,
    pub requires_plugins: bool,
}

/// What the candidate's release policy says of a check (docs/adr/ADR-027.md principle one: a candidate without
/// `contracts/release-policy.json` has no policy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyCheck {
    /// The candidate has no release policy: nothing degrades, nothing requires plugins.
    NoPolicy,
    /// The policy has no such check.
    Missing,
    Check(Policy),
}

/// What evidence is judged against beyond the candidate and spec: the store's attempts and candidates, the files as
/// they are now, and the frozen check plan.
pub trait Lookup {
    /// The attempt `attempt_id`'s run and task role, if it exists.
    fn producer(&self, attempt_id: &str) -> Option<Producer>;
    /// The run the candidate `candidate_id` belongs to.
    fn run_of(&self, candidate_id: &str) -> Option<String>;
    /// The sha256 of the file at `path` (relative to the checkout root) now.
    fn sha256_of(&self, path: &str) -> Option<String>;
    /// The id the candidate export at `path` computes, if it is one.
    fn candidate_at(&self, path: &str) -> Option<String>;
    /// Whether `path`, resolved (`..` and links), is inside the directory `dir`.
    fn inside(&self, path: &str, dir: &str) -> bool;
    /// Whether another record directory than `record_dir` uses the record id `record_id` (rule 19).
    fn record_id_reused(&self, record_id: &str, record_dir: &str) -> bool;
    /// Whether the directory `dir` is, holds or lies in another record's directory.
    fn overlaps(&self, dir: &str) -> bool;
    /// The frozen plan's entry for `check_id`.
    fn planned(&self, check_id: &str) -> Option<Planned>;
    /// The live-sessions document at `path`.
    fn live_sessions(&self, path: &str) -> Option<Value>;
    /// The candidate's release policy on `check_id`.
    fn policy(&self, check_id: &str) -> PolicyCheck;
}

/// Why evidence does not count: a closed set (docs/adr/ADR-027.md decision 8). The first eleven are AC-052-04's;
/// the rest are tools/release_gate.py's other rules for one record, and the producer's (principle two).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    CandidateMismatch,
    TreeMismatch,
    SpecMismatch,
    EnvironmentMismatch,
    TestPlanMismatch,
    ProducerMissing,
    IntervalInvalid,
    SourceChanged,
    OutputChanged,
    ArtifactMissing,
    NoObservations,
    ErrorOutputChanged,
    ArtifactChanged,
    ArtifactsDifferFromPlan,
    ExitInconsistent,
    CommandMismatch,
    ModeMismatch,
    CwdMismatch,
    CheckNotPlanned,
    ManifestMismatch,
    RunnerTool,
    RunnerRoot,
    OutsideRecordDir,
    RecordDirOverlap,
    RecordIdReused,
    LiveSessionsUnbound,
    LiveSessionsMissing,
    ResultLineInvalid,
    CheckNotInPolicy,
    PlanModeNotPolicy,
    ProducerMismatch,
    ProducerNotIndependent,
}

impl Reason {
    /// The code written into event payloads and acceptance_results.reason.
    pub fn code(self) -> &'static str {
        match self {
            Reason::CandidateMismatch => "CANDIDATE_MISMATCH",
            Reason::TreeMismatch => "TREE_MISMATCH",
            Reason::SpecMismatch => "SPEC_MISMATCH",
            Reason::EnvironmentMismatch => "ENVIRONMENT_MISMATCH",
            Reason::TestPlanMismatch => "TEST_PLAN_MISMATCH",
            Reason::ProducerMissing => "PRODUCER_MISSING",
            Reason::IntervalInvalid => "INTERVAL_INVALID",
            Reason::SourceChanged => "SOURCE_CHANGED",
            Reason::OutputChanged => "OUTPUT_CHANGED",
            Reason::ArtifactMissing => "ARTIFACT_MISSING",
            Reason::NoObservations => "NO_OBSERVATIONS",
            Reason::ErrorOutputChanged => "ERROR_OUTPUT_CHANGED",
            Reason::ArtifactChanged => "ARTIFACT_CHANGED",
            Reason::ArtifactsDifferFromPlan => "ARTIFACTS_DIFFER_FROM_PLAN",
            Reason::ExitInconsistent => "EXIT_INCONSISTENT",
            Reason::CommandMismatch => "COMMAND_MISMATCH",
            Reason::ModeMismatch => "MODE_MISMATCH",
            Reason::CwdMismatch => "CWD_MISMATCH",
            Reason::CheckNotPlanned => "CHECK_NOT_PLANNED",
            Reason::ManifestMismatch => "MANIFEST_MISMATCH",
            Reason::RunnerTool => "RUNNER_TOOL",
            Reason::RunnerRoot => "RUNNER_ROOT",
            Reason::OutsideRecordDir => "OUTSIDE_RECORD_DIR",
            Reason::RecordDirOverlap => "RECORD_DIR_OVERLAP",
            Reason::RecordIdReused => "RECORD_ID_REUSED",
            Reason::LiveSessionsUnbound => "LIVE_SESSIONS_UNBOUND",
            Reason::LiveSessionsMissing => "LIVE_SESSIONS_MISSING",
            Reason::ResultLineInvalid => "RESULT_LINE_INVALID",
            Reason::CheckNotInPolicy => "CHECK_NOT_IN_POLICY",
            Reason::PlanModeNotPolicy => "PLAN_MODE_NOT_POLICY",
            Reason::ProducerMismatch => "PRODUCER_MISMATCH",
            Reason::ProducerNotIndependent => "PRODUCER_NOT_INDEPENDENT",
        }
    }
}

/// Why evidence does not count: its class (`INSUFFICIENT` when it observed nothing, else `INVALID`; the evidence
/// outcome is INVALID either way, REQ-052 decision 3) and every reason found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub class: &'static str,
    pub reasons: Vec<Reason>,
}

impl Invalid {
    /// The reasons' codes, in the order found.
    pub fn codes(&self) -> Vec<&'static str> {
        self.reasons.iter().map(|r| r.code()).collect()
    }
}

/// Whether the record `r` claims success: exit 0 and not timed out (principle one, rules 9 and 12).
fn claims_success(r: &Record) -> bool {
    r.exit_code == 0 && !r.timed_out
}

/// The record's interval in seconds, when its times parse and do not run backwards.
fn interval(r: &Record) -> Option<f64> {
    let seconds = |t: &str| {
        crate::time::parse_rfc3339_millis(t)
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
    };
    let (start, end) = seconds(&r.started_at).zip(seconds(&r.finished_at))?;
    (end >= start).then_some(end - start)
}

/// Whether the recorded duration agrees with the interval: within max(5 s, 1 %), as tools/release_gate.py has it.
fn duration_agrees(r: &Record) -> bool {
    interval(r)
        .is_some_and(|elapsed| (elapsed - r.duration_seconds).abs() <= (elapsed * 0.01).max(5.0))
}

/// The live-sessions document of a live record, if it lists one among its artifacts.
fn sessions_of(r: &Record, world: &dyn Lookup) -> Option<Option<Value>> {
    r.artifacts
        .iter()
        .find(|a| a.name == "live-sessions.json")
        .map(|a| world.live_sessions(&a.path))
}

/// Whether `v` has contracts/test-result.schema.json's shape (rule 1): the in-crate validator is not used for the
/// same reason as the record's; an observation naming an `artifact` is not a result line (decision 8).
fn result_line(v: &Value) -> bool {
    let Some(o) = v.as_object() else {
        return false;
    };
    let known = [
        "schema_version",
        "test_id",
        "name",
        "status",
        "duration_ms",
        "detail",
    ];
    let test_id = v["test_id"].as_str().is_some_and(|t| {
        let b = t.as_bytes();
        // ^TEST-[0-9]{3}-[0-9]{2}$
        b.len() == 11
            && t.starts_with("TEST-")
            && b[8] == b'-'
            && b[5..8].iter().chain(&b[9..11]).all(u8::is_ascii_digit)
    });
    o.keys().all(|k| known.contains(&k.as_str()))
        && v["schema_version"] == "1.0"
        && test_id
        && v["name"]
            .as_str()
            .is_some_and(|n| !n.is_empty() && n.chars().count() <= 512)
        && ["PASS", "FAIL", "DEGRADED", "SKIPPED"]
            .contains(&v["status"].as_str().unwrap_or_default())
        && (v
            .get("duration_ms")
            .is_none_or(|d| d.is_null() || d.as_u64().is_some()))
        && v.get("detail")
            .is_none_or(|d| d.as_str().is_some_and(|t| t.chars().count() <= 4096))
}

/// Whether `v` has contracts/live-session.schema.json's shape and binds check `check` and candidate `candidate`.
fn bound(v: &Value, check: &str, candidate: &str) -> bool {
    let text = |x: &Value| x.as_str().is_some_and(|s| !s.is_empty());
    v["schema_version"] == "1.0"
        && v["check_id"] == check
        && v["harness_candidate_id"] == candidate
        && v["sessions"].as_array().is_some_and(|sessions| {
            !sessions.is_empty()
                && sessions.iter().all(|s| {
                    ["codex", "claude", "pi"].contains(&s["agent"].as_str().unwrap_or_default())
                        && text(&s["cli_version"])
                        && text(&s["session_id"])
                        && text(&s["resolved_model"])
                })
        })
}

/// Whether evidence `e` counts for candidate `c` under spec `s` (REQ-052 behaviour 7; docs/adr/ADR-027.md
/// principle one's "veto" and "store" rules, decision 8): every binding matches, its producer is a qa or reviewer
/// attempt of the candidate's run, its record holds every rule tools/release_gate.py applies to one record, and every
/// file it names is what it recorded. Rules 9 (duration) and 12 (artifacts, observations, live sessions) veto only a
/// record that claims success; a timeout and an exit code are conclusions ([`outcome`]). Every reason found is
/// reported, none before another.
pub fn valid(e: &Evidence, c: &Sealed, s: &str, world: &dyn Lookup) -> Result<(), Invalid> {
    let m = &c.manifest;
    let r = &e.record;
    let success = claims_success(r);
    let mut reasons = Vec::new();
    let mut need = |ok: bool, reason: Reason| {
        if !ok {
            reasons.push(reason);
        }
    };
    need(r.candidate_id == c.candidate_id, Reason::CandidateMismatch);
    need(e.git_tree == m.git_tree, Reason::TreeMismatch);
    need(
        r.spec_sha256 == s && s == m.spec.sha256,
        Reason::SpecMismatch,
    );
    need(
        r.environment_sha256 == m.environment.sha256,
        Reason::EnvironmentMismatch,
    );
    need(
        r.test_plan_sha256 == m.test_plan.sha256,
        Reason::TestPlanMismatch,
    );
    match world.producer(&r.producer_attempt_id) {
        None => need(false, Reason::ProducerMissing),
        Some(p) => {
            need(
                world.run_of(&c.candidate_id).as_deref() == Some(p.run_id.as_str()),
                Reason::ProducerMismatch,
            );
            need(
                p.role == "qa" || p.role == "reviewer",
                Reason::ProducerNotIndependent,
            );
        }
    }
    // Rule 9: an interval that runs backwards vetoes (AC-052-04 variant 8); a duration that disagrees with it never
    // does, it only leaves the record incomplete ([`outcome`], M2T10D-701).
    need(interval(r).is_some(), Reason::IntervalInvalid);
    need(r.source_unchanged, Reason::SourceChanged);
    let holds = |f: &str, sha: &str| world.sha256_of(f).as_deref() == Some(sha);
    need(
        holds(&r.stdout.path, &r.stdout.sha256),
        Reason::OutputChanged,
    );
    need(
        holds(&r.stderr.path, &r.stderr.sha256),
        Reason::ErrorOutputChanged,
    );
    need(
        !success || r.missing_artifacts.is_empty(),
        Reason::ArtifactMissing,
    );
    need(
        !success || !e.observations.is_empty(),
        Reason::NoObservations,
    );
    need(
        e.observations
            .iter()
            .filter(|o| o.get("artifact").is_none())
            .all(result_line),
        Reason::ResultLineInvalid,
    );
    need(
        r.artifacts.iter().all(|a| holds(&a.path, &a.sha256)),
        Reason::ArtifactChanged,
    );
    // Rule 11: a record claiming success hides no failure; past its deadline a child's own exit code (0 after SIGTERM)
    // claims nothing, and its FAIL lines are conclusions (M2T10D-854).
    need(
        !success || !e.observations.iter().any(|o| o["status"] == "FAIL"),
        Reason::ExitInconsistent,
    );
    let policy = world.policy(&r.check_id);
    match world.planned(&r.check_id) {
        None => need(false, Reason::CheckNotPlanned),
        Some(plan) => {
            need(r.command == plan.argv, Reason::CommandMismatch);
            need(
                r.mode == plan.mode && ["mock", "live", "product"].contains(&r.mode.as_str()),
                Reason::ModeMismatch,
            );
            let normal = |p: &str| -> std::path::PathBuf {
                std::path::Path::new(p)
                    .components()
                    .filter(|c| !matches!(c, std::path::Component::CurDir))
                    .collect()
            };
            need(normal(&r.cwd) == normal(&plan.cwd), Reason::CwdMismatch);
            let mut accounted: Vec<&str> = r
                .artifacts
                .iter()
                .map(|a| a.name.as_str())
                .chain(r.missing_artifacts.iter().map(String::as_str))
                .collect();
            let mut declared: Vec<&str> = plan.artifacts.iter().map(String::as_str).collect();
            accounted.sort_unstable();
            accounted.dedup();
            declared.sort_unstable();
            declared.dedup();
            need(accounted == declared, Reason::ArtifactsDifferFromPlan);
            if let PolicyCheck::Check(p) = &policy {
                need(p.mode == plan.mode, Reason::PlanModeNotPolicy);
            }
        }
    }
    need(policy != PolicyCheck::Missing, Reason::CheckNotInPolicy);
    need(
        holds(&r.candidate_manifest.path, &r.candidate_manifest.sha256)
            && world.candidate_at(&r.candidate_manifest.path).as_deref()
                == Some(c.candidate_id.as_str()),
        Reason::ManifestMismatch,
    );
    need(
        ["tools/run_check.py", "hoh"].contains(&r.runner.tool.as_str()),
        Reason::RunnerTool,
    );
    need(r.runner.euid != 0, Reason::RunnerRoot);
    need(
        [&r.stdout.path, &r.stderr.path]
            .into_iter()
            .chain(r.artifacts.iter().map(|a| &a.path))
            .all(|p| world.inside(p, &e.record_dir)),
        Reason::OutsideRecordDir,
    );
    need(!world.overlaps(&e.record_dir), Reason::RecordDirOverlap);
    need(
        !world.record_id_reused(&r.record_id, &e.record_dir),
        Reason::RecordIdReused,
    );
    if r.mode == "live" {
        match sessions_of(r, world) {
            // Rule 10: a sessions document there must be of the schema's shape and bound to this check.
            Some(doc) => need(
                doc.is_some_and(|v| bound(&v, &r.check_id, &c.candidate_id)),
                Reason::LiveSessionsUnbound,
            ),
            // Rule 12: a live record claiming success has its sessions, declared by the plan or not (M2T10D-501);
            // declared and missing, it is ARTIFACT_MISSING already.
            None => need(
                !success
                    || r.missing_artifacts
                        .iter()
                        .any(|m| m == "live-sessions.json"),
                Reason::LiveSessionsMissing,
            ),
        }
    }
    match reasons.as_slice() {
        [] => Ok(()),
        [Reason::NoObservations] => Err(Invalid {
            class: "INSUFFICIENT",
            reasons,
        }),
        _ => Err(Invalid {
            class: "INVALID",
            reasons,
        }),
    }
}

/// An evidence's or a conclusion's status (contracts/sql/001_initial.sql `evidence.outcome`,
/// `acceptance_results.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Degraded,
    Fail,
    Blocked,
    Invalid,
}

impl Status {
    pub fn code(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Degraded => "DEGRADED",
            Status::Fail => "FAIL",
            Status::Blocked => "BLOCKED",
            Status::Invalid => "INVALID",
        }
    }
}

/// An evidence's outcome and, where the outcome table names one, its reason (`INSUFFICIENT`, `TIMED_OUT`,
/// `PLUGINS_MISSING`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub status: Status,
    pub reason: Option<&'static str>,
}

/// The outcome of evidence `e` whose validity is `verdict` (docs/adr/ADR-027.md principle one, the outcome function):
/// the first that holds of (1) vetoed → INVALID; (2) timed out → FAIL; (3) a compliant degradation of complete
/// evidence → DEGRADED whatever its exit code; (4) a check requiring plugins whose Pi sessions do not all list them →
/// FAIL; (5) exit 0 → PASS; (6) exit 3 → BLOCKED; (7) FAIL (NOT_STARTED, INTERRUPTED, INCOMPLETE or none). Complete
/// evidence has no artifact missing, a duration that agrees with its interval and, for a live check, its bound sessions
/// (M2T10D-500, -501, -600).
pub fn outcome(e: &Evidence, verdict: &Result<(), Invalid>, world: &dyn Lookup) -> Outcome {
    let r = &e.record;
    let of = |status, reason| Outcome { status, reason };
    if let Err(invalid) = verdict {
        let reason = (invalid.class == "INSUFFICIENT").then_some("INSUFFICIENT");
        return of(Status::Invalid, reason);
    }
    if r.timed_out {
        return of(Status::Fail, Some("TIMED_OUT"));
    }
    let policy = match world.policy(&r.check_id) {
        PolicyCheck::Check(p) => Some(p),
        _ => None,
    };
    let sessions = if r.mode == "live" {
        sessions_of(r, world).flatten()
    } else {
        None
    };
    // Complete evidence: no artifact missing, a live check's bound sessions, a duration that agrees with its interval
    // (M2T10D-500, -501, -600).
    let complete = r.missing_artifacts.is_empty()
        && (r.mode != "live" || sessions.is_some())
        && duration_agrees(r);
    // A compliant degradation (rule 14): a degradable check, at least one DEGRADED line, each with its basis, and no
    // FAIL line; SKIPPED lines leave their TESTs without a conclusion, as the gate judges each AC (M2T10D-551).
    // Its diagnostic: a non-empty output of the record (rule 14, M2T10D-704).
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let diagnosed = [&r.stdout.sha256, &r.stderr.sha256]
        .into_iter()
        .chain(r.artifacts.iter().map(|a| &a.sha256))
        .any(|sha| sha != empty);
    let compliant = diagnosed
        && policy.as_ref().is_some_and(|p| p.degradable)
        && e.observations.iter().any(|o| o["status"] == "DEGRADED")
        && e.observations
            .iter()
            .filter(|o| o["status"] == "DEGRADED")
            .all(|o| based(&o))
        && !e.observations.iter().any(|o| o["status"] == "FAIL");
    if compliant && complete {
        return of(Status::Degraded, None);
    }
    if r.exit_code == 0 {
        let plugins = policy.as_ref().is_none_or(|p| !p.requires_plugins)
            || sessions.as_ref().is_some_and(|v| {
                v["sessions"].as_array().is_some_and(|all| {
                    all.iter()
                        .filter(|s| s["agent"] == "pi")
                        .all(|s| s["plugins"].as_array().is_some_and(|p| !p.is_empty()))
                })
            });
        if !plugins {
            return of(Status::Fail, Some("PLUGINS_MISSING"));
        }
        if complete {
            return of(Status::Pass, None);
        }
    }
    match r.exit_code {
        3 => of(Status::Blocked, None),
        // Not a test failure (M2T10D-753): the command did not start, or the run was interrupted or killed.
        127 => of(Status::Fail, Some("NOT_STARTED")),
        130 => of(Status::Fail, Some("INTERRUPTED")),
        code if code < 0 => of(Status::Fail, Some("INTERRUPTED")),
        // Exit 0 reaches here only claiming success with a duration that disagrees with its interval (missing
        // artifacts or sessions were vetoed): it proves nothing and a rerun can supply it (M2T10D-801).
        0 => of(Status::Fail, Some("INCOMPLETE")),
        _ => of(Status::Fail, None),
    }
}

/// Whether a DEGRADED line carries its basis: a `detail` naming the reason and the upstream reference (rule 14).
fn based(line: &&Value) -> bool {
    line["detail"]
        .as_str()
        .is_some_and(|d| !d.trim().is_empty())
}

/// What member `e`, of outcome `outcome`, concludes on TEST `test_id` (docs/adr/ADR-027.md principle one, the
/// conclusion function): `scenario` when its check is the TEST's scenario check, `requirement` when the TEST's
/// requirement may degrade, `check` when the policy lets its check degrade. The first that holds of (1) vetoed →
/// INVALID; (2) a FAIL line → FAIL; (3) a DEGRADED line that is not compliant (scenario check, degradable check and
/// requirement, its basis) → FAIL; (4) the scenario check passing with its lines all PASS → PASS; (5) a degraded run's
/// lines all PASS or compliant DEGRADED, one DEGRADED at least → DEGRADED; else nothing (a compliant DEGRADED line in
/// a run that was blocked or timed out among them, M2T10D-551).
pub fn conclude(
    e: &Evidence,
    outcome: &Outcome,
    test_id: &str,
    scenario: bool,
    requirement: bool,
    check: bool,
) -> Option<Status> {
    let lines: Vec<&Value> = e
        .observations
        .iter()
        .filter(|o| o["test_id"] == test_id)
        .collect();
    let degraded: Vec<&&Value> = lines.iter().filter(|o| o["status"] == "DEGRADED").collect();
    let compliant = scenario && requirement && check && degraded.iter().all(|o| based(o));
    if outcome.status == Status::Invalid {
        Some(Status::Invalid)
    } else if lines.iter().any(|o| o["status"] == "FAIL") || (!degraded.is_empty() && !compliant) {
        Some(Status::Fail)
    } else if scenario
        && outcome.status == Status::Pass
        && !lines.is_empty()
        && lines.iter().all(|o| o["status"] == "PASS")
    {
        Some(Status::Pass)
    } else if scenario
        && outcome.status == Status::Degraded
        && !degraded.is_empty()
        && lines
            .iter()
            .all(|o| o["status"] == "PASS" || o["status"] == "DEGRADED")
    {
        Some(Status::Degraded)
    } else {
        None
    }
}

/// An acceptance criterion of the run's spec as the rows need it: its declared TEST, its scenario check, and whether
/// its requirement may degrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcSpec {
    pub ac_id: String,
    pub test_id: String,
    pub check_id: String,
    pub degradable: bool,
}

/// The acceptance rows of evidence `e` of outcome `outcome` on the spec's criteria `acs`, `check` when the policy lets
/// its check degrade (docs/adr/ADR-027.md decision 9).
pub fn rows(e: &Evidence, outcome: &Outcome, acs: &[AcSpec], check: bool) -> Vec<(String, Status)> {
    let checked = &e.record.check_id;
    let named = |test: &str| e.observations.iter().any(|o| o["test_id"] == test);
    acs.iter()
        .filter_map(|ac| {
            let scenario = &ac.check_id == checked;
            let status = if outcome.status == Status::Invalid {
                (scenario || named(&ac.test_id)).then_some(Status::Invalid)
            } else {
                conclude(e, outcome, &ac.test_id, scenario, ac.degradable, check)
            };
            status.map(|s| (ac.ac_id.clone(), s))
        })
        .collect()
}

#[cfg(test)]
mod tests;
