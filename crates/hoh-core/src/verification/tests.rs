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
