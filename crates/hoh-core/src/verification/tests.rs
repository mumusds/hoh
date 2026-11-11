use super::*;
use serde_json::json;

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

/// What the checks are judged against: the files and attempts a test world knows.
struct World {
    attempts: Vec<String>,
    files: Vec<(String, String)>,
}

impl Lookup for World {
    fn attempt_exists(&self, attempt_id: &str) -> bool {
        self.attempts.iter().any(|a| a == attempt_id)
    }

    fn sha256_of(&self, path: &str) -> Option<String> {
        self.files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, s)| s.clone())
    }
}

fn e0(c: &Sealed) -> Evidence {
    let m = &c.manifest;
    Evidence {
        check_id: "unit".to_owned(),
        candidate_id: c.candidate_id.clone(),
        git_tree: m.git_tree.clone(),
        spec_sha256: m.spec.sha256.clone(),
        environment_sha256: m.environment.sha256.clone(),
        test_plan_sha256: m.test_plan.sha256.clone(),
        started_at: "2026-11-11T10:00:00Z".to_owned(),
        finished_at: "2026-11-11T10:00:05Z".to_owned(),
        source_unchanged: true,
        stdout: file(".hoh/evidence/C/unit/r1/stdout.log", 9),
        missing_artifacts: Vec::new(),
        producer_attempt_id: "T1/attempt-1".to_owned(),
        observations: vec![json!({"test_id": "TEST-052-04", "status": "PASS"})],
    }
}

#[test]
fn test_052_04_evidence_is_valid_only_when_every_binding_holds() -> R {
    // AC-052-04 (REQ-052 behaviour 7, docs/adr/ADR-027.md decision 5): e0 valid; each of the 11 tampered aspects not
    // valid, observations=[] for INSUFFICIENT, the others INVALID, each with its own reason code.
    let c = manifest().seal();
    let s = c.manifest.spec.sha256.clone();
    let base = e0(&c);
    let world = World {
        attempts: vec![base.producer_attempt_id.clone()],
        files: vec![(base.stdout.path.clone(), base.stdout.sha256.clone())],
    };
    assert_eq!(valid(&base, &c, &s, &world), Ok(()));
    type Tamper = fn(&mut Evidence);
    let variants: [(&str, Tamper, &str, &str); 11] = [
        (
            "candidate_id",
            |e| e.candidate_id = format!("CAND-{}", "0".repeat(64)),
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
            |e| e.spec_sha256 = "0".repeat(64),
            "INVALID",
            "SPEC_MISMATCH",
        ),
        (
            "environment_sha256",
            |e| e.environment_sha256 = "0".repeat(64),
            "INVALID",
            "ENVIRONMENT_MISMATCH",
        ),
        (
            "test_plan_sha256",
            |e| e.test_plan_sha256 = "0".repeat(64),
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
            |e| e.producer_attempt_id = "nobody".to_owned(),
            "INVALID",
            "PRODUCER_MISSING",
        ),
        (
            "finished_at < started_at",
            |e| e.finished_at = "2026-11-11T09:59:59Z".to_owned(),
            "INVALID",
            "INTERVAL_INVALID",
        ),
        (
            "source_unchanged",
            |e| e.source_unchanged = false,
            "INVALID",
            "SOURCE_CHANGED",
        ),
        (
            "stdout sha256",
            |e| e.stdout.sha256 = "0".repeat(64),
            "INVALID",
            "OUTPUT_CHANGED",
        ),
        (
            "missing_artifacts",
            |e| e.missing_artifacts = vec!["report.json".to_owned()],
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
            (invalid.class, invalid.reasons.as_slice()),
            (class, &[code][..]),
            "{aspect}"
        );
        table.push(json!({"aspect": aspect, "class": invalid.class, "reasons": invalid.reasons}));
    }
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join("test_052_04-table.json"),
            serde_json::to_vec_pretty(&json!({"e0": "valid", "variants": table}))?,
        )?;
    }
    Ok(())
}
