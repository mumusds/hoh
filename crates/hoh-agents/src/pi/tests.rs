//! The argv, error-class and verdict rules of the Pi adapter.

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

/// Pi 1.5.0 has no structured error codes, so its error texts are classified only by the recorded patterns
/// (REQ-026, CR-003; docs/13): `No API key found` → BLOCKED_AUTH, `Model not found` → MODEL_UNAVAILABLE, anything else
/// PERMANENT_EXTERNAL with the text kept. That holds for a refused prompt and for a provider error after acceptance:
/// HoH does not read statuses or phrases out of provider text (M1T05R-17..20; pi has already retried what it judged
/// transient).
#[test]
fn pi_errors_are_classified_only_by_their_recorded_text() {
    for (text, class) in [
        ("No API key found for the selected model.", "BLOCKED_AUTH"),
        ("  No API key found for key sk-status429", "BLOCKED_AUTH"),
        ("Model not found: a/b", "MODEL_UNAVAILABLE"),
        ("\tModel not found: vendor/status503", "MODEL_UNAVAILABLE"),
        ("boom", "PERMANENT_EXTERNAL"),
    ] {
        assert_eq!(error_class(text), class, "{text}");
    }
    for text in [
        r#"OpenAI API error (429): {"message":"Too Many Requests"}"#,
        r#"Azure OpenAI API error (401): {"message":"Your token has expired"}"#,
        r#"OpenAI API error (404): {"message":"model gpt-x does not exist"}"#,
        r#"acme/eu API error (503): {"message":"Service Unavailable"}"#,
        r#"{"error":{"code":429,"message":"Resource exhausted","status":"RESOURCE_EXHAUSTED"}}"#,
        "Throttling error: Resource exhausted",
        "AccessDeniedException: Permission denied",
        "429 Too Many Requests",
        "401 Unauthorized",
        "rate limit exceeded",
        "Request timed out",
        "Invalid API key provided",
        "the provider said: No API key found",
    ] {
        assert_eq!(error_class(text), "PERMANENT_EXTERNAL", "{text}");
    }
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

/// A settled turn whose last message failed is a failure; a cancel still wins.
#[test]
fn a_provider_error_fails_a_settled_turn() {
    let failed = Ending {
        settled: Some(SettledBy::AgentSettled),
        provider_error: Some("PERMANENT_EXTERNAL"),
        ..Ending::default()
    };
    assert_eq!(
        verdict(&failed),
        (AttemptState::Failed, Some("PERMANENT_EXTERNAL".to_owned()))
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
    // A socket: pi's open fails at once (ENXIO) and it goes on (M1T05R-21); also through a symlink.
    let socket =
        std::os::unix::net::UnixListener::bind(sessions.join("a_socket.jsonl")).expect("bind");
    std::os::unix::fs::symlink(
        sessions.join("a_socket.jsonl"),
        sessions.join("linked_socket.jsonl"),
    )
    .expect("symlink");
    assert_eq!(seen, Ok(want.clone()));
    assert_eq!(session_files(&sessions), Ok(want));
    drop(socket);
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
