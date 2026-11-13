use super::*;

const CANDIDATE: &str = "CAND-1111111111111111111111111111111111111111111111111111111111111111";

fn refs() -> (ObjectRef, ObjectRef) {
    (
        ObjectRef {
            path: ".hoh/objects/sha256/aa/raw".to_owned(),
            sha256: "a".repeat(64),
        },
        ObjectRef {
            path: ".hoh/objects/sha256/bb/attempt".to_owned(),
            sha256: "b".repeat(64),
        },
    )
}

#[test]
fn the_last_report_block_of_the_final_message_is_read() {
    // tools/agent_task.py's rule: the last ```hoh-report block; a JSON object of role, status, scope, findings.
    let text = "first\n```hoh-report\n{\"status\": \"FAIL\"}\n```\nthen\n```hoh-report\n{\"role\": \"qa\", \
                \"status\": \"PASS\", \"scope\": [\"AC-1\"], \"findings\": []}\n```\n";
    assert_eq!(
        block(text, "qa"),
        Ok(json!({"role": "qa", "status": "PASS", "scope": ["AC-1"], "findings": []}))
    );
    for (bad, why) in [
        ("no block at all", "no ```hoh-report block"),
        ("```hoh-report\nnot json\n```", "not JSON"),
        ("```hoh-report\n[1]\n```", "a JSON object"),
        (
            "```hoh-report\n{\"status\": \"PASS\", \"extra\": 1}\n```",
            "only",
        ),
        (
            "```hoh-report\n{\"role\": \"reviewer\", \"status\": \"PASS\"}\n```",
            "role",
        ),
    ] {
        let got = block(bad, "qa");
        assert!(
            got.as_ref().is_err_and(|e| e.contains(why)),
            "{bad}: {got:?}"
        );
    }
}

#[test]
fn a_report_is_completed_by_the_runtime_and_checked_against_its_schema() {
    // Decision 15: the HoH session, the candidate, the spec and the two objects are filled in; the result is valid
    // under contracts/independent-report.schema.json, or refused (here: no scope).
    let (raw, attempt) = refs();
    let body = json!({"status": "FAIL", "scope": ["AC-1"], "findings": [{"id": "F1", "severity": "high",
        "summary": "the parser drops a line"}]});
    let report = build(
        &body,
        "reviewer",
        "R/session/reviewer/1",
        (CANDIDATE, &"c".repeat(64)),
        &raw,
        &attempt,
    );
    let Ok(report) = report else {
        panic!("{report:?}");
    };
    assert_eq!(report["schema_version"], "1.1");
    assert_eq!(report["role"], "reviewer");
    assert_eq!(report["session_id"], "R/session/reviewer/1");
    assert_eq!(report["candidate_id"], CANDIDATE);
    assert_eq!(report["status"], "FAIL");
    assert_eq!(
        report["raw_session"],
        json!({"path": raw.path, "sha256": raw.sha256})
    );
    assert_eq!(report["native_attempt"]["sha256"], json!(attempt.sha256));
    assert!(
        crate::schema::independent_report()
            .validate(&report)
            .is_ok()
    );
    let unscoped = build(
        &json!({"status": "PASS"}),
        "qa",
        "s",
        (CANDIDATE, &"c".repeat(64)),
        &raw,
        &attempt,
    );
    assert!(unscoped.is_err(), "{unscoped:?}");
}

#[test]
fn a_native_attempt_export_is_valid_under_its_schema() {
    let export = native_attempt(
        "qa",
        "COMPLETED",
        &["s".to_owned(), "native".to_owned()],
        CANDIDATE,
        &"d".repeat(64),
    );
    let Ok(export) = export else {
        panic!("{export:?}");
    };
    assert_eq!(export["record_type"], "runtime_attempt_export");
    assert_eq!(export["stdout_sha256"], json!("d".repeat(64)));
    assert!(crate::schema::native_attempt().validate(&export).is_ok());
}

/// A report by the reviewer in session `s` with findings `findings`, its raw session and native attempt objects as
/// bytes: (report, raw digest, native) as the host keeps them, consistent with each other.
fn kept(findings: Value) -> (Vec<u8>, String, Vec<u8>) {
    let raw = b"{\"type\": \"result\"}\n";
    let raw_sha256 = crate::context::sha256_hex(raw);
    let native = native_attempt(
        "reviewer",
        "COMPLETED",
        &["s".to_owned(), "native-1".to_owned()],
        CANDIDATE,
        &raw_sha256,
    )
    .unwrap_or_default();
    let native = serde_json::to_vec(&native).unwrap_or_default();
    let object = |sha256: &str| ObjectRef {
        path: crate::objects::relative_path(sha256),
        sha256: sha256.to_owned(),
    };
    let report = build(
        &json!({"status": "FAIL", "scope": ["AC-1"], "findings": findings}),
        "reviewer",
        "s",
        (CANDIDATE, &"c".repeat(64)),
        &object(&raw_sha256),
        &object(&crate::context::sha256_hex(&native)),
    )
    .unwrap_or_default();
    (
        serde_json::to_vec(&report).unwrap_or_default(),
        raw_sha256,
        native,
    )
}

/// `value` as bytes, changed by `change`.
fn changed(bytes: &[u8], change: impl Fn(&mut Value)) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(bytes).unwrap_or_default();
    change(&mut value);
    serde_json::to_vec(&value).unwrap_or_default()
}

#[test]
fn a_consistent_report_is_verified_with_its_native_sessions_and_status() {
    // docs/adr/ADR-029.md decision 7 (host side): the store is handed the report and its native attempt's session ids
    // and status, read from the objects the host checked.
    let finding = json!([{"id": "F1", "severity": "low", "summary": "a typo"}]);
    let (report, raw, native) = kept(finding);
    let checked = verify(&report, &raw, &native);
    let Ok(checked) = checked else {
        panic!("{checked:?}");
    };
    assert_eq!(checked.session_ids, ["s", "native-1"]);
    assert_eq!(checked.status, "COMPLETED");
    assert_eq!(checked.report["session_id"], "s");
    assert_eq!(checked.report["findings"][0]["id"], "F1");
}

/// A defect: what it is, the report, the raw digest and the native attempt given, and what the refusal names.
type Case = (&'static str, Vec<u8>, String, Vec<u8>, &'static str);

#[test]
fn each_defect_of_a_report_or_its_objects_is_refused_by_the_hosts_check() {
    // docs/adr/ADR-029.md decision 7 and its test plan: each defect (schema, raw and native digests, a native attempt
    // without the report's session, another candidate or role, a stdout that is not the raw session, a repeated
    // finding id) is refused, an output to correct (`verification.output_invalid`).
    let (report, raw, native) = kept(json!([]));
    let native_with = |change: &dyn Fn(&mut Value)| {
        let native = changed(&native, change);
        let report = changed(&report, |r| {
            r["native_attempt"]["sha256"] = json!(crate::context::sha256_hex(&native));
        });
        (report, native)
    };
    let mut cases: Vec<Case> = vec![
        (
            "not valid under the schema",
            changed(&report, |r| r["status"] = json!("MAYBE")),
            raw.clone(),
            native.clone(),
            "invalid",
        ),
        (
            "not JSON",
            b"{".to_vec(),
            raw.clone(),
            native.clone(),
            "JSON",
        ),
        (
            "a raw session of other bytes",
            report.clone(),
            "f".repeat(64),
            native.clone(),
            "raw_session",
        ),
        (
            "a native attempt of other bytes",
            report.clone(),
            raw.clone(),
            changed(&native, |n| n["status"] = json!("COMPLETED ")),
            "native_attempt",
        ),
    ];
    for (what, change, why) in [
        (
            "a native attempt invalid under its schema",
            Box::new(|n: &mut Value| n["session_ids"] = json!([])) as Box<dyn Fn(&mut Value)>,
            "native attempt export is invalid",
        ),
        (
            "a native attempt without the report's session",
            Box::new(|n: &mut Value| n["session_ids"] = json!(["other"])),
            "session",
        ),
        (
            "a native attempt of another candidate",
            Box::new(|n: &mut Value| n["candidate_id"] = json!(format!("CAND-{}", "e".repeat(64)))),
            "candidate",
        ),
        (
            "a native attempt of another role",
            Box::new(|n: &mut Value| n["role"] = json!("qa")),
            "role",
        ),
        (
            "a native stdout that is not the raw session",
            Box::new(|n: &mut Value| n["stdout_sha256"] = json!("e".repeat(64))),
            "stdout",
        ),
    ] {
        let (report, native) = native_with(&*change);
        cases.push((what, report, raw.clone(), native, why));
    }
    let twice = json!([{"id": "F1", "severity": "low", "summary": "a"}, {"id": "F1", "severity": "high",
        "summary": "b"}]);
    let (report, raw, native) = kept(twice);
    cases.push(("a finding id twice", report, raw, native, "finding id F1"));
    for (what, report, raw, native, why) in cases {
        let got = verify(&report, &raw, &native);
        assert!(
            got.as_ref().is_err_and(|e| e.contains(why)),
            "{what}: {got:?}"
        );
    }
}

/// A project at `dir` keeping the reviewer's report with findings F1 and F2 by session `s`, its raw session and native
/// attempt as objects, and F1's Issue traced as the store registers it.
fn traced(dir: &std::path::Path) -> IssueChain {
    let (report, raw_sha256, native) = kept(json!([
        {"id": "F1", "severity": "critical", "summary": "out of bounds",
         "evidence": [{"path": "src/a.rs", "sha256": "e".repeat(64)}]},
        {"id": "F2", "severity": "low", "summary": "a name"}
    ]));
    let put = |bytes: &[u8]| crate::objects::put(dir, bytes).unwrap_or_default();
    let raw = put(b"{\"type\": \"result\"}\n");
    assert_eq!(raw, raw_sha256);
    let (native, report) = (put(&native), put(&report));
    let at = |sha256: String| ObjectRef {
        path: crate::objects::relative_path(&sha256),
        sha256,
    };
    let report = at(report);
    IssueChain {
        issue: ChainIssue {
            issue_id: "I1".to_owned(),
            candidate_id: Some(CANDIDATE.to_owned()),
            severity: "critical".to_owned(),
            fingerprint: "f".repeat(64),
            finding_id: "F1".to_owned(),
        },
        report: ChainReport {
            report_id: "A/report".to_owned(),
            state: "CURRENT".to_owned(),
            path: report.path,
            sha256: report.sha256,
        },
        raw: at(raw),
        native: Some(at(native)),
    }
}

#[test]
fn test_054_04_an_issue_traces_to_its_report_object_and_raw_session() {
    // AC-054-04 (docs/adr/ADR-029.md decision 15): the report object read by its name, its finding, its raw_session and
    // native_attempt the registered objects with their bytes recomputed, the native attempt holding the report's
    // session and naming the raw session as its stdout: every link holds.
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let chain = traced(dir.path());
    let check = verify_chain(dir.path(), &chain);
    assert_eq!(
        check,
        ChainCheck {
            report_digest: true,
            finding_present: true,
            raw_digest: true,
            native_digest: true,
            session_in_native: true,
            stdout_is_raw: true,
        }
    );
    assert!(check.holds());
}

#[test]
fn each_broken_link_of_an_issues_chain_is_found() {
    // Decision 15's negative controls: a finding the report does not name, a raw or native artifact other than the
    // report's references, a damaged object, a missing native attempt, and nothing at all when the report object is
    // gone.
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let chain = traced(dir.path());
    let mut other_finding = chain.clone();
    other_finding.issue.finding_id = "F9".to_owned();
    let check = verify_chain(dir.path(), &other_finding);
    assert!(
        !check.finding_present && check.report_digest && check.raw_digest,
        "{check:?}"
    );
    let mut other_raw = chain.clone();
    other_raw.raw.sha256 = "a".repeat(64);
    let check = verify_chain(dir.path(), &other_raw);
    assert!(
        !check.raw_digest && !check.stdout_is_raw && check.native_digest,
        "{check:?}"
    );
    let mut no_native = chain.clone();
    no_native.native = None;
    let check = verify_chain(dir.path(), &no_native);
    assert!(
        !check.native_digest && !check.session_in_native && check.raw_digest,
        "{check:?}"
    );
    // The native attempt damaged in place: its bytes no longer hash to its name.
    let native = chain.native.clone().unwrap_or_else(|| panic!("no native"));
    std::fs::write(dir.path().join(&native.path), b"{}").unwrap_or_else(|e| panic!("{e}"));
    let check = verify_chain(dir.path(), &chain);
    assert!(
        !check.native_digest && !check.session_in_native && check.report_digest,
        "{check:?}"
    );
    // The report object gone: no link can be checked.
    std::fs::remove_file(dir.path().join(&chain.report.path)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(verify_chain(dir.path(), &chain), ChainCheck::default());
}
