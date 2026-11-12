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
        "SETTLED",
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
