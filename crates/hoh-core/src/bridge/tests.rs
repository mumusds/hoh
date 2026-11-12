use super::*;
use serde_json::json;

fn identity() -> Identity {
    Identity {
        run_id: "R".to_owned(),
        task_id: "T1".to_owned(),
        attempt_id: "T1/attempt-1".to_owned(),
        role: "developer".to_owned(),
        role_epoch: 2,
    }
}

fn result(status: &str, summary: &str) -> Value {
    json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "T1", "attempt_id": "T1/attempt-1", "role": "developer",
        "role_epoch": 2, "status": status, "summary": summary, "candidate_id": null, "evidence": [], "issues": [],
        "next_action": "wait for verification",
    })
}

fn message(kind: &str) -> Value {
    json!({
        "schema_version": "1.0", "message_id": "m1", "run_id": "R", "task_id": "T1", "attempt_id": "T1/attempt-1",
        "sender_role": "developer", "recipient_role": "commander", "role_epoch": 2, "correlation_id": "c1",
        "causation_id": null, "idempotency_key": "k1", "expected_revision": 0, "spec_sha256": "a".repeat(64),
        "candidate_id": null, "kind": kind, "payload": {"question": "which spec?"},
    })
}

fn lines(values: &[Value]) -> Vec<String> {
    values.iter().map(Value::to_string).collect()
}

#[test]
fn the_last_valid_task_result_and_the_queries_and_steers_are_kept() {
    // docs/adr/ADR-028.md decision 2: the last TaskResult wins; query and steer messages are kept in order.
    let parsed = parse(
        &lines(&[
            result("BLOCKED", "first"),
            message("query"),
            result("COMPLETED", "second"),
            message("steer"),
        ]),
        &identity(),
    );
    assert_eq!(
        parsed.task_result.as_ref().map(|r| r.summary.as_str()),
        Some("second")
    );
    let kinds: Vec<MessageKind> = parsed.messages.iter().map(|m| m.kind).collect();
    assert_eq!(kinds, [MessageKind::Query, MessageKind::Steer]);
    assert!(parsed.refused.is_empty(), "{:?}", parsed.refused);
}

#[test]
fn a_line_off_its_schema_or_another_identity_is_refused_alone() {
    // Decision 2: a refused line does not void the others; each is named by position, digest and reason.
    let mut stale = result("COMPLETED", "an older epoch");
    stale["role_epoch"] = json!(1);
    let mut other = result("COMPLETED", "another task");
    other["task_id"] = json!("T2");
    let mut foreign = message("query");
    foreign["sender_role"] = json!("qa");
    let mut handoff = message("query");
    handoff["kind"] = json!("handoff");
    let text = vec![
        "not json".to_owned(),
        json!({"status": "COMPLETED"}).to_string(),
        stale.to_string(),
        other.to_string(),
        foreign.to_string(),
        handoff.to_string(),
        json!([1, 2]).to_string(),
        result("COMPLETED", "kept").to_string(),
    ];
    let parsed = parse(&text, &identity());
    assert_eq!(
        parsed.task_result.as_ref().map(|r| r.summary.as_str()),
        Some("kept")
    );
    assert!(parsed.messages.is_empty());
    let refused: Vec<usize> = parsed.refused.iter().map(|r| r.line).collect();
    assert_eq!(refused, [0, 1, 2, 3, 4, 5, 6]);
    for r in &parsed.refused {
        assert_eq!(
            r.sha256,
            crate::context::sha256_hex(text[r.line].as_bytes())
        );
        assert!(!r.reason.is_empty());
    }
}

#[test]
fn the_last_task_result_shaped_line_decides_even_when_it_is_invalid() {
    // docs/adr/ADR-028.md decision 2 (M2T16D-359): an agent that wrote COMPLETED and then a FAILED off its identity meant
    // the FAILED; the result is invalid (a protocol correction), never the earlier COMPLETED.
    let mut late = result("FAILED", "changed my mind");
    late["role_epoch"] = json!(9);
    let parsed = parse(
        &lines(&[result("COMPLETED", "early"), message("query"), late]),
        &identity(),
    );
    assert_eq!(parsed.task_result, None);
    assert!(parsed.result_invalid);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(
        parsed.refused.iter().map(|r| r.line).collect::<Vec<_>>(),
        [2]
    );
    // Without any TaskResult-shaped line there is no result and nothing invalid about it.
    let none = parse(&lines(&[message("steer")]), &identity());
    assert_eq!((none.task_result, none.result_invalid), (None, false));
}
