use super::*;
use crate::context::sha256_hex;

fn binding() -> Binding {
    Binding {
        review_id: "rv-1".to_owned(),
        attempt_id: "a-1".to_owned(),
        spec_revision: 3,
        spec_sha256: "3".repeat(64),
        git_commit: "c".repeat(40),
    }
}

const RAW: &str = "raw stdout log";

/// A consistent native attempt record and report, as bytes, for `binding`.
fn objects(binding: &Binding) -> (Vec<u8>, Vec<u8>) {
    let raw_sha256 = sha256_hex(RAW.as_bytes());
    let native = native(
        binding,
        &["S-rev".to_owned(), "native-9".to_owned()],
        &raw_sha256,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let native_bytes = serde_json::to_vec(&native).unwrap_or_default();
    let body = json!({"role": "reviewer", "status": "PASS", "scope": ["docs/spec"], "findings": [
        {"id": "F-1", "severity": "low", "summary": "wording"}]});
    let reference = |sha256: String| ObjectRef {
        path: crate::objects::relative_path(&sha256),
        sha256,
    };
    let report = build(
        &body,
        "S-rev",
        binding,
        &reference(raw_sha256),
        &reference(sha256_hex(&native_bytes)),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    (
        serde_json::to_vec(&report).unwrap_or_default(),
        native_bytes,
    )
}

fn codes(checked: &Checked) -> Vec<&str> {
    checked
        .reasons
        .iter()
        .map(|r| r.split_once(':').map_or(r.as_str(), |(code, _)| code))
        .collect()
}

fn raw_sha256() -> String {
    sha256_hex(RAW.as_bytes())
}

#[test]
fn spec_report_native_and_report_are_built_valid_and_bound() {
    let (report, native) = objects(&binding());
    let checked = check(Some(&report), Some(&native), &raw_sha256(), &binding());
    assert_eq!(checked.reasons, Vec::<String>::new());
    let (report, native) = (
        checked.report.unwrap_or_default(),
        checked.native.unwrap_or_default(),
    );
    assert_eq!(native["record_type"], "runtime_spec_review_export");
    assert_eq!(native["status"], "COMPLETED");
    assert_eq!(native["review_id"], "rv-1");
    assert_eq!(native["attempt_id"], "a-1");
    assert_eq!(native["stdout_sha256"], json!(raw_sha256()));
    assert_eq!(report["schema_version"], "1.0");
    assert_eq!(report["role"], "reviewer");
    assert_eq!(report["spec_revision"], 3);
    assert_eq!(report["git_commit"], json!("c".repeat(40)));
    assert!(report.get("candidate_id").is_none());
}

#[test]
fn spec_report_build_refuses_what_its_schema_refuses() {
    let reference = ObjectRef {
        path: "x".to_owned(),
        sha256: "a".repeat(64),
    };
    let bad = json!({"status": "MAYBE", "scope": ["s"], "findings": []});
    assert!(build(&bad, "S", &binding(), &reference, &reference).is_err());
    let mut unbound = binding();
    unbound.git_commit = "not a commit".to_owned();
    assert!(native(&unbound, &["S".to_owned()], &"a".repeat(64)).is_err());
    assert!(native(&binding(), &[], &"a".repeat(64)).is_err());
}

#[test]
fn spec_report_each_tampered_binding_is_named() {
    // AC-031-04 revision 3: the record's review_id, attempt_id, stdout_sha256, spec_sha256 and git_commit each
    // replaced once, and no record at all.
    let mut cases: Vec<(&str, Binding, &str)> = Vec::new();
    let tamper = |edit: &dyn Fn(&mut Binding)| {
        let mut b = binding();
        edit(&mut b);
        b
    };
    cases.push((
        "review_id",
        tamper(&|b| b.review_id = "rv-other".to_owned()),
        codes::REVIEW_ID_MISMATCH,
    ));
    cases.push((
        "attempt_id",
        tamper(&|b| b.attempt_id = "a-other".to_owned()),
        codes::ATTEMPT_ID_MISMATCH,
    ));
    cases.push((
        "spec_sha256",
        tamper(&|b| b.spec_sha256 = "4".repeat(64)),
        "spec_sha256_mismatch",
    ));
    cases.push((
        "git_commit",
        tamper(&|b| b.git_commit = "d".repeat(40)),
        "git_commit_mismatch",
    ));
    for (name, recorded, code) in cases {
        let (report, _) = objects(&binding());
        let native_bytes = serde_json::to_vec(
            &native(&recorded, &["S-rev".to_owned()], &raw_sha256()).unwrap_or_default(),
        )
        .unwrap_or_default();
        // The report names the replaced record, so only the binding differs.
        let mut report: Value = serde_json::from_slice(&report).unwrap_or_default();
        report["native_attempt"]["sha256"] = json!(sha256_hex(&native_bytes));
        let report = serde_json::to_vec(&report).unwrap_or_default();
        let checked = check(
            Some(&report),
            Some(&native_bytes),
            &raw_sha256(),
            &binding(),
        );
        assert_eq!(codes(&checked), [code], "{name}: {:?}", checked.reasons);
    }
    let (report, _) = objects(&binding());
    let stdout = serde_json::to_vec(
        &native(&binding(), &["S-rev".to_owned()], &"e".repeat(64)).unwrap_or_default(),
    )
    .unwrap_or_default();
    let mut named: Value = serde_json::from_slice(&report).unwrap_or_default();
    named["native_attempt"]["sha256"] = json!(sha256_hex(&stdout));
    let named = serde_json::to_vec(&named).unwrap_or_default();
    assert_eq!(
        codes(&check(
            Some(&named),
            Some(&stdout),
            &raw_sha256(),
            &binding()
        )),
        [codes::STDOUT_SHA256_MISMATCH]
    );
    assert_eq!(
        codes(&check(Some(&report), None, &raw_sha256(), &binding())),
        [codes::NATIVE_ATTEMPT_MISSING]
    );
}

#[test]
fn spec_report_check_collects_every_reason() {
    // No check stops the others: a record replaced wholesale is named for each binding and its digest.
    let (report, _) = objects(&binding());
    let mut other = binding();
    other.review_id = "rv-2".to_owned();
    other.attempt_id = "a-2".to_owned();
    other.spec_revision = 4;
    let native_bytes = serde_json::to_vec(
        &native(&other, &["S-else".to_owned()], &"e".repeat(64)).unwrap_or_default(),
    )
    .unwrap_or_default();
    let checked = check(
        Some(&report),
        Some(&native_bytes),
        &"f".repeat(64),
        &binding(),
    );
    assert_eq!(
        codes(&checked),
        [
            codes::RAW_SESSION_SHA256_MISMATCH,
            codes::NATIVE_ATTEMPT_SHA256_MISMATCH,
            codes::SESSION_NOT_IN_NATIVE_ATTEMPT,
            codes::REVIEW_ID_MISMATCH,
            codes::ATTEMPT_ID_MISMATCH,
            codes::STDOUT_SHA256_MISMATCH,
            "spec_revision_mismatch",
        ]
    );
}

#[test]
fn spec_report_invalid_objects_and_duplicate_findings_are_named() {
    let (report, native_bytes) = objects(&binding());
    assert_eq!(
        codes(&check(None, Some(&native_bytes), &raw_sha256(), &binding())),
        [codes::REPORT_MISSING]
    );
    assert_eq!(
        codes(&check(
            Some(b"not json"),
            Some(&native_bytes),
            &raw_sha256(),
            &binding()
        )),
        [codes::REPORT_INVALID]
    );
    let mut failed: Value = serde_json::from_slice(&native_bytes).unwrap_or_default();
    failed["status"] = json!("FAILED");
    let failed = serde_json::to_vec(&failed).unwrap_or_default();
    let mut named: Value = serde_json::from_slice(&report).unwrap_or_default();
    named["native_attempt"]["sha256"] = json!(sha256_hex(&failed));
    let named = serde_json::to_vec(&named).unwrap_or_default();
    assert_eq!(
        codes(&check(
            Some(&named),
            Some(&failed),
            &raw_sha256(),
            &binding()
        )),
        [codes::NATIVE_ATTEMPT_INVALID]
    );
    let mut twice: Value = serde_json::from_slice(&report).unwrap_or_default();
    twice["findings"] = json!([{"id": "F-1", "severity": "low", "summary": "a"},
        {"id": "F-1", "severity": "medium", "summary": "b"}]);
    let twice = serde_json::to_vec(&twice).unwrap_or_default();
    assert_eq!(
        codes(&check(
            Some(&twice),
            Some(&native_bytes),
            &raw_sha256(),
            &binding()
        )),
        [codes::DUPLICATE_FINDING_ID]
    );
    // A report bound to another revision than its review's is named for each key.
    let mut stale: Value = serde_json::from_slice(&report).unwrap_or_default();
    stale["spec_revision"] = json!(2);
    stale["git_commit"] = json!("b".repeat(40));
    let stale = serde_json::to_vec(&stale).unwrap_or_default();
    assert_eq!(
        codes(&check(
            Some(&stale),
            Some(&native_bytes),
            &raw_sha256(),
            &binding()
        )),
        ["spec_revision_mismatch", "git_commit_mismatch"]
    );
}
