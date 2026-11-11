//! A product candidate export against the bootstrap tools (docs/adr/ADR-027.md decision 2): Python's jsonschema
//! validates it against the locked contracts/candidate.schema.json (whose `^(?!/)` pattern the in-crate validator
//! cannot read), and tools/candidate.py computes the same identity.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_core::verification::{FileRef, Manifest};
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
