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
/// An error, not a refusal, when that jsonschema cannot check `date-time` (installed without the format extras that
/// requirements-dev.txt names): it would accept any string there and a negative case would pass for the wrong reason.
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
        if 'date-time' not in jsonschema.FormatChecker().checkers:\n    \
            sys.stderr.write('jsonschema cannot check date-time: install requirements-dev.txt')\n    \
            sys.exit(3)\n\
        schema = json.load(open(sys.argv[1] + '/contracts/' + sys.argv[2]))\n\
        jsonschema.validate(json.load(open(sys.argv[3])), schema, format_checker=jsonschema.FormatChecker())\n";
    let out = Command::new("python3")
        .args(["-I", "-c", script, root, schema])
        .arg(&path)
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok(match out.status.code() {
        Some(0) => Ok(()),
        Some(3) => return Err(stderr.into()),
        _ => Err(stderr),
    })
}

#[test]
fn the_plan_shape_check_agrees_with_the_locked_check_plan_schema() -> R {
    // docs/adr/ADR-027.md principle one rule 1 (M2T10R-01): plan_shape refuses exactly what Python's jsonschema refuses.
    let good = serde_json::json!({"schema_version": "1.0", "plan_id": "p", "description": "d", "checks": {
        "unit": {"mode": "product", "argv": ["bash", "u.sh"], "cwd": "sub/dir", "timeout_seconds": 1.5,
                 "artifacts": ["test-results.jsonl", "a_b-c.1"], "description": "x"},
        "qa-2": {"mode": "live", "argv": ["x"]}}});
    let mut cases = vec![("valid", good.clone())];
    let entry = |key: &str, value: serde_json::Value| {
        let mut v = good.clone();
        v["checks"]["unit"][key] = value;
        v
    };
    cases.extend([
        ("absolute cwd", entry("cwd", serde_json::json!("/tmp"))),
        ("empty cwd", entry("cwd", serde_json::json!(""))),
        ("unknown mode", entry("mode", serde_json::json!("fake"))),
        ("empty argv", entry("argv", serde_json::json!([]))),
        (
            "empty argument",
            entry("argv", serde_json::json!(["bash", ""])),
        ),
        (
            "zero timeout",
            entry("timeout_seconds", serde_json::json!(0)),
        ),
        (
            "text timeout",
            entry("timeout_seconds", serde_json::json!("1")),
        ),
        (
            "path artifact",
            entry("artifacts", serde_json::json!(["../x"])),
        ),
        (
            "dot artifact",
            entry("artifacts", serde_json::json!([".x"])),
        ),
        ("twice", entry("artifacts", serde_json::json!(["x", "x"]))),
        (
            "long artifact",
            entry("artifacts", serde_json::json!(["a".repeat(129)])),
        ),
        ("unknown key", entry("retries", serde_json::json!(2))),
    ]);
    let mut top = |key: &str, value: serde_json::Value| {
        let mut v = good.clone();
        v[key] = value;
        cases.push((key.to_owned().leak(), v));
    };
    top("schema_version", serde_json::json!("2.0"));
    top("plan_id", serde_json::json!(""));
    top("checks", serde_json::json!({}));
    top("extra", serde_json::json!(1));
    let mut bad_name = good.clone();
    bad_name["checks"]["Unit"] = good["checks"]["unit"].clone();
    cases.push(("upper-case check name", bad_name));
    for (label, plan) in cases {
        let python = python_validates("check-plan.schema.json", &plan)?.is_ok();
        let ours = hoh_core::verification::plan_shape(&plan);
        assert_eq!(ours.is_empty(), python, "{label}: {ours:?}");
    }
    Ok(())
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
