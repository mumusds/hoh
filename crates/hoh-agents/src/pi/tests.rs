//! The argv, refusal and verdict rules of the Pi adapter.

use super::*;

const HELP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/pi/1.5.0/help.txt"
);

fn launch() -> Launch {
    Launch {
        program: PathBuf::from("pi"),
        help: std::fs::read_to_string(HELP).expect("recorded help"),
        cwd: PathBuf::from("/tmp"),
        session_dir: PathBuf::from("/repo/.hoh/pi-sessions/S1"),
        session_id: "sid-1".to_owned(),
        resume: false,
        provider: None,
        model: None,
        settled_event: true,
    }
}

#[test]
fn the_session_is_on_every_command_line_and_never_ephemeral() {
    let mut l = launch();
    let argv = args(&l).expect("argv");
    assert_eq!(
        argv,
        [
            "--mode",
            "rpc",
            "--session-dir",
            "/repo/.hoh/pi-sessions/S1",
            "--session-id",
            "sid-1"
        ]
    );
    assert!(!argv.iter().any(|a| a == "--no-session"));
    l.provider = Some("zai".to_owned());
    l.model = Some("glm".to_owned());
    assert_eq!(
        args(&l).expect("argv")[6..],
        ["--provider", "zai", "--model", "glm"]
    );
    l.help = l.help.replace("--session-id", "");
    assert!(matches!(args(&l), Err(Blocked(d)) if d.contains("--session-id")));
}

#[test]
fn refusals_are_classified_by_their_recorded_text() {
    assert_eq!(
        refusal("No API key found for the selected model."),
        "BLOCKED_AUTH"
    );
    assert_eq!(refusal("Model not found: a/b"), "MODEL_UNAVAILABLE");
    assert_eq!(refusal("boom"), "PERMANENT_EXTERNAL");
}

#[test]
fn only_a_settled_turn_settles_and_a_cancel_wins() {
    let failed = |why: &str| (AttemptState::Failed, Some(why.to_owned()));
    let settled = Ending {
        settled: Some(SettledBy::AgentSettled),
        ..Ending::default()
    };
    assert_eq!(verdict(&settled), (AttemptState::Settled, None));
    for by in [SettledBy::Handled, SettledBy::State] {
        assert_eq!(
            verdict(&Ending {
                settled: Some(by),
                ..settled
            }),
            (AttemptState::Settled, None)
        );
    }
    assert_eq!(
        verdict(&Ending {
            aborted: true,
            ..settled
        }),
        (AttemptState::Cancelled, None)
    );
    let cut = Ending::default();
    assert_eq!(verdict(&cut), failed("PROTOCOL_INCOMPLETE"));
    assert_eq!(
        verdict(&Ending {
            tool_started: true,
            ..cut
        }),
        (
            AttemptState::UnknownEffect,
            Some("PROTOCOL_INCOMPLETE".to_owned())
        )
    );
    let all = Ending {
        too_large: true,
        deadline: true,
        logs_incomplete: true,
        failure: Some("BLOCKED_AUTH"),
        ..settled
    };
    assert_eq!(
        verdict(&Ending {
            cancelled: true,
            ..all
        }),
        (AttemptState::Cancelled, None)
    );
    assert_eq!(verdict(&all), failed("PROTOCOL_FRAME_TOO_LARGE"));
    assert_eq!(
        verdict(&Ending {
            too_large: false,
            ..all
        }),
        failed("DEADLINE_EXCEEDED")
    );
    assert_eq!(
        verdict(&Ending {
            too_large: false,
            deadline: false,
            logs_incomplete: false,
            ..all
        }),
        failed("BLOCKED_AUTH")
    );
}

#[test]
fn provider_failures_are_classified() {
    assert_eq!(provider_failure("No API key found for x"), "BLOCKED_AUTH");
    assert_eq!(provider_failure("401 Unauthorized"), "BLOCKED_AUTH");
    assert_eq!(
        provider_failure("Model not found: a/b"),
        "MODEL_UNAVAILABLE"
    );
    for m in [
        "429 Too Many Requests",
        "529 overloaded",
        "rate limit exceeded",
        "503 Service Unavailable",
        "Request timed out",
    ] {
        assert_eq!(provider_failure(m), "TRANSIENT_EXTERNAL", "{m}");
    }
    assert_eq!(
        provider_failure("400 malformed request"),
        "PERMANENT_EXTERNAL"
    );
    // A settled turn whose last message failed is a failure; a cancel still wins.
    let failed = Ending {
        settled: Some(SettledBy::AgentSettled),
        provider_error: Some("TRANSIENT_EXTERNAL"),
        ..Ending::default()
    };
    assert_eq!(
        verdict(&failed),
        (AttemptState::Failed, Some("TRANSIENT_EXTERNAL".to_owned()))
    );
    assert_eq!(
        verdict(&Ending {
            cancelled: true,
            ..failed
        }),
        (AttemptState::Cancelled, None)
    );
}

/// The files noted before a resumed start: every regular `*.jsonl` (symlinks followed) with its identity; a FIFO
/// would block pi itself, so it makes the directory unsafe (M1T05R-12).
#[test]
fn the_noted_session_files_are_the_regular_ones() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let sessions = dir.path().join("sessions");
    assert_eq!(
        session_files(&sessions),
        Ok(Vec::new()),
        "a missing directory has none"
    );
    std::fs::create_dir_all(&sessions).expect("sessions");
    let a = sessions.join("2026-11-04T00-00-00-000Z_sid-1.jsonl");
    std::fs::write(&a, "{}\n").expect("write");
    std::fs::write(sessions.join("notes.txt"), "x").expect("write");
    let outside = dir.path().join("elsewhere.jsonl");
    std::fs::write(&outside, "{}\n").expect("write");
    std::os::unix::fs::symlink(&outside, sessions.join("linked.jsonl")).expect("symlink");
    std::os::unix::fs::symlink(dir.path().join("gone"), sessions.join("dangling.jsonl"))
        .expect("symlink");
    let files = session_files(&sessions).expect("no unsafe entry");
    let id = |p: &std::path::Path| {
        let m = std::fs::metadata(p).expect("meta");
        (p.canonicalize().expect("canonical"), m.dev(), m.ino())
    };
    let mut want = vec![id(&a), id(&outside)];
    want.sort();
    assert_eq!(files, want);
    // pi skips what fails at once: a directory named like a session file, also one it may not search (M1T05R-15).
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(sessions.join("a_directory.jsonl")).expect("dir");
    let closed = sessions.join("closed.jsonl");
    std::fs::create_dir_all(&closed).expect("dir");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let seen = session_files(&sessions);
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(seen, Ok(want));
    // What would block or flood pi's read does not pass: a FIFO, a device.
    let fifo = sessions.join("0000_fifo.jsonl");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );
    assert_eq!(session_files(&sessions), Err("0000_fifo.jsonl".to_owned()));
    std::fs::remove_file(&fifo).expect("remove");
    std::os::unix::fs::symlink("/dev/zero", sessions.join("0001_zero.jsonl")).expect("symlink");
    assert_eq!(session_files(&sessions), Err("0001_zero.jsonl".to_owned()));
}

/// The status as pi writes it (pi-ai error-body.js `formatProviderError`: `<label> error (NNN): …` or `NNN: …`; a raw
/// SDK message starting `NNN` and a space, a colon or the end), or named as `HTTP NNN` / `status NNN` with word
/// boundaries; never a number inside a body, a key, an id or a unit (M1T05R-14/16).
#[test]
fn provider_statuses_are_read_as_pi_writes_them() {
    for (message, class) in [
        (
            r#"OpenAI API error (429): {"message":"Too Many Requests"}"#,
            "TRANSIENT_EXTERNAL",
        ),
        (
            r#"Azure OpenAI API error (503): {"message":"Service Unavailable"}"#,
            "TRANSIENT_EXTERNAL",
        ),
        (
            r#"OpenAI API error (401): {"message":"bad key"}"#,
            "BLOCKED_AUTH",
        ),
        ("OpenAI Decisions error (403): forbidden", "BLOCKED_AUTH"),
        (
            "llama.cpp error (500): slot unavailable",
            "TRANSIENT_EXTERNAL",
        ),
        (
            "Mistral API error (404): model foo-1 does not exist",
            "MODEL_UNAVAILABLE",
        ),
        ("error (413): request too large", "PERMANENT_EXTERNAL"),
        (r#"429: {"error":"slow down"}"#, "TRANSIENT_EXTERNAL"),
        ("429 Too Many Requests", "TRANSIENT_EXTERNAL"),
        ("403 status code (no body)", "BLOCKED_AUTH"),
        ("529", "TRANSIENT_EXTERNAL"),
        ("HTTP 429 from the upstream gateway", "TRANSIENT_EXTERNAL"),
        ("upstream returned status 503", "TRANSIENT_EXTERNAL"),
        ("request failed with status code 401", "BLOCKED_AUTH"),
        ("  No API key found for the selected model.", "BLOCKED_AUTH"),
        ("\tModel not found: x/y", "MODEL_UNAVAILABLE"),
        ("too many requests, slow down", "TRANSIENT_EXTERNAL"),
        ("429abc request failed: bad input", "PERMANENT_EXTERNAL"),
        (
            "Request status 429abc failed: bad input",
            "PERMANENT_EXTERNAL",
        ),
        ("HTTP 4290 is not a status", "PERMANENT_EXTERNAL"),
        ("400ms timeout exceeded", "TRANSIENT_EXTERNAL"),
        (
            r#"OpenAI API error (500): {"note":"401 in the body is not the status"}"#,
            "TRANSIENT_EXTERNAL",
        ),
        (
            "Something failed: OpenAI API error (401): later in the text",
            "PERMANENT_EXTERNAL",
        ),
    ] {
        assert_eq!(provider_failure(message), class, "{message}");
    }
}

#[test]
fn a_rate_limit_that_mentions_the_key_is_not_an_authentication_failure() {
    assert_eq!(
        provider_failure("429 rate limit reached for this API key"),
        "TRANSIENT_EXTERNAL"
    );
    assert_eq!(
        provider_failure("This model's context is too long for this API key's tier"),
        "PERMANENT_EXTERNAL"
    );
    assert_eq!(provider_failure("Invalid API key provided"), "BLOCKED_AUTH");
    assert_eq!(provider_failure("Incorrect API key"), "BLOCKED_AUTH");
    assert_eq!(provider_failure("authentication failed"), "BLOCKED_AUTH");
    // Numbers that belong to a key, a model or an id do not decide (M1T05R-13).
    assert_eq!(
        provider_failure("401 Invalid API key provided: sk-test429"),
        "BLOCKED_AUTH"
    );
    assert_eq!(
        provider_failure("Model not found: vendor/model-503"),
        "MODEL_UNAVAILABLE"
    );
    assert_eq!(
        provider_failure("No API key found for model-529"),
        "BLOCKED_AUTH"
    );
    assert_eq!(
        provider_failure("Request req_503abc failed: bad input"),
        "PERMANENT_EXTERNAL"
    );
    assert_eq!(
        provider_failure("503 Service Unavailable"),
        "TRANSIENT_EXTERNAL"
    );
    assert_eq!(
        provider_failure("404 model gpt-x does not exist"),
        "MODEL_UNAVAILABLE"
    );
    assert_eq!(provider_failure("400 bad request"), "PERMANENT_EXTERNAL");
    // Only the status says what these are.
    assert_eq!(
        provider_failure("401 Your token has expired"),
        "BLOCKED_AUTH"
    );
    assert_eq!(provider_failure("403 Forbidden"), "BLOCKED_AUTH");
    assert_eq!(provider_failure("502 Bad Gateway"), "TRANSIENT_EXTERNAL");
}
