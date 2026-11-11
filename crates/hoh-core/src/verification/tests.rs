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
    plan: Vec<(String, Planned)>,
    sessions: Vec<(String, Value)>,
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
        observations: vec![json!({"test_id": "TEST-052-04", "status": "PASS"})],
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
            |e| e.record.missing_artifacts = vec!["report.json".to_owned()],
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
    let variants: [(&str, Tamper, Reason); 20] = [
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
            "timed out",
            |e, _| e.record.timed_out = true,
            Reason::TimedOut,
        ),
        (
            "exit 0 with a failing test",
            |e, _| {
                e.observations
                    .push(json!({"test_id": "TEST-052-04", "status": "FAIL"}))
            },
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
            "duration",
            |e, _| e.record.duration_seconds = 100.0,
            Reason::DurationMismatch,
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
            "live without its sessions",
            |e, w| {
                e.record.mode = "live".to_owned();
                w.plan[0].1.mode = "live".to_owned();
            },
            Reason::LiveSessionsUnbound,
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
    let bound = json!({"check_id": "unit", "harness_candidate_id": c.candidate_id});
    w.sessions.push((path.clone(), bound.clone()));
    assert_eq!(valid(&e, &c, &s, &w), Ok(()));
    for (key, other) in [
        ("check_id", "adapter-live"),
        ("harness_candidate_id", "CAND-x"),
    ] {
        let mut v = bound.clone();
        v[key] = json!(other);
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
    e.record.timed_out = true;
    e.record.candidate_id = format!("CAND-{}", "0".repeat(64));
    w.producers[0].1.role = "developer".to_owned();
    let got = valid(&e, &c, &s, &w).map_err(|i| i.codes());
    assert_eq!(
        got,
        Err(vec![
            "CANDIDATE_MISMATCH",
            "PRODUCER_NOT_INDEPENDENT",
            "ERROR_OUTPUT_CHANGED",
            "TIMED_OUT"
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
