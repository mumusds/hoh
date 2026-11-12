//! The helpers as an agent runs them, against a host stand-in that answers their requests.

use super::*;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Wait for request `n` and answer it with `answer`; the request.
fn serve(helpers: &Helpers, n: u64, answer: &Value) -> R<Value> {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(request) = next(helpers, n) {
            respond(helpers, n, answer)?;
            return Ok(request?);
        }
        if Instant::now() > end {
            return Err(format!("no request {n}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn run_check_asks_for_its_check_and_ends_with_its_exit_code() -> R {
    // Decision 7: `run-check <id>` writes {kind: check, check_id} as the next numbered request, prints the answer and
    // ends with the check's exit code; a refusal (no exit code) ends 2; an id that is no plan id is refused at once.
    let dir = tempfile::tempdir()?;
    let helpers = write_helpers(dir.path(), "C/round/1/task/qa/attempt-1")?;
    assert!(
        helpers
            .run_check
            .starts_with(dir.path().join(".hoh/attempts"))
    );
    let first = Command::new(&helpers.run_check)
        .arg("unit")
        .stdout(Stdio::piped())
        .spawn()?;
    let request = serve(
        &helpers,
        1,
        &json!({"status": "ran", "outcome": "FAIL", "exit_code": 3}),
    )?;
    assert_eq!(request, json!({"kind": "check", "check_id": "unit"}));
    let out = first.wait_with_output()?;
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8(out.stdout)?.contains("\"outcome\":\"FAIL\""));
    let second = Command::new(&helpers.run_check).arg("lint").spawn()?;
    serve(
        &helpers,
        2,
        &json!({"status": "refused", "reasons": ["worktree dirty: x"]}),
    )?;
    assert_eq!(second.wait_with_output()?.status.code(), Some(2));
    let bad = Command::new(&helpers.run_check)
        .arg("a b; rm -rf /")
        .output()?;
    assert_eq!(bad.status.code(), Some(2));
    assert!(next(&helpers, 3).is_none(), "nothing was asked");
    Ok(())
}

#[test]
fn ask_sends_its_message_file_and_prints_the_answer() -> R {
    let dir = tempfile::tempdir()?;
    let helpers = write_helpers(dir.path(), "A")?;
    let message = dir.path().join("m.json");
    std::fs::write(&message, "{\"message_id\": \"m1\", \"kind\": \"steer\"}")?;
    let ask = Command::new(&helpers.ask)
        .arg(&message)
        .stdout(Stdio::piped())
        .spawn()?;
    let request = serve(&helpers, 1, &json!({"status": "deferred"}))?;
    assert_eq!(
        request,
        json!({"kind": "message", "message": {"message_id": "m1", "kind": "steer"}})
    );
    let out = ask.wait_with_output()?;
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8(out.stdout)?.contains("deferred"));
    Ok(())
}

#[test]
fn a_change_of_the_candidate_is_told_from_other_messages() -> R {
    // Decision 13: a steer to the developer, or a query whose intent is change_candidate, is a change request; an
    // information query or a steer to another role is deferred.
    let message = |kind: &str, to: &str, payload: Value| -> R<Message> {
        Ok(serde_json::from_value(json!({
            "schema_version": "1.0", "message_id": "m", "run_id": "R", "task_id": "T", "attempt_id": "A",
            "sender_role": "qa", "recipient_role": to, "role_epoch": 1, "correlation_id": "c", "causation_id": null,
            "idempotency_key": "m", "expected_revision": 1, "spec_sha256": "1".repeat(64), "candidate_id": null,
            "kind": kind, "payload": payload
        }))?)
    };
    let change = |m: &Message| {
        matches!(
            classify(m, "C/round/1", "C"),
            VerifierRequest::RejectChange(_)
        )
    };
    assert!(change(&message(
        "steer",
        "developer",
        json!({"text": "fix it"})
    )?));
    assert!(change(&message(
        "query",
        "developer",
        json!({"question": "change?", "intent": "change_candidate"})
    )?));
    assert!(!change(&message(
        "query",
        "developer",
        json!({"question": "what does X mean?"})
    )?));
    assert!(!change(&message(
        "steer",
        "reviewer",
        json!({"text": "look at Y"})
    )?));
    let VerifierRequest::RejectChange(r) = classify(
        &message(
            "steer",
            "developer",
            json!({"text": "fix it", "severity": "critical"}),
        )?,
        "C/round/1",
        "C",
    ) else {
        return Err("not a change".into());
    };
    assert_eq!(
        (
            r.severity.as_str(),
            r.text.as_str(),
            r.candidate_id.as_str()
        ),
        ("critical", "fix it", "C")
    );
    Ok(())
}
