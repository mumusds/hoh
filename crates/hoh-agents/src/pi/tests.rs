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
        skills: Vec::new(),
        resume: false,
        provider: None,
        model: None,
        settled_event: true,
        profile: crate::Profile::Yolo,
        capabilities: crate::capabilities::Probed::new(),
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

/// `agent_settled` is relied on only for a version whose recorded traffic has it; any other version checks for idle
/// with `get_state` (ADR-013 decision 3), which ends a turn on `agent_settled` too (M1R-06).
#[test]
fn only_a_recorded_version_is_trusted_to_send_agent_settled() {
    // Every version trusted has recorded traffic with the event (M1R-09).
    for version in SETTLED_EVENT_VERSIONS {
        let recorded = std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/protocol/pi/{version}/rpc-prompt/stdout.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("recorded traffic");
        assert!(recorded.contains("\"agent_settled\""), "{version}");
    }
    assert!(emits_settled(Some("1.5.0")));
    assert!(emits_settled(Some("1.5.0\n")));
    // 1.6.0's recorded traffic has the event too (tests/fixtures/protocol/pi/1.6.0, PI16).
    assert!(emits_settled(Some("1.6.0")));
    assert!(emits_settled(Some("1.6.0\n")));
    for other in [
        None,
        Some(""),
        Some("1.5.1"),
        Some("1.5.2"),
        Some("1.6.1"),
        Some("1.7.0"),
        Some("pi 1.5.0"),
        Some("pi 1.6.0"),
        Some("1.5.0-beta"),
        Some("1.6.0-beta"),
    ] {
        assert!(!emits_settled(other), "{other:?}");
    }
}

#[test]
fn a_sessions_capabilities_are_events_and_resume() {
    // docs/adr/ADR-022.md decision 9, as run() records them.
    let mut l = launch();
    l.capabilities = [
        ("structured_events", Some(true)),
        ("resume", Some(true)),
        ("mid_turn_steer", Some(true)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let caps = session_capabilities(&l).expect("capabilities");
    assert_eq!(caps["structured_events"], true);
    assert_eq!(caps["resume"], true);
    assert_ne!(caps["mid_turn_steer"], true, "pi takes no steer here");
}

#[test]
fn a_session_runs_commands_where_the_probe_found_it() {
    // docs/adr/ADR-030.md decision 12: pi runs commands on every turn, so the session has what the probe found (left
    // out where it could not tell).
    for (probed, expected) in [
        (Some(true), Some(true)),
        (Some(false), Some(false)),
        (None, None),
    ] {
        let mut l = launch();
        l.capabilities
            .insert("command_execution".to_owned(), probed);
        let caps = session_capabilities(&l).expect("capabilities");
        assert_eq!(
            caps.get("command_execution").and_then(Value::as_bool),
            expected,
            "probed {probed:?}: {caps}"
        );
    }
}

/// `help` without the declaration line of `flag`.
fn without_declaration(help: &str, flag: &str) -> String {
    help.lines()
        .filter(|line| crate::adapter::declared(line).is_none_or(|names| !names.contains(&flag)))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn the_read_only_args_are_decision_3s_row() {
    // ADR-032 decision 3 (Q-pi-ext (A)): the allowlist, no MCP, no extension at all; then today's provider and model.
    let mut l = launch();
    l.profile = crate::Profile::ReadOnly;
    l.provider = Some("p".to_owned());
    l.model = Some("m".to_owned());
    assert_eq!(
        args(&l).expect("args"),
        [
            "--mode",
            "rpc",
            "--session-dir",
            "/repo/.hoh/pi-sessions/S1",
            "--session-id",
            "sid-1",
            "--tools",
            "read,grep,find,ls",
            "--no-mcp",
            "--no-extensions",
            "--provider",
            "p",
            "--model",
            "m"
        ]
    );
    assert!(!args(&l).expect("args").iter().any(|a| a == "-e"));
    // Yolo and Plain start as today.
    for profile in [crate::Profile::Yolo, crate::Profile::Plain] {
        l.profile = profile;
        assert!(
            !args(&l)
                .expect("args")
                .iter()
                .any(|a| a == "--tools" || a == "--no-extensions")
        );
    }
}

#[test]
fn the_read_only_profile_is_blocked_without_any_of_its_options() {
    let recorded = launch().help;
    for (help, missing) in [
        (without_declaration(&recorded, "--tools"), "--tools"),
        (without_declaration(&recorded, "--no-mcp"), "--no-mcp"),
        (
            without_declaration(&recorded, "--no-extensions"),
            "--no-extensions",
        ),
        (
            recorded.replace(
                "  ls         - List directory contents (read-only, off by default)\n",
                "",
            ),
            "ls",
        ),
    ] {
        let mut l = launch();
        l.profile = crate::Profile::ReadOnly;
        l.help = help;
        assert!(
            matches!(args(&l), Err(Blocked(ref why)) if why.contains(missing)),
            "{missing}: {:?}",
            args(&l)
        );
    }
}

#[test]
fn a_read_only_turn_records_the_session_a_yolo_turn_records() {
    let mut l = launch();
    l.capabilities = [
        ("structured_events", Some(true)),
        ("resume", Some(true)),
        ("command_execution", Some(true)),
        ("read_only_workspace", Some(true)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let yolo = session_capabilities(&l).expect("capabilities");
    l.profile = crate::Profile::ReadOnly;
    assert_eq!(session_capabilities(&l).expect("capabilities"), yolo);
    assert_eq!(yolo["read_only_workspace"], true, "{yolo}");
    l.profile = crate::Profile::Yolo;
    l.help = without_declaration(&l.help, "--no-extensions");
    assert_eq!(
        session_capabilities(&l).expect("capabilities")["read_only_workspace"],
        false
    );
}

/// A committed S0 excerpt (docs/adr/ADR-032.md): its stdout and request log without their header lines, and its
/// observations.
fn excerpt(case: &str) -> (Vec<u8>, Vec<Value>, Value) {
    let base = format!(
        "{}/../../tests/fixtures/commander-readonly/excerpts/pi/{case}",
        env!("CARGO_MANIFEST_DIR")
    );
    let read = |kind: &str| std::fs::read_to_string(format!("{base}.{kind}")).expect(kind);
    let stdout: String = read("stdout.excerpt.jsonl")
        .lines()
        .skip(1)
        .map(|l| format!("{l}\n"))
        .collect();
    let log = read("stdin.excerpt.jsonl")
        .lines()
        .skip(1)
        .map(|l| serde_json::from_str(l).expect("a command"))
        .collect();
    let meta: Value = serde_json::from_str(&read("meta.json")).expect("meta");
    (stdout.into_bytes(), log, meta["observations"].clone())
}

#[test]
fn the_replay_of_a_recorded_turn_checks_the_session_file_as_live_does() {
    // ADR-032 design correction 7 item 2 (M2T20D-801): the resumed session's file identity is the recorded one, judged
    // by the check live and replay share; the commands come from the launch alone, compared with the request log.
    use crate::observed::request_log_problem;
    for case in ["noext-new", "noext-resumed", "control"] {
        let (stdout, log, observed) = excerpt(case);
        let facts = replay_facts(&stdout, &observed);
        assert_eq!(
            facts.state,
            AttemptState::Settled,
            "{case}: {:?}",
            facts.failure
        );
        assert!(
            facts
                .final_text
                .as_deref()
                .is_some_and(|t| t.contains("```hoh-decisions")),
            "{case}"
        );
        let sent = facts.sent.expect("sent");
        assert_eq!(request_log_problem(&sent, &log), None, "{case}: {sent:?}");
    }
    let (stdout, log, observed) = excerpt("noext-resumed");
    let other_inode = {
        let mut o = observed["opened"].clone();
        o[2] = json!(o[2].as_u64().expect("inode") + 1);
        o
    };
    for (field, value) in [
        ("opened", Value::Null),
        ("opened", other_inode),
        ("noted", json!([])),
    ] {
        let mut changed = observed.clone();
        changed[field] = value.clone();
        let facts = replay_facts(&stdout, &changed);
        assert_eq!(
            facts.failure.as_deref(),
            Some("SESSION_MISMATCH"),
            "{field} = {value}"
        );
        // No prompt is sent after a failed check: the log (with its prompt) is not the adapter's.
        assert!(request_log_problem(&facts.sent.expect("sent"), &log).is_some());
    }
    // A new session's file is not checked.
    let (stdout, _, observed) = excerpt("noext-new");
    let mut changed = observed;
    changed["opened"] = json!(["/elsewhere", 1, 2]);
    assert_eq!(replay_facts(&stdout, &changed).state, AttemptState::Settled);
    // pi's recordings before run 20261117T002630Z have no launch: not observed, never settled.
    let (stdout, _, observed) = excerpt("ro-new");
    let facts = replay_facts(&stdout, &observed);
    assert!(
        facts
            .failure
            .as_deref()
            .is_some_and(|f| f.starts_with(crate::observed::NOT_OBSERVED))
    );
}

#[test]
fn the_session_check_is_one_function() {
    let mut l = launch();
    let file = (PathBuf::from("/s/a.jsonl"), 1, 2);
    assert!(session_matches(&l, &[], Some("sid-1"), None));
    assert!(!session_matches(&l, &[], Some("other"), None));
    assert!(!session_matches(&l, &[], None, None));
    l.resume = true;
    assert!(session_matches(
        &l,
        std::slice::from_ref(&file),
        Some("sid-1"),
        Some(&file)
    ));
    assert!(!session_matches(
        &l,
        std::slice::from_ref(&file),
        Some("sid-1"),
        None
    ));
    assert!(!session_matches(
        &l,
        std::slice::from_ref(&file),
        Some("sid-1"),
        Some(&(PathBuf::from("/s/a.jsonl"), 1, 3))
    ));
    assert!(!session_matches(&l, &[], Some("sid-1"), Some(&file)));
}

#[test]
fn native_skill_flags_require_the_probed_option_and_keep_read_only_profile() {
    let mut l = launch();
    l.skills = vec![
        l.session_dir.join("skills/tdd"),
        l.session_dir.join("skills/implementation"),
    ];
    l.profile = crate::Profile::ReadOnly;
    let argv = args(&l).unwrap();
    let loaded: Vec<_> = argv
        .windows(2)
        .filter(|p| p[0] == "--skill")
        .map(|p| p[1].as_str())
        .collect();
    assert_eq!(
        loaded,
        [
            "/repo/.hoh/pi-sessions/S1/skills/tdd",
            "/repo/.hoh/pi-sessions/S1/skills/implementation"
        ]
    );
    assert!(argv.iter().any(|a| a == "--no-extensions"));
    assert!(argv.iter().any(|a| a == "--no-skills"));
    assert!(
        argv.windows(2)
            .any(|p| p == ["--tools", "read,grep,find,ls"])
    );
    l.help = l
        .help
        .lines()
        .filter(|line| !line.contains("--skill "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(matches!(args(&l), Err(Blocked(why)) if why.contains("--skill")));
    l.help = launch().help;
    l.help = without_declaration(&l.help, "--no-skills");
    assert!(matches!(args(&l), Err(Blocked(why)) if why.contains("--no-skills")));
    l.skills.clear();
    assert!(args(&l).is_ok());
}
