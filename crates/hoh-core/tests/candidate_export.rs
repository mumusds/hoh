//! A product candidate export and execution record against the bootstrap tools (docs/adr/ADR-027.md decisions 2
//! and 8): Python's jsonschema validates them against the locked contracts/candidate.schema.json and
//! execution.schema.json (whose `^(?!/)` patterns the in-crate validator cannot read), and tools/candidate.py computes
//! the same identity.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_core::verification::{FileRef, Manifest, NamedRef, Record, Runner};
use std::process::Command;

type R = Result<(), Box<dyn std::error::Error>>;

fn file(path: &str, digit: char) -> FileRef {
    FileRef {
        path: path.to_owned(),
        sha256: std::iter::repeat_n(digit, 64).collect(),
    }
}

#[test]
fn a_product_export_is_a_valid_candidate_with_the_identity_the_bootstrap_tool_computes() -> R {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let sealed = Manifest {
        git_commit: "a".repeat(40),
        git_tree: "b".repeat(64),
        spec: file("spec/requirements.json", '1'),
        environment: file(".hoh/environment.json", '2'),
        test_plan: file("tests/check-plan.json", '3'),
        source_files: vec![
            file("src/z.rs", '4'),
            file("文档/说明 \"引号\".md", '5'),
            file("src/a.rs", '6'),
        ],
    }
    .seal();
    let export = sealed.to_value();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("candidate.json");
    std::fs::write(&path, export.to_string())?;
    let script = "import json,sys\n\
        sys.path.insert(0, sys.argv[1] + '/tools')\n\
        import jsonschema\n\
        from candidate import identity\n\
        schema = json.load(open(sys.argv[1] + '/contracts/candidate.schema.json'))\n\
        data = json.load(open(sys.argv[2]))\n\
        jsonschema.validate(data, schema)\n\
        print(identity(data))\n";
    let out = Command::new("python3")
        .args(["-I", "-c", script, root])
        .arg(&path)
        .output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout)?.trim(), sealed.candidate_id);
    Ok(())
}

/// Whether the locked schema `schema` (under contracts/) accepts `value`, by Python's jsonschema: stderr when not.
fn python_validates(
    schema: &str,
    value: &serde_json::Value,
) -> Result<Result<(), String>, Box<dyn std::error::Error>> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("value.json");
    std::fs::write(&path, value.to_string())?;
    let script = "import json,sys\n\
        import jsonschema\n\
        schema = json.load(open(sys.argv[1] + '/contracts/' + sys.argv[2]))\n\
        jsonschema.validate(json.load(open(sys.argv[3])), schema, format_checker=jsonschema.FormatChecker())\n";
    let out = Command::new("python3")
        .args(["-I", "-c", script, root, schema])
        .arg(&path)
        .output()?;
    Ok(if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    })
}

#[test]
fn a_product_execution_record_is_valid_under_the_locked_schema_and_its_shape_check_agrees() -> R {
    // docs/adr/ADR-027.md decision 8 (M2T10D-255): the product writes records the gate reads; Record::from_value
    // refuses what the locked schema refuses.
    let record = Record {
        schema_version: "1.1".to_owned(),
        record_type: "command_execution".to_owned(),
        record_id: "r1".to_owned(),
        check_id: "unit".to_owned(),
        candidate_id: format!("CAND-{}", "a".repeat(64)),
        spec_sha256: "1".repeat(64),
        mode: "product".to_owned(),
        command: vec!["bash".to_owned(), "scripts/product/unit.sh".to_owned()],
        cwd: ".".to_owned(),
        started_at: "2026-11-11T10:00:00.250Z".to_owned(),
        finished_at: "2026-11-11T10:00:05+00:00".to_owned(),
        duration_seconds: 4.75,
        exit_code: 0,
        timed_out: false,
        stdout: file(".hoh/evidence/C/unit/r1/stdout.log", '2'),
        stderr: file(".hoh/evidence/C/unit/r1/stderr.log", '3'),
        candidate_manifest: file(".hoh/objects/sha256/aa/candidate", '4'),
        source_unchanged: true,
        environment_sha256: "5".repeat(64),
        test_plan_sha256: "6".repeat(64),
        artifacts: vec![NamedRef {
            name: "test-results.jsonl".to_owned(),
            path: ".hoh/evidence/C/unit/r1/artifacts/test-results.jsonl".to_owned(),
            sha256: "7".repeat(64),
        }],
        missing_artifacts: Vec::new(),
        producer_attempt_id: "T1/attempt-1".to_owned(),
        runner: Runner {
            tool: "hoh".to_owned(),
            euid: 1000,
            boot_id: Some("b".to_owned()),
        },
    };
    let value = serde_json::to_value(&record)?;
    python_validates("execution.schema.json", &value)?
        .map_err(|e| format!("valid record refused: {e}"))?;
    assert_eq!(Record::from_value(&value)?, record);
    for (key, bad) in [
        ("schema_version", serde_json::json!("1.0")),
        ("mode", serde_json::json!("fake")),
        ("cwd", serde_json::json!("/abs")),
        ("command", serde_json::json!([])),
        ("duration_seconds", serde_json::json!(-1)),
        ("candidate_id", serde_json::json!("CAND-1")),
        ("started_at", serde_json::json!("yesterday")),
        ("extra", serde_json::json!(1)),
    ] {
        let mut v = value.clone();
        v[key] = bad;
        let python = python_validates("execution.schema.json", &v)?;
        assert!(python.is_err(), "the schema accepts {key}");
        assert!(
            Record::from_value(&v).is_err(),
            "the shape check accepts {key}"
        );
    }
    Ok(())
}
