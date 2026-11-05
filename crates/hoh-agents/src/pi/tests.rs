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

/// pi 1.5.0's findById, mirrored: the cases where pi finds the session must be found, those where it does not must
/// not (M1T05R-07/08).
#[test]
fn the_session_is_found_as_pi_finds_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("repo");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let cwd = cwd.canonicalize().expect("canonical cwd");
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions");
    let mut l = launch();
    l.cwd = cwd.clone();
    l.session_dir = sessions.clone();
    let header = |cwd: &str| {
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"sid-1\",\"timestamp\":\"2026-11-04T00:00:00.000Z\",\"cwd\":{}}}\n",
            serde_json::to_string(cwd).expect("json")
        )
    };
    let file = sessions.join("2026-11-04T00-00-00-000Z_sid-1.jsonl");
    let here = cwd.to_str().expect("utf-8 cwd");
    let found = |body: &str| {
        std::fs::write(&file, body).expect("write");
        find_session(&l)
    };
    // Found by pi: blank lines, a BOM-only line and lines that are not JSON come first; a long header; a relative cwd
    // (taken from pi's own cwd); a header without a newline at the end.
    for body in [
        header(here),
        format!("\n   \n{}", header(here)),
        format!("\u{feff}\n{}", header(here)),
        format!("not json\n{{broken\n{}", header(here)),
        format!("null\nfalse\n0\n\"\"\n{}", header(here)),
        header(here).replace(
            "\"version\":3",
            &format!("\"pad\":\"{}\",\"version\":3", "x".repeat(70 << 10)),
        ),
        header("."),
        header(&format!("{here}/sub/..")),
        header(here).trim_end().to_owned(),
    ] {
        assert_eq!(
            found(&body).as_deref(),
            Some(file.canonicalize().expect("file").as_path()),
            "{}",
            &body[..body.len().min(120)]
        );
    }
    // Not found by pi: empty, another cwd, the first JSON line is not a header, a number, no string id, a header past
    // 1 MiB, a cwd that names the same directory only through a symlink.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&cwd, &link).expect("symlink");
    for body in [
        String::new(),
        header("/elsewhere"),
        format!("{{\"type\":\"message\",\"id\":\"x\"}}\n{}", header(here)),
        header(here).replace("\"type\":\"session\"", "\"type\":\"message\""),
        format!("7\n{}", header(here)),
        header(here).replace("\"id\":\"sid-1\"", "\"id\":1"),
        format!("{}\n{}", "x".repeat((1 << 20) + 10), header(here)),
        header(link.to_str().expect("utf-8")),
        header(""),
    ] {
        assert_eq!(found(&body), None, "{}", &body[..body.len().min(120)]);
    }
    // A FIFO named like a session file is skipped, not opened (it would block).
    std::fs::remove_file(&file).expect("remove");
    let fifo = sessions.join("2026-11-04T00-00-00-000Z_sid-1.jsonl");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );
    let other = sessions.join("2026-11-05T00-00-00-000Z_sid-1.jsonl");
    std::fs::write(&other, header(here)).expect("write");
    assert_eq!(
        find_session(&l).as_deref(),
        Some(other.canonicalize().expect("other").as_path())
    );
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
}
