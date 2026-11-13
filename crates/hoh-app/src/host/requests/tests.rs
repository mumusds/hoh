//! The helpers as an agent runs them, against a host stand-in that answers their requests.

use super::*;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// `make()`'s command spawned, again while the kernel says the helper just written is busy: another test's child,
/// forked while the helper was open for writing, holds it until that child execs (ETXTBSY; the product's agent runs a
/// helper long after it was written).
fn spawned(make: impl Fn() -> Command) -> R<std::process::Child> {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match make().spawn() {
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy && Instant::now() < end =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return Ok(other?),
        }
    }
}

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
    let helpers = write_helpers(dir.path(), "C/round/1/task/qa/attempt-1", 600)?;
    assert!(
        helpers
            .run_check
            .starts_with(dir.path().join(".hoh/attempts"))
    );
    let first = spawned(|| {
        let mut c = Command::new(&helpers.run_check);
        c.arg("unit").stdout(Stdio::piped());
        c
    })?;
    let request = serve(
        &helpers,
        1,
        &json!({"status": "ran", "outcome": "FAIL", "exit_code": 3}),
    )?;
    assert_eq!(request, json!({"kind": "check", "check_id": "unit"}));
    let out = first.wait_with_output()?;
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8(out.stdout)?.contains("\"outcome\":\"FAIL\""));
    let second = spawned(|| {
        let mut c = Command::new(&helpers.run_check);
        c.arg("lint");
        c
    })?;
    serve(
        &helpers,
        2,
        &json!({"status": "refused", "reasons": ["worktree dirty: x"]}),
    )?;
    assert_eq!(second.wait_with_output()?.status.code(), Some(2));
    let bad = spawned(|| {
        let mut c = Command::new(&helpers.run_check);
        c.arg("a b; rm -rf /").stdout(Stdio::piped());
        c
    })?
    .wait_with_output()?;
    assert_eq!(bad.status.code(), Some(2));
    assert!(next(&helpers, 3).is_none(), "nothing was asked");
    Ok(())
}

#[test]
fn ask_sends_its_message_file_and_prints_the_answer() -> R {
    let dir = tempfile::tempdir()?;
    let helpers = write_helpers(dir.path(), "A", 600)?;
    let message = dir.path().join("m.json");
    std::fs::write(&message, "{\"message_id\": \"m1\", \"kind\": \"steer\"}")?;
    let ask = spawned(|| {
        let mut c = Command::new(&helpers.ask);
        c.arg(&message).stdout(Stdio::piped());
        c
    })?;
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

#[test]
fn run_check_waits_for_the_plans_longest_check_and_its_grace() -> R {
    // docs/adr/ADR-028.md decision 7: the check's wait is the frozen plan's timeout plus the grace, the plan's longest
    // check (an hour for one naming none), never a fixed bound a long check outlives.
    let plan = serde_json::json!({"checks": {"unit": {"timeout_seconds": 60}, "soak": {"timeout_seconds": 7200.5},
        "plain": {}}});
    let grace = std::time::Duration::from_secs(15);
    assert_eq!(check_wait(&plan, grace), 7201 + 15 + 60);
    assert_eq!(
        check_wait(&serde_json::json!({"checks": {"plain": {}}}), grace),
        3600 + 15 + 60
    );
    let dir = tempfile::tempdir()?;
    let helpers = write_helpers(dir.path(), "A", 7276)?;
    let script = std::fs::read_to_string(&helpers.run_check)?;
    assert!(script.contains("-ge 7276"), "{script}");
    Ok(())
}

/// `child`'s exit code once it ends, or an error (the child killed) when it runs past `limit`.
fn ended_within(mut child: std::process::Child, limit: Duration) -> R<Option<i32>> {
    let end = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("still running after {limit:?}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_helper_whose_request_directory_is_gone_ends_with_2_and_makes_none() -> R {
    // M2T18R-54: the host removes the request directory of an attempt no longer live (verifying.rs), and a test's
    // project goes with its temporary directory. A helper running then (an orphaned agent's) ends with 2 within its next
    // poll, never waiting out its whole wait (an hour and more) nor making the directory again.
    let dir = tempfile::tempdir()?;
    let message = dir.path().join("m.json");
    std::fs::write(&message, "{\"message_id\": \"m1\", \"kind\": \"steer\"}")?;
    // Gone before it runs: refused at once.
    let helpers = write_helpers(dir.path(), "A", 600)?;
    std::fs::remove_dir_all(&helpers.requests)?;
    for (helper, arg) in [
        (&helpers.run_check, std::ffi::OsStr::new("unit")),
        (&helpers.ask, message.as_os_str()),
    ] {
        let child = spawned(|| {
            let mut c = Command::new(helper);
            c.arg(arg).stdout(Stdio::null()).stderr(Stdio::null());
            c
        })?;
        assert_eq!(
            ended_within(child, Duration::from_secs(5))?,
            Some(2),
            "{}",
            helper.display()
        );
        assert!(
            !helpers.requests.exists(),
            "{} made it again",
            helper.display()
        );
    }
    // Gone, with its whole attempt directory, while it waits for its answer: it ends within a few seconds.
    let helpers = write_helpers(dir.path(), "B", 600)?;
    let child = spawned(|| {
        let mut c = Command::new(&helpers.run_check);
        c.arg("unit").stdout(Stdio::null()).stderr(Stdio::null());
        c
    })?;
    let end = Instant::now() + Duration::from_secs(20);
    while next(&helpers, 1).is_none() {
        if Instant::now() > end {
            return Err("no request 1".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let attempt = helpers.requests.parent().ok_or("no attempt directory")?;
    std::fs::remove_dir_all(attempt)?;
    assert_eq!(ended_within(child, Duration::from_secs(5))?, Some(2));
    assert!(!attempt.exists(), "the attempt directory was made again");
    Ok(())
}
