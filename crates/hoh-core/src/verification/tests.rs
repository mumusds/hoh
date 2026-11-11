use super::*;
use serde_json::{Value, json};

type R = Result<(), Box<dyn std::error::Error>>;

fn file(path: &str, n: u8) -> FileRef {
    FileRef {
        path: path.to_owned(),
        sha256: format!("{n:x}").repeat(64).chars().take(64).collect(),
    }
}

fn manifest() -> Manifest {
    Manifest {
        git_commit: "a".repeat(40),
        git_tree: "b".repeat(40),
        spec: file("spec/requirements.json", 1),
        environment: file(".hoh/environment.json", 2),
        test_plan: file("tests/check-plan.json", 3),
        // Out of order and non-ASCII: the identity sorts and keeps them as they are.
        source_files: vec![
            file("src/z.rs", 4),
            file("文档/说明.md", 5),
            file("src/a.rs", 6),
        ],
    }
}

#[test]
fn a_candidates_identity_is_the_sha256_of_its_sorted_compact_json_without_its_id() -> R {
    // REQ-052 behaviour 3, docs/adr/ADR-027.md decision 2: CAND- + sha256(canonical JSON of the manifest minus
    // candidate_id), the same bytes tools/candidate.py hashes (keys sorted, no spaces, non-ASCII unescaped).
    let sealed = manifest().seal();
    let value = sealed.to_value();
    assert_eq!(value["schema_version"], "1.0");
    let id = value["candidate_id"].as_str().ok_or("no id")?;
    assert!(id.starts_with("CAND-") && id.len() == 69, "{id}");
    let mut without = value.clone();
    without
        .as_object_mut()
        .ok_or("not an object")?
        .remove("candidate_id");
    let canonical = serde_json::to_string(&without)?;
    assert!(
        canonical.contains("文档/说明.md"),
        "non-ASCII is not escaped: {canonical}"
    );
    assert!(
        !canonical.contains(": ") && !canonical.contains(", "),
        "compact: {canonical}"
    );
    assert_eq!(
        id,
        format!("CAND-{}", crate::context::sha256_hex(canonical.as_bytes()))
    );
    // Source files sorted by path, each digest copied to its *_sha256 field.
    let paths: Vec<&str> = value["source_files"]
        .as_array()
        .ok_or("no files")?
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert_eq!(paths, ["src/a.rs", "src/z.rs", "文档/说明.md"]);
    assert_eq!(value["spec_sha256"], value["spec"]["sha256"]);
    // The same content, the same id; any change, another.
    assert_eq!(manifest().seal().candidate_id, sealed.candidate_id);
    let mut other = manifest();
    other.source_files[0].sha256 = "f".repeat(64);
    assert_ne!(other.seal().candidate_id, sealed.candidate_id);
    Ok(())
}

#[test]
fn a_sealed_manifest_reads_back_and_a_forged_or_misshapen_export_is_refused() -> R {
    let sealed = manifest().seal();
    let value = sealed.to_value();
    let back = Sealed::from_value(&value)?;
    assert_eq!(back, sealed);
    // An export whose id does not match its content is refused.
    let mut forged = value.clone();
    forged["git_tree"] = json!("c".repeat(40));
    assert!(Sealed::from_value(&forged).is_err());
    // Not the schema's shape: an extra field, an absolute path, an empty inventory.
    let mut extra = value.clone();
    extra["run_id"] = json!("R");
    assert!(Sealed::from_value(&extra).is_err());
    let mut absolute = value.clone();
    absolute["spec"]["path"] = json!("/etc/passwd");
    assert!(Sealed::from_value(&absolute).is_err());
    let mut empty = value.clone();
    empty["source_files"] = json!([]);
    assert!(Sealed::from_value(&empty).is_err());
    Ok(())
}

/// What the checks are judged against: the attempts, candidates, files, plan and live sessions a test world knows.
#[derive(Clone)]
struct World {
    producers: Vec<(String, Producer)>,
    runs: Vec<(String, String)>,
    files: Vec<(String, String)>,
    candidates: Vec<(String, String)>,
    overlapping: Vec<String>,
    /// Record ids another record directory uses too.
    reused: Vec<String>,
    plan: Vec<(String, Planned)>,
    sessions: Vec<(String, Value)>,
    /// `None`: the candidate has no release policy.
    policy: Option<Vec<(String, Policy)>>,
}

impl Lookup for World {
    fn producer(&self, attempt_id: &str) -> Option<Producer> {
        self.producers
            .iter()
            .find(|(a, _)| a == attempt_id)
            .map(|(_, p)| p.clone())
    }

    fn run_of(&self, candidate_id: &str) -> Option<String> {
        self.runs
            .iter()
            .find(|(c, _)| c == candidate_id)
            .map(|(_, r)| r.clone())
    }

    fn sha256_of(&self, path: &str) -> Option<String> {
        self.files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, s)| s.clone())
    }

    fn candidate_at(&self, path: &str) -> Option<String> {
        self.candidates
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, c)| c.clone())
    }

    fn inside(&self, path: &str, dir: &str) -> bool {
        !path.split('/').any(|s| s == "..") && std::path::Path::new(path).starts_with(dir)
    }

    fn record_id_reused(&self, record_id: &str, _record_dir: &str) -> bool {
        self.reused.iter().any(|r| r == record_id)
    }

    fn overlaps(&self, dir: &str) -> bool {
        self.overlapping.iter().any(|d| d == dir)
    }

    fn planned(&self, check_id: &str) -> Option<Planned> {
        self.plan
            .iter()
            .find(|(c, _)| c == check_id)
            .map(|(_, p)| p.clone())
    }

    fn live_sessions(&self, path: &str) -> Option<Value> {
        self.sessions
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, v)| v.clone())
    }

    fn policy(&self, check_id: &str) -> PolicyCheck {
        match &self.policy {
            None => PolicyCheck::NoPolicy,
            Some(checks) => checks
                .iter()
                .find(|(c, _)| c == check_id)
                .map_or(PolicyCheck::Missing, |(_, p)| PolicyCheck::Check(p.clone())),
        }
    }
}

const DIR: &str = ".hoh/evidence/C/unit/r1";

fn e0(c: &Sealed) -> Evidence {
    let m = &c.manifest;
    Evidence {
        record: Record {
            schema_version: "1.1".to_owned(),
            record_type: "command_execution".to_owned(),
            record_id: "r1".to_owned(),
            check_id: "unit".to_owned(),
            candidate_id: c.candidate_id.clone(),
            spec_sha256: m.spec.sha256.clone(),
            mode: "product".to_owned(),
            command: vec!["bash".to_owned(), "scripts/product/unit.sh".to_owned()],
            cwd: ".".to_owned(),
            started_at: "2026-11-11T10:00:00Z".to_owned(),
            finished_at: "2026-11-11T10:00:05Z".to_owned(),
            duration_seconds: 5.0,
            exit_code: 0,
            timed_out: false,
            stdout: file(&format!("{DIR}/stdout.log"), 9),
            stderr: file(&format!("{DIR}/stderr.log"), 10),
            candidate_manifest: file(".hoh/objects/sha256/cc/candidate", 11),
            source_unchanged: true,
            environment_sha256: m.environment.sha256.clone(),
            test_plan_sha256: m.test_plan.sha256.clone(),
            artifacts: vec![NamedRef {
                name: "test-results.jsonl".to_owned(),
                path: format!("{DIR}/artifacts/test-results.jsonl"),
                sha256: "c".repeat(64),
            }],
            missing_artifacts: Vec::new(),
            producer_attempt_id: "T1/attempt-1".to_owned(),
            runner: Runner {
                tool: "hoh".to_owned(),
                euid: 1000,
                boot_id: None,
            },
        },
        git_tree: m.git_tree.clone(),
        record_dir: DIR.to_owned(),
        observations: vec![line("TEST-052-04", "PASS")],
    }
}

/// The world in which `e0(c)` is valid: its producer a qa attempt of the candidate's run, every file it names as it
/// recorded, its check planned as it ran.
fn world0(c: &Sealed, e: &Evidence) -> World {
    let r = &e.record;
    let mut files = vec![
        (r.stdout.path.clone(), r.stdout.sha256.clone()),
        (r.stderr.path.clone(), r.stderr.sha256.clone()),
        (
            r.candidate_manifest.path.clone(),
            r.candidate_manifest.sha256.clone(),
        ),
    ];
    files.extend(
        r.artifacts
            .iter()
            .map(|a| (a.path.clone(), a.sha256.clone())),
    );
    World {
        producers: vec![(
            r.producer_attempt_id.clone(),
            Producer {
                run_id: "R1".to_owned(),
                role: "qa".to_owned(),
            },
        )],
        runs: vec![(c.candidate_id.clone(), "R1".to_owned())],
        files,
        candidates: vec![(r.candidate_manifest.path.clone(), c.candidate_id.clone())],
        overlapping: Vec::new(),
        reused: Vec::new(),
        plan: vec![(
            r.check_id.clone(),
            Planned {
                argv: r.command.clone(),
                mode: r.mode.clone(),
                cwd: ".".to_owned(),
                artifacts: r.artifacts.iter().map(|a| a.name.clone()).collect(),
            },
        )],
        sessions: Vec::new(),
        policy: None,
    }
}

#[test]
fn test_052_04_evidence_is_valid_only_when_every_binding_holds() -> R {
    // AC-052-04 (REQ-052 behaviour 7, docs/adr/ADR-027.md decision 5): e0 valid; each of the 11 tampered aspects not
    // valid, observations=[] for INSUFFICIENT, the others INVALID, each with its own reason code.
    let c = manifest().seal();
    let s = c.manifest.spec.sha256.clone();
    let base = e0(&c);
    let world = world0(&c, &base);
    assert_eq!(valid(&base, &c, &s, &world), Ok(()));
    type Tamper = fn(&mut Evidence);
    let variants: [(&str, Tamper, &str, &str); 11] = [
        (
            "candidate_id",
            |e| e.record.candidate_id = format!("CAND-{}", "0".repeat(64)),
            "INVALID",
            "CANDIDATE_MISMATCH",
        ),
        (
            "git_tree",
            |e| e.git_tree = "e".repeat(40),
            "INVALID",
            "TREE_MISMATCH",
        ),
        (
            "spec_sha256",
            |e| e.record.spec_sha256 = "0".repeat(64),
            "INVALID",
            "SPEC_MISMATCH",
        ),
        (
            "environment_sha256",
            |e| e.record.environment_sha256 = "0".repeat(64),
            "INVALID",
            "ENVIRONMENT_MISMATCH",
        ),
        (
            "test_plan_sha256",
            |e| e.record.test_plan_sha256 = "0".repeat(64),
            "INVALID",
            "TEST_PLAN_MISMATCH",
        ),
        (
            "observations",
            |e| e.observations.clear(),
            "INSUFFICIENT",
            "NO_OBSERVATIONS",
        ),
        (
            "producer attempt",
            |e| e.record.producer_attempt_id = "nobody".to_owned(),
            "INVALID",
            "PRODUCER_MISSING",
        ),
        (
            "finished_at < started_at",
            |e| e.record.finished_at = "2026-11-11T09:59:59Z".to_owned(),
            "INVALID",
            "INTERVAL_INVALID",
        ),
        (
            "source_unchanged",
            |e| e.record.source_unchanged = false,
            "INVALID",
            "SOURCE_CHANGED",
        ),
        (
            "stdout sha256",
            |e| e.record.stdout.sha256 = "0".repeat(64),
            "INVALID",
            "OUTPUT_CHANGED",
        ),
        (
            "missing_artifacts",
            |e| {
                // Declared and not found (found and missing together still the plan's, rule 8).
                e.record.missing_artifacts = vec!["test-results.jsonl".to_owned()];
                e.record.artifacts.clear();
            },
            "INVALID",
            "ARTIFACT_MISSING",
        ),
    ];
    let mut table = Vec::new();
    for (aspect, tamper, class, code) in variants {
        let mut e = base.clone();
        tamper(&mut e);
        let got = valid(&e, &c, &s, &world);
        let Err(invalid) = &got else {
            return Err(format!("{aspect}: valid").into());
        };
        assert_eq!(
            (invalid.class, invalid.codes().as_slice()),
            (class, &[code][..]),
            "{aspect}"
        );
        table.push(json!({"aspect": aspect, "class": invalid.class, "reasons": invalid.codes()}));
    }
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join("test_052_04-table.json"),
            serde_json::to_vec_pretty(&json!({"e0": "valid", "variants": table}))?,
        )?;
    }
    Ok(())
}

#[test]
fn every_rule_the_gate_applies_to_one_record_has_a_reason_of_its_own() -> R {
    // docs/adr/ADR-027.md decision 8 (M2T10D-156, -255) and principle two (-252): beyond AC-052-04's eleven, each of
    // tools/release_gate.py's rules for one record and each producer rule, tampered alone, gives exactly its reason.
    let c = manifest().seal();
    let s = c.manifest.spec.sha256.clone();
    let base = e0(&c);
    let w0 = world0(&c, &base);
    assert_eq!(valid(&base, &c, &s, &w0), Ok(()));
    type Tamper = fn(&mut Evidence, &mut World);
    let variants: [(&str, Tamper, Reason); 23] = [
        (
            "a record id another record directory uses",
            |e, w| w.reused.push(e.record.record_id.clone()),
            Reason::RecordIdReused,
        ),
        (
            "a result line not of the schema's shape",
            |e, _| {
                e.observations
                    .push(json!({"schema_version": "1.0", "test_id": "TEST-052-04", "name": "x", "status": "MAYBE"}));
            },
            Reason::ResultLineInvalid,
        ),
        (
            "stderr sha256",
            |e, _| e.record.stderr.sha256 = "0".repeat(64),
            Reason::ErrorOutputChanged,
        ),
        (
            "artifact sha256",
            |e, _| e.record.artifacts[0].sha256 = "0".repeat(64),
            Reason::ArtifactChanged,
        ),
        (
            "artifact names",
            |e, _| e.record.artifacts[0].name = "other.jsonl".to_owned(),
            Reason::ArtifactsDifferFromPlan,
        ),
        (
            "exit 0 with a failing test",
            |e, _| e.observations.push(line("TEST-052-04", "FAIL")),
            Reason::ExitInconsistent,
        ),
        (
            "command",
            |e, _| e.record.command.push("--extra".to_owned()),
            Reason::CommandMismatch,
        ),
        (
            "mode",
            |e, _| e.record.mode = "mock".to_owned(),
            Reason::ModeMismatch,
        ),
        (
            "bundle mode, even when planned",
            |e, w| {
                e.record.mode = "bundle".to_owned();
                w.plan[0].1.mode = "bundle".to_owned();
            },
            Reason::ModeMismatch,
        ),
        (
            "cwd",
            |e, _| e.record.cwd = "sub".to_owned(),
            Reason::CwdMismatch,
        ),
        (
            "check not in the plan",
            |_, w| w.plan.clear(),
            Reason::CheckNotPlanned,
        ),
        (
            "candidate manifest digest",
            |e, _| e.record.candidate_manifest.sha256 = "0".repeat(64),
            Reason::ManifestMismatch,
        ),
        (
            "candidate manifest of another candidate",
            |_, w| w.candidates[0].1 = format!("CAND-{}", "0".repeat(64)),
            Reason::ManifestMismatch,
        ),
        (
            "runner tool",
            |e, _| e.record.runner.tool = "make".to_owned(),
            Reason::RunnerTool,
        ),
        (
            "runner root",
            |e, _| e.record.runner.euid = 0,
            Reason::RunnerRoot,
        ),
        (
            "output outside its record",
            |e, w| {
                e.record.stdout.path = format!("{DIR}/../r2/stdout.log");
                w.files
                    .push((e.record.stdout.path.clone(), e.record.stdout.sha256.clone()));
            },
            Reason::OutsideRecordDir,
        ),
        (
            "record directory overlapping another",
            |e, w| w.overlapping.push(e.record_dir.clone()),
            Reason::RecordDirOverlap,
        ),
        (
            "producer of another run",
            |_, w| w.producers[0].1.run_id = "R2".to_owned(),
            Reason::ProducerMismatch,
        ),
        (
            "producer the developer",
            |_, w| w.producers[0].1.role = "developer".to_owned(),
            Reason::ProducerNotIndependent,
        ),
        (
            "a live check claiming success without its sessions",
            |e, w| {
                e.record.mode = "live".to_owned();
                w.plan[0].1.mode = "live".to_owned();
                w.plan[0].1.artifacts.push("live-sessions.json".to_owned());
                e.record.missing_artifacts = vec!["live-sessions.json".to_owned()];
            },
            Reason::ArtifactMissing,
        ),
        (
            "check not in the release policy",
            |_, w| w.policy = Some(Vec::new()),
            Reason::CheckNotInPolicy,
        ),
        (
            "planned in another mode than the policy's",
            |_, w| {
                w.policy = Some(vec![(
                    "unit".to_owned(),
                    Policy {
                        mode: "mock".to_owned(),
                        degradable: false,
                        requires_plugins: false,
                    },
                )]);
            },
            Reason::PlanModeNotPolicy,
        ),
        (
            "found and missing artifacts not the plan's (a failed run, so nothing is claimed missing)",
            |e, _| {
                e.record.exit_code = 1;
                e.record.missing_artifacts = vec!["report.json".to_owned()];
            },
            Reason::ArtifactsDifferFromPlan,
        ),
    ];
    for (aspect, tamper, reason) in variants {
        let mut e = base.clone();
        let mut w = w0.clone();
        tamper(&mut e, &mut w);
        let got = valid(&e, &c, &s, &w);
        assert_eq!(
            got,
            Err(Invalid {
                class: "INVALID",
                reasons: vec![reason]
            }),
            "{aspect}"
        );
    }
    Ok(())
}

#[test]
fn a_live_record_counts_only_with_sessions_bound_to_its_check_and_candidate() -> R {
    let c = manifest().seal();
    let s = c.manifest.spec.sha256.clone();
    let mut e = e0(&c);
    e.record.mode = "live".to_owned();
    e.record.artifacts.push(NamedRef {
        name: "live-sessions.json".to_owned(),
        path: format!("{DIR}/artifacts/live-sessions.json"),
        sha256: "d".repeat(64),
    });
    let mut w = world0(&c, &e);
    w.plan[0].1.mode = "live".to_owned();
    let path = e.record.artifacts[1].path.clone();
    // A document of the schema's shape: one session at least.
    let bound = json!({"schema_version": "1.0", "check_id": "unit", "harness_candidate_id": c.candidate_id,
                       "sessions": [{"agent": "codex", "cli_version": "1", "session_id": "s1", "resolved_model": "m"}]});
    w.sessions.push((path.clone(), bound.clone()));
    assert_eq!(valid(&e, &c, &s, &w), Ok(()));
    for (key, other) in [
        ("check_id", json!("adapter-live")),
        ("harness_candidate_id", json!("CAND-x")),
        ("sessions", json!([])),
    ] {
        let mut v = bound.clone();
        v[key] = other;
        let mut w2 = w.clone();
        w2.sessions = vec![(path.clone(), v)];
        assert_eq!(
            valid(&e, &c, &s, &w2).map_err(|i| i.codes()),
            Err(vec!["LIVE_SESSIONS_UNBOUND"]),
            "{key}"
        );
    }
    Ok(())
}

#[test]
fn every_reason_is_found_together_and_none_hides_another() -> R {
    // Principle one (docs/adr/ADR-027.md, M2T10D-200): a record's faults are all listed, none ranked before another.
    let c = manifest().seal();
    let s = c.manifest.spec.sha256.clone();
    let mut e = e0(&c);
    let mut w = world0(&c, &e);
    e.record.stderr.sha256 = "0".repeat(64);
    e.record.runner.euid = 0;
    e.record.candidate_id = format!("CAND-{}", "0".repeat(64));
    w.producers[0].1.role = "developer".to_owned();
    let got = valid(&e, &c, &s, &w).map_err(|i| i.codes());
    assert_eq!(
        got,
        Err(vec![
            "CANDIDATE_MISMATCH",
            "PRODUCER_NOT_INDEPENDENT",
            "ERROR_OUTPUT_CHANGED",
            "RUNNER_ROOT"
        ])
    );
    Ok(())
}

#[test]
fn a_record_is_read_only_in_the_schemas_shape() -> R {
    let c = manifest().seal();
    let e = e0(&c);
    let value = serde_json::to_value(&e.record)?;
    assert_eq!(Record::from_value(&value)?, e.record);
    let mut extra = value.clone();
    extra["git_tree"] = json!("b".repeat(40));
    assert!(Record::from_value(&extra).is_err(), "no field is added");
    for (key, bad) in [
        ("schema_version", json!("1.0")),
        ("record_type", json!("other")),
        ("candidate_id", json!("CAND-1")),
        ("mode", json!("fake")),
        ("command", json!([])),
        ("cwd", json!("/abs")),
        ("started_at", json!("yesterday")),
        ("duration_seconds", json!(-1)),
        ("record_id", json!("")),
    ] {
        let mut v = value.clone();
        v[key] = bad;
        assert!(Record::from_value(&v).is_err(), "{key}");
    }
    let mut stdout = value.clone();
    stdout["stdout"]["path"] = json!("/etc/passwd");
    assert!(Record::from_value(&stdout).is_err());
    Ok(())
}

/// A test-result line.
fn line(test: &str, status: &str) -> Value {
    json!({"schema_version": "1.0", "test_id": test, "name": format!("{test} case"), "status": status})
}

fn degraded(test: &str) -> Value {
    let mut v = line(test, "DEGRADED");
    v["detail"] = json!("pi plugin x unavailable; upstream: https://example.invalid/issue/1");
    v
}

fn degradable_policy() -> Option<Vec<(String, Policy)>> {
    Some(vec![(
        "unit".to_owned(),
        Policy {
            mode: "product".to_owned(),
            degradable: true,
            requires_plugins: false,
        },
    )])
}

#[test]
fn a_run_that_does_not_claim_success_may_lack_its_artifacts_observations_and_duration() -> R {
    // docs/adr/ADR-027.md principle one, rules 9 and 12 (M2T10D-450, -502): missing artifacts, no observations and a
    // duration that disagrees with its interval veto only a record that claims success (exit 0, not timed out); a
    // failed, blocked, interrupted or timed-out run is valid evidence of that.
    let c = manifest().seal();
    let s = c.manifest.spec.sha256.clone();
    for (exit, timed_out) in [(1, false), (3, false), (130, false), (124, true)] {
        let mut e = e0(&c);
        let w = world0(&c, &e);
        e.record.exit_code = exit;
        e.record.timed_out = timed_out;
        e.record.missing_artifacts = vec!["test-results.jsonl".to_owned()];
        e.record.artifacts.clear();
        e.observations.clear();
        // tools/run_check.py writes 0 for an interrupted run's duration: here over a 30 s interval.
        e.record.finished_at = "2026-11-11T10:00:30Z".to_owned();
        e.record.duration_seconds = 0.0;
        assert_eq!(valid(&e, &c, &s, &w), Ok(()), "exit {exit}");
    }
    // A duration that disagrees never vetoes, not even a record claiming success (a clock changed or the host
    // slept): the record is incomplete, so it satisfies nothing (rule 9, M2T10D-701, -655).
    let mut e = e0(&c);
    let w = world0(&c, &e);
    e.record.finished_at = "2026-11-11T10:00:30Z".to_owned();
    e.record.duration_seconds = 0.0;
    let verdict = valid(&e, &c, &s, &w);
    assert_eq!(verdict, Ok(()));
    let o = outcome(&e, &verdict, &w);
    // It fails for a reason that is no test failure: a rerun on the same candidate can supply it (M2T10D-801).
    assert_eq!((o.status.code(), o.reason), ("FAIL", Some("INCOMPLETE")));
    assert_eq!(
        conclude(&e, &o, "TEST-052-04", true, false, false),
        None,
        "its PASS line proves nothing"
    );
    Ok(())
}

#[test]
fn a_timed_out_run_exiting_0_with_a_failing_line_is_a_failure_not_a_veto() -> R {
    // Rule 11 vetoes a FAIL line only in a record that claims success (exit 0, not timed out): past its deadline the
    // product records the child's own exit code, which may be 0 after SIGTERM (M2T10D-854).
    let c = manifest().seal();
    let s = c.manifest.spec.sha256.clone();
    let mut e = e0(&c);
    let w = world0(&c, &e);
    e.observations.push(line("TEST-052-04", "FAIL"));
    assert_eq!(
        valid(&e, &c, &s, &w).map_err(|i| i.codes()),
        Err(vec!["EXIT_INCONSISTENT"])
    );
    e.record.timed_out = true;
    let verdict = valid(&e, &c, &s, &w);
    assert_eq!(verdict, Ok(()));
    let o = outcome(&e, &verdict, &w);
    assert_eq!((o.status.code(), o.reason), ("FAIL", Some("TIMED_OUT")));
    assert_eq!(
        conclude(&e, &o, "TEST-052-04", true, false, false),
        Some(Status::Fail)
    );
    Ok(())
}

#[test]
fn the_outcome_is_the_first_of_the_ordered_rules_that_holds() -> R {
    // Principle one's outcome function (M2T10D-254, -302, -401, -402, -451, -500, -501).
    let c = manifest().seal();
    let s = c.manifest.spec.sha256.clone();
    let base = e0(&c);
    let w0 = world0(&c, &base);
    let outcome_of = |e: &Evidence, w: &World| outcome(e, &valid(e, &c, &s, w), w);
    let status = |o: Outcome| (o.status.code(), o.reason);
    assert_eq!(status(outcome_of(&base, &w0)), ("PASS", None));
    // A veto is INVALID; no observations on a run claiming success is INSUFFICIENT.
    let mut e = base.clone();
    e.record.runner.euid = 0;
    assert_eq!(status(outcome_of(&e, &w0)), ("INVALID", None));
    let mut e = base.clone();
    e.observations.clear();
    assert_eq!(
        status(outcome_of(&e, &w0)),
        ("INVALID", Some("INSUFFICIENT"))
    );
    // Exit codes; a run that did not start, was interrupted or killed by a signal fails for a reason that is no test
    // failure (M2T10D-753).
    for (exit, expected, reason) in [
        (0, "PASS", None),
        (3, "BLOCKED", None),
        (1, "FAIL", None),
        (101, "FAIL", None),
        (127, "FAIL", Some("NOT_STARTED")),
        (130, "FAIL", Some("INTERRUPTED")),
        (-15, "FAIL", Some("INTERRUPTED")),
    ] {
        let mut e = base.clone();
        e.record.exit_code = exit;
        assert_eq!(
            status(outcome_of(&e, &w0)),
            (expected, reason),
            "exit {exit}"
        );
    }
    // A compliant degradation is DEGRADED whatever the exit code, never after a timeout, never with an artifact
    // missing.
    let mut wd = w0.clone();
    wd.policy = degradable_policy();
    for exit in [0, 3, 1] {
        let mut e = base.clone();
        e.record.exit_code = exit;
        e.observations = vec![line("TEST-048-01", "PASS"), degraded("TEST-048-02")];
        assert_eq!(
            status(outcome_of(&e, &wd)),
            ("DEGRADED", None),
            "exit {exit}"
        );
        let mut timed = e.clone();
        timed.record.timed_out = true;
        assert_eq!(
            status(outcome_of(&timed, &wd)),
            ("FAIL", Some("TIMED_OUT")),
            "exit {exit} timed out"
        );
        if exit != 0 {
            let mut incomplete = e.clone();
            let mut wi = wd.clone();
            wi.plan[0].1.artifacts.push("report.json".to_owned());
            incomplete.record.missing_artifacts = vec!["report.json".to_owned()];
            assert_ne!(
                outcome_of(&incomplete, &wi).status.code(),
                "DEGRADED",
                "exit {exit} with an artifact missing"
            );
            // A duration that disagrees with its interval: no veto for a run that claims nothing, but no
            // degradation either (M2T10D-600).
            let mut skewed = e.clone();
            skewed.record.finished_at = "2026-11-11T10:00:01Z".to_owned();
            skewed.record.duration_seconds = 200.0;
            assert_eq!(valid(&skewed, &c, &s, &wd), Ok(()), "exit {exit} skewed");
            assert_ne!(
                outcome_of(&skewed, &wd).status.code(),
                "DEGRADED",
                "exit {exit} with its duration skewed"
            );
        }
    }
    // SKIPPED lines beside a compliant degradation leave it DEGRADED (their TESTs conclude nothing); a FAIL line does
    // not (M2T10D-551).
    let mut e = base.clone();
    e.record.exit_code = 3;
    e.observations = vec![degraded("TEST-048-01"), line("TEST-049-01", "SKIPPED")];
    assert_eq!(outcome_of(&e, &wd).status.code(), "DEGRADED");
    e.observations = vec![degraded("TEST-048-01"), line("TEST-049-01", "FAIL")];
    assert_eq!(outcome_of(&e, &wd).status.code(), "BLOCKED");
    // A degradation's diagnostic is a non-empty output of the record: with stdout, stderr and every artifact empty it
    // has none, so it is not compliant (rule 14, M2T10D-704).
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned();
    let mut bare = base.clone();
    bare.record.exit_code = 3;
    bare.observations = vec![degraded("TEST-048-01")];
    bare.record.stdout.sha256 = empty.clone();
    bare.record.stderr.sha256 = empty.clone();
    bare.record.artifacts[0].sha256 = empty.clone();
    let mut we = world0(&c, &bare);
    we.policy = degradable_policy();
    assert_eq!(outcome_of(&bare, &we).status.code(), "BLOCKED");
    // Not compliant: a check the policy does not let degrade, or a DEGRADED line without its basis.
    let mut e = base.clone();
    e.record.exit_code = 3;
    e.observations = vec![degraded("TEST-048-02")];
    assert_eq!(outcome_of(&e, &w0).status.code(), "BLOCKED");
    e.observations = vec![line("TEST-048-02", "DEGRADED")];
    assert_eq!(outcome_of(&e, &wd).status.code(), "BLOCKED");
    // A live check: PASS needs its bound sessions, whether or not the plan declares them; a check that requires
    // plugins passes only when every Pi session lists them.
    let mut e = base.clone();
    e.record.mode = "live".to_owned();
    let mut wl = world0(&c, &e);
    wl.plan[0].1.mode = "live".to_owned();
    assert_eq!(
        status(outcome_of(&e, &wl)),
        ("INVALID", None),
        "live without sessions"
    );
    e.record.artifacts.push(NamedRef {
        name: "live-sessions.json".to_owned(),
        path: format!("{DIR}/artifacts/live-sessions.json"),
        sha256: "d".repeat(64),
    });
    let mut wl = world0(&c, &e);
    wl.plan[0].1.mode = "live".to_owned();
    wl.policy = Some(vec![(
        "unit".to_owned(),
        Policy {
            mode: "live".to_owned(),
            degradable: true,
            requires_plugins: true,
        },
    )]);
    let sessions = |plugins: Value| {
        json!({"schema_version": "1.0", "check_id": "unit", "harness_candidate_id": c.candidate_id,
               "sessions": [{"agent": "pi", "cli_version": "1", "session_id": "p1", "resolved_model": "m",
                             "plugins": plugins}]})
    };
    let path = e.record.artifacts[1].path.clone();
    wl.sessions = vec![(path.clone(), sessions(json!(["hoh"])))];
    assert_eq!(status(outcome_of(&e, &wl)), ("PASS", None));
    wl.sessions = vec![(path, sessions(json!([])))];
    assert_eq!(
        status(outcome_of(&e, &wl)),
        ("FAIL", Some("PLUGINS_MISSING"))
    );
    Ok(())
}

#[test]
fn a_members_conclusion_on_a_test_is_one_function_of_its_outcome_and_lines() -> R {
    // Principle one's conclude(member, TEST) (M2T10D-403, -452, -551): failures count from any check, satisfaction
    // only from the scenario check; a DEGRADED line fails unless it is compliant (scenario check, degradable check and
    // requirement, its detail), and a compliant one in a run that did not degrade (blocked, timed out) concludes
    // nothing.
    let c = manifest().seal();
    let of = |status: Status| Outcome {
        status,
        reason: None,
    };
    let with = |lines: Vec<Value>| {
        let mut e = e0(&c);
        e.observations = lines;
        e
    };
    let t = "TEST-001-01";
    let bare = line(t, "DEGRADED");
    type Case = (Vec<Value>, Status, bool, bool, bool, Option<&'static str>);
    // (lines, outcome, scenario, requirement degradable, check degradable, conclusion)
    let cases: Vec<Case> = vec![
        // (1) a vetoed member, whatever its lines.
        (
            vec![line(t, "PASS")],
            Status::Invalid,
            true,
            false,
            false,
            Some("INVALID"),
        ),
        // (2) a FAIL line, from any check.
        (
            vec![line(t, "PASS"), line(t, "FAIL")],
            Status::Pass,
            false,
            false,
            false,
            Some("FAIL"),
        ),
        // (3) a DEGRADED line that is not compliant.
        (
            vec![degraded(t)],
            Status::Degraded,
            false,
            true,
            true,
            Some("FAIL"),
        ),
        (
            vec![degraded(t)],
            Status::Degraded,
            true,
            false,
            true,
            Some("FAIL"),
        ),
        (
            vec![degraded(t)],
            Status::Degraded,
            true,
            true,
            false,
            Some("FAIL"),
        ),
        (
            vec![bare.clone()],
            Status::Degraded,
            true,
            true,
            true,
            Some("FAIL"),
        ),
        (vec![bare], Status::Blocked, true, true, true, Some("FAIL")),
        // (4) the scenario check passing with its lines all PASS.
        (
            vec![line(t, "PASS")],
            Status::Pass,
            true,
            false,
            false,
            Some("PASS"),
        ),
        // (5) a compliant degradation.
        (
            vec![line(t, "PASS"), degraded(t)],
            Status::Degraded,
            true,
            true,
            true,
            Some("DEGRADED"),
        ),
        // (6) nothing to conclude.
        (vec![line(t, "PASS")], Status::Pass, false, true, true, None),
        (vec![line(t, "PASS")], Status::Fail, true, true, true, None),
        (
            vec![line(t, "PASS")],
            Status::Blocked,
            true,
            true,
            true,
            None,
        ),
        (
            vec![line(t, "SKIPPED")],
            Status::Pass,
            true,
            true,
            true,
            None,
        ),
        (
            vec![line(t, "PASS"), line(t, "SKIPPED")],
            Status::Pass,
            true,
            true,
            true,
            None,
        ),
        (
            vec![line("TEST-002-01", "PASS")],
            Status::Pass,
            true,
            true,
            true,
            None,
        ),
        (
            vec![line(t, "PASS")],
            Status::Degraded,
            true,
            true,
            true,
            None,
        ),
        (vec![degraded(t)], Status::Blocked, true, true, true, None),
        (vec![degraded(t)], Status::Fail, true, true, true, None),
    ];
    for (lines, status, scenario, requirement, check, expected) in cases {
        let got = conclude(
            &with(lines.clone()),
            &of(status),
            t,
            scenario,
            requirement,
            check,
        );
        assert_eq!(
            got.map(Status::code),
            expected,
            "{lines:?} {status:?} scenario {scenario} requirement {requirement} check {check}"
        );
    }
    Ok(())
}

#[test]
fn an_evidences_rows_are_its_conclusions_on_the_specs_acceptance_criteria() -> R {
    // docs/adr/ADR-027.md decision 9: a row per (AC, evidence) whose conclusion is not "nothing"; a vetoed evidence
    // writes INVALID for its check's scenario ACs and the ACs its lines name, the rest of the veto being the
    // candidate's INVALID state.
    let c = manifest().seal();
    let ac = |id: &str, test: &str, check: &str, degradable: bool| AcSpec {
        ac_id: id.to_owned(),
        test_id: test.to_owned(),
        check_id: check.to_owned(),
        degradable,
    };
    let acs = vec![
        ac("AC-001-01", "TEST-001-01", "unit", false),
        ac("AC-001-02", "TEST-001-02", "unit", false),
        ac("AC-002-01", "TEST-002-01", "recovery", false),
        ac("AC-003-01", "TEST-003-01", "integration", false),
    ];
    let mut e = e0(&c);
    e.observations = vec![
        line("TEST-001-01", "PASS"),
        line("TEST-001-02", "SKIPPED"),
        line("TEST-002-01", "FAIL"),
        line("TEST-002-01", "PASS"),
    ];
    let pass = Outcome {
        status: Status::Pass,
        reason: None,
    };
    let codes = |rows: Vec<(String, Status)>| -> Vec<(String, &'static str)> {
        rows.into_iter().map(|(a, s)| (a, s.code())).collect()
    };
    // A PASS unit run: its scenario AC with all PASS lines is PASS; a skipped one has no row; another check's AC
    // fails on its FAIL line; an AC it says nothing of has no row.
    assert_eq!(
        codes(rows(&e, &pass, &acs, false)),
        vec![
            ("AC-001-01".to_owned(), "PASS"),
            ("AC-002-01".to_owned(), "FAIL")
        ]
    );
    // Vetoed: INVALID for unit's scenario ACs and the ACs its lines name, nothing for the others.
    let invalid = Outcome {
        status: Status::Invalid,
        reason: None,
    };
    assert_eq!(
        codes(rows(&e, &invalid, &acs, false)),
        vec![
            ("AC-001-01".to_owned(), "INVALID"),
            ("AC-001-02".to_owned(), "INVALID"),
            ("AC-002-01".to_owned(), "INVALID")
        ]
    );
    // A blocked run writes only its failures.
    let blocked = Outcome {
        status: Status::Blocked,
        reason: None,
    };
    assert_eq!(
        codes(rows(&e, &blocked, &acs, false)),
        vec![("AC-002-01".to_owned(), "FAIL")]
    );
    Ok(())
}
