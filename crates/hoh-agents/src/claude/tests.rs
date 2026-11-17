//! The argv and verdict rules of the Claude adapter.

use super::*;

const HELP: &str = "Usage: claude [options]\n  -p, --print\n  --output-format <format>  text, json, stream-json\n  --verbose\n  --include-partial-messages\n  --resume [sessionId]\n  --dangerously-skip-permissions\n  --permission-mode <mode>\n";

fn launch() -> Launch {
    Launch {
        program: PathBuf::from("claude"),
        help: HELP.to_owned(),
        cwd: PathBuf::from("/tmp"),
        profile: crate::Profile::Yolo,
        partial: false,
        resume: None,
        model: None,
        root: false,
        // What the probe finds for HELP.
        capabilities: [
            ("structured_events", Some(true)),
            ("partial_messages", Some(true)),
            ("resume", Some(true)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
    }
}

#[test]
fn the_argv_comes_from_the_help_and_never_maps_auto() {
    let a = argv(&launch()).expect("argv");
    assert_eq!(
        a.args,
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            YOLO_FLAG
        ]
    );
    assert_eq!(a.capabilities["partial_messages"], true);
    let mut l = launch();
    l.partial = true;
    l.resume = Some("9fcce4a1".to_owned());
    let a = argv(&l).expect("argv");
    assert_eq!(
        a.args,
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            PARTIAL_FLAG,
            YOLO_FLAG,
            "--resume",
            "9fcce4a1"
        ]
    );
    assert!(
        !a.args
            .iter()
            .any(|x| x == "--permission-mode" || x == "auto")
    );
    // Partial deltas only when offered; the capability says so.
    l.help = HELP.replace("  --include-partial-messages\n", "");
    let a = argv(&l).expect("argv");
    assert!(!a.args.iter().any(|x| x == PARTIAL_FLAG));
    assert_eq!(a.capabilities["partial_messages"], false);
}

#[test]
fn a_turn_that_cannot_run_as_asked_is_blocked() {
    let blocked = |edit: fn(&mut Launch)| {
        let mut l = launch();
        edit(&mut l);
        argv(&l).expect_err("blocked")
    };
    blocked(|l| l.help = HELP.replace("  --dangerously-skip-permissions\n", ""));
    blocked(|l| l.root = true);
    blocked(|l| l.help = HELP.replace("stream-json", "text, json"));
    blocked(|l| {
        l.help = HELP.replace("  --resume [sessionId]\n", "");
        l.resume = Some("x".to_owned());
    });
    // Without the YOLO profile the bypass flag is neither needed nor added.
    let mut l = launch();
    l.profile = crate::Profile::Plain;
    l.root = true;
    l.help = HELP.replace("  --dangerously-skip-permissions\n", "");
    assert!(!argv(&l).expect("argv").args.iter().any(|x| x == YOLO_FLAG));
}

#[test]
fn only_a_clean_success_settles_and_a_cancel_always_wins() {
    let success = ResultFrame {
        subtype: Some("success".to_owned()),
        is_error: Some(false),
        text: None,
    };
    let ok = Some(Termination::Code(0));
    let clean = Ending {
        result: Some(&success),
        exit: ok,
        ..Ending::default()
    };
    assert_eq!(verdict(&clean), (AttemptState::Settled, None));
    let failed = |reason: &str| (AttemptState::Failed, Some(reason.to_owned()));
    let max_turns = ResultFrame {
        subtype: Some("error_max_turns".to_owned()),
        is_error: Some(true),
        text: None,
    };
    assert_eq!(
        verdict(&Ending {
            result: Some(&max_turns),
            exit: Some(Termination::Code(1)),
            ..clean
        }),
        failed("error_max_turns")
    );
    let error = ResultFrame {
        subtype: Some("success".to_owned()),
        is_error: Some(true),
        text: None,
    };
    let errored = Ending {
        result: Some(&error),
        ..clean
    };
    assert_eq!(verdict(&errored), failed("is_error"));
    assert_eq!(
        verdict(&Ending {
            result: None,
            ..clean
        }),
        failed("PROTOCOL_INCOMPLETE")
    );
    assert_eq!(
        verdict(&Ending {
            exit: Some(Termination::Code(1)),
            ..clean
        }),
        failed("exit code 1")
    );
    assert_eq!(
        verdict(&Ending {
            too_large: true,
            ..clean
        }),
        failed("PROTOCOL_FRAME_TOO_LARGE")
    );
    // A turn stopped by its deadline, or whose raw logs are incomplete, is no success whatever it printed.
    assert_eq!(
        verdict(&Ending {
            deadline: true,
            ..clean
        }),
        failed("DEADLINE_EXCEEDED")
    );
    assert_eq!(
        verdict(&Ending {
            logs_incomplete: true,
            ..clean
        }),
        failed("RAW_LOG_INCOMPLETE")
    );
    // An undelivered prompt spoils a success but does not hide a failure the result names.
    assert_eq!(
        verdict(&Ending {
            prompt_undelivered: true,
            ..clean
        }),
        failed("PROMPT_NOT_DELIVERED")
    );
    assert_eq!(
        verdict(&Ending {
            prompt_undelivered: true,
            ..errored
        }),
        failed("is_error")
    );
    // Claude's own authentication error marker turns a failed turn into BLOCKED_AUTH (recorded case unauthenticated).
    assert_eq!(
        verdict(&Ending {
            auth_failed: true,
            exit: Some(Termination::Code(1)),
            ..errored
        }),
        failed("BLOCKED_AUTH")
    );
    // After SIGINT Claude still prints result(error_during_execution) and exits 0: cancelled all the same, and a cancel
    // wins over every other cause.
    let interrupted = ResultFrame {
        subtype: Some("error_during_execution".to_owned()),
        is_error: Some(true),
        text: None,
    };
    for ending in [
        Ending {
            result: Some(&interrupted),
            ..clean
        },
        clean,
        Ending {
            too_large: true,
            deadline: true,
            logs_incomplete: true,
            prompt_undelivered: true,
            auth_failed: true,
            result: None,
            exit: None,
            ..clean
        },
    ] {
        assert_eq!(
            verdict(&Ending {
                cancelled: true,
                ..ending
            }),
            (AttemptState::Cancelled, None)
        );
    }
}

#[test]
fn a_chosen_model_is_passed_only_where_the_help_offers_it() {
    // docs/adr/ADR-022.md decision 14 (REQ-018: a binding's model): `--model` from the help, never assumed.
    let mut l = launch();
    l.help = format!("{HELP}  --model <model>\n");
    l.model = Some("m-test".to_owned());
    let args = argv(&l).expect("argv").args;
    assert_eq!(args[args.len() - 2..], ["--model", "m-test"]);
    l.help = HELP.to_owned();
    assert_eq!(
        argv(&l).expect_err("blocked"),
        Blocked("claude --help offers no --model".to_owned())
    );
    l.model = None;
    assert!(!argv(&l).expect("argv").args.iter().any(|x| x == "--model"));
}

#[test]
fn a_sessions_capabilities_are_those_its_argv_gives() {
    // docs/adr/ADR-022.md decision 9: the binder records what a turn of this launch would have.
    let l = launch();
    assert_eq!(
        session_capabilities(&l),
        Ok(argv(&l).expect("argv").capabilities)
    );
    let mut blocked = launch();
    blocked.help = HELP.replace("stream-json", "text, json");
    assert!(session_capabilities(&blocked).is_err());
}

#[test]
fn a_yolo_session_runs_commands_where_the_probe_found_it() {
    // docs/adr/ADR-030.md decision 12: the adapter runs commands under the YOLO profile only, and the session has
    // what the probe found (left out where it could not tell).
    for (probed, profile, expected) in [
        (Some(true), crate::Profile::Yolo, Some(true)),
        (Some(false), crate::Profile::Yolo, Some(false)),
        (None, crate::Profile::Yolo, None),
        (Some(true), crate::Profile::Plain, Some(false)),
    ] {
        let mut l = launch();
        l.capabilities
            .insert("command_execution".to_owned(), probed);
        l.profile = profile;
        let caps = session_capabilities(&l).expect("capabilities");
        assert_eq!(
            caps.get("command_execution").and_then(Value::as_bool),
            expected,
            "probed {probed:?}, profile {profile:?}: {caps}"
        );
    }
}

/// The frames of a turn as Claude's stdout.
fn stdout(frames: &[Value]) -> Vec<u8> {
    frames
        .iter()
        .flat_map(|f| format!("{f}\n").into_bytes())
        .collect()
}

/// When a turn of `frames` says the limit it failed on lifts: its replay's [`Driven::retry_at`].
fn retry_at(frames: &[Value]) -> Option<std::time::SystemTime> {
    judged(&stdout(frames)).ok()?.0.retry_at()
}

fn at(secs: u64) -> Option<std::time::SystemTime> {
    Some(std::time::UNIX_EPOCH + Duration::from_secs(secs))
}

const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"sid","model":"m"}"#;

fn init() -> Value {
    serde_json::from_str(INIT).expect("init")
}

fn limit_event(status: &str, resets_at: Value) -> Value {
    json!({"type": "rate_limit_event", "rate_limit_info": {"status": status, "resetsAt": resets_at,
           "rateLimitType": "five_hour"}, "session_id": "sid"})
}

/// The API-error assistant frame of a 429: `typed` adds the @internal typed kind and its parameters.
fn limit_error(error: &str, typed: Option<(&str, Value)>) -> Value {
    let mut frame = json!({"type": "assistant", "parent_tool_use_id": null, "session_id": "sid", "error": error,
                           "is_api_error_message": true, "api_error_status": 429,
                           "message": {"role": "assistant", "content": [{"type": "text", "text": "API Error"}]}});
    if let Some((kind, resets_at)) = typed {
        frame["api_error"] = json!(kind);
        frame["api_error_params"] =
            json!({"rate_limit_info": {"status": "rejected", "resetsAt": resets_at}});
    }
    frame
}

fn failed_result() -> Value {
    json!({"type": "result", "subtype": "success", "is_error": true, "api_error_status": 429,
           "terminal_reason": "api_error", "result": "API Error", "session_id": "sid"})
}

fn settled_result() -> Value {
    json!({"type": "result", "subtype": "success", "is_error": false, "result": "ok", "session_id": "sid"})
}

#[test]
fn a_failed_turns_usage_limit_says_when_it_lifts() {
    // M2T07R-256 (REQ-042 behaviour 1): the error frame's own record of a usage limit (@internal fields) gives the
    // reset; without a typed kind the turn's last rejected rate_limit_event (public) does.
    let typed = limit_error("rate_limit", Some(("usage_limit_reached", json!(2_000))));
    let event = limit_event("rejected", json!(1_000));
    assert_eq!(
        retry_at(&[init(), event.clone(), typed.clone(), failed_result()]),
        at(2_000)
    );
    assert_eq!(retry_at(&[init(), typed, failed_result()]), at(2_000));
    let untyped = limit_error("rate_limit", None);
    assert_eq!(
        retry_at(&[init(), event.clone(), untyped.clone(), failed_result()]),
        at(1_000)
    );
    // A plain 429 (an API key, a throttle): nothing says when.
    assert_eq!(retry_at(&[init(), untyped.clone(), failed_result()]), None);
    // A limit that was lifted again, by a later event, no longer says when.
    assert_eq!(
        retry_at(&[
            init(),
            event.clone(),
            limit_event("allowed", json!(1_000)),
            untyped.clone(),
            failed_result()
        ]),
        None
    );
    // A typed kind that is not a usage limit (credits, an entitlement) is not lifted by time.
    let credits = limit_error(
        "rate_limit",
        Some(("model_requires_usage_credits", json!(2_000))),
    );
    assert_eq!(
        retry_at(&[init(), event.clone(), credits, failed_result()]),
        None
    );
    // Another error (a 503) is no limit.
    assert_eq!(
        retry_at(&[
            init(),
            event,
            limit_error("server_error", None),
            failed_result()
        ]),
        None
    );
}

#[test]
fn only_a_turn_that_failed_on_the_limit_carries_its_reset() {
    let typed = limit_error("rate_limit", Some(("usage_limit_reached", json!(2_000))));
    // Settled after all (a fallback model, a retry that worked).
    assert_eq!(retry_at(&[init(), typed.clone(), settled_result()]), None);
    // The conversation went on after the limit: a later main-thread message without an error.
    let reply = json!({"type": "assistant", "parent_tool_use_id": null, "session_id": "sid", "message": {"role": "assistant"}});
    assert_eq!(
        retry_at(&[init(), typed.clone(), reply, failed_result()]),
        None
    );
    // A subagent's frame is not the turn's.
    let mut sub = typed.clone();
    sub["parent_tool_use_id"] = json!("toolu_1");
    assert_eq!(retry_at(&[init(), sub, failed_result()]), None);
    // A stream cut short before the result, and a cancel, end otherwise.
    assert_eq!(retry_at(&[init(), typed.clone()]), None);
    let mut cancelled = stdout(&[init(), typed]);
    cancelled.extend_from_slice(format!("{}\n", crate::adapter::REPLAY_CANCEL).as_bytes());
    cancelled.extend_from_slice(&stdout(&[failed_result()]));
    assert_eq!(
        judged(&cancelled).ok().and_then(|(d, _)| d.retry_at()),
        None
    );
}

#[test]
fn a_rejected_event_counts_only_for_the_failure_it_came_with() {
    // RA-04 (M2T07R-256 review): a successful main-thread message after a rejected account-wide rate_limit_event (five
    // hours) shows the limit no longer held; a later untyped 429 is a new failure that event says nothing about
    // (Codex's replay: rejected, success, untyped 429, failed result). A rejected event after the success does count.
    let event = limit_event("rejected", json!(1_000));
    let untyped = limit_error("rate_limit", None);
    let reply = json!({"type": "assistant", "parent_tool_use_id": null, "session_id": "sid",
                       "message": {"role": "assistant", "model": "m", "content": [{"type": "text", "text": "ok"}]}});
    assert_eq!(
        retry_at(&[
            init(),
            event.clone(),
            reply.clone(),
            untyped.clone(),
            failed_result()
        ]),
        None
    );
    assert_eq!(
        retry_at(&[init(), reply, event, untyped, failed_result()]),
        at(1_000)
    );
}

#[test]
fn only_a_success_of_the_same_thread_and_limit_clears_a_rejected_event() {
    // RA-103 (M2T07R-256 review round 2; design correction rule 4): a subagent's success never clears the main
    // thread's evidence (it may run another model: Codex's replay, seven_day_opus rejected, a Sonnet subagent's
    // success, an untyped 429); a model limit is cleared only by a main-thread success of the model it was recorded
    // under. RA-04's "a subagent's success clears it" was the wrong expectation.
    let opus = json!({"type": "system", "subtype": "init", "session_id": "sid", "model": "claude-opus-5-5"});
    let weekly_opus = json!({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected",
                             "resetsAt": 1_000, "rateLimitType": "seven_day_opus"}, "session_id": "sid"});
    let success = |parent: Value, model: &str| {
        json!({"type": "assistant", "parent_tool_use_id": parent, "session_id": "sid",
               "message": {"role": "assistant", "model": model, "content": [{"type": "text", "text": "ok"}]}})
    };
    let untyped = limit_error("rate_limit", None);
    for (frame, kept) in [
        (success(json!("toolu_1"), "claude-sonnet-5-5"), true),
        (success(json!("toolu_1"), "claude-opus-5-5"), true),
        (success(Value::Null, "claude-sonnet-5-5"), true),
        (success(Value::Null, "claude-opus-5-5"), false),
    ] {
        assert_eq!(
            retry_at(&[
                opus.clone(),
                weekly_opus.clone(),
                frame.clone(),
                untyped.clone(),
                failed_result()
            ]),
            if kept { at(1_000) } else { None },
            "{frame}"
        );
    }
    // An account-wide limit (five hours) is cleared by any main-thread success, but not by a subagent's.
    let five_hours = limit_event("rejected", json!(1_000));
    assert_eq!(
        retry_at(&[
            opus.clone(),
            five_hours.clone(),
            success(json!("toolu_1"), "claude-opus-5-5"),
            untyped.clone(),
            failed_result()
        ]),
        at(1_000)
    );
    assert_eq!(
        retry_at(&[
            opus,
            five_hours,
            success(Value::Null, "claude-sonnet-5-5"),
            untyped,
            failed_result()
        ]),
        None
    );
}

#[test]
fn a_reset_that_is_no_instant_is_none() {
    for resets_at in [
        json!(0),
        json!(-5),
        json!(1.5),
        json!("2000"),
        json!(null),
        json!(u64::MAX),
    ] {
        let typed = limit_error(
            "rate_limit",
            Some(("usage_limit_reached", resets_at.clone())),
        );
        assert_eq!(
            retry_at(&[init(), typed, failed_result()]),
            None,
            "{resets_at}"
        );
        let untyped = limit_error("rate_limit", None);
        assert_eq!(
            retry_at(&[
                init(),
                limit_event("rejected", resets_at.clone()),
                untyped,
                failed_result()
            ]),
            None,
            "{resets_at}"
        );
    }
}

/// The provider-fault recordings of claude 2.1.325 (tests/fixtures/protocol/_provider-faults, record_faults.py).
fn fault(case: &str) -> (Vec<u8>, Value) {
    let dir = format!(
        "{}/../../tests/fixtures/protocol/_provider-faults/claude/2.1.325/{case}",
        env!("CARGO_MANIFEST_DIR")
    );
    let stdout = std::fs::read(format!("{dir}/stdout.jsonl")).expect("stdout.jsonl");
    let meta = serde_json::from_slice(&std::fs::read(format!("{dir}/meta.json")).expect("meta"))
        .expect("meta.json");
    (stdout, meta)
}

#[test]
fn the_recorded_usage_limit_says_when_it_lifts_and_a_recorded_retry_after_does_not() {
    // M2T07R-256: a subscriber's 5-hour limit carries the reset the fake provider sent; a 429 or 503 with a Retry-After
    // reaches the stream with no value at all (Claude waits a short one itself and drops a long one).
    let (bytes, meta) = fault("fault-usage-limit");
    let sent = meta["provider"]["requests"][0]["headers"]["anthropic-ratelimit-unified-reset"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .expect("the reset sent");
    let (driven, _) = judged(&bytes).expect("judged");
    assert_eq!(driven.verdict().0, AttemptState::Failed);
    assert_eq!(driven.retry_at(), at(sent));
    for case in [
        "fault-429-retry-after-long",
        "fault-429-retry-after-short",
        "fault-503-retry-after-long",
    ] {
        let (driven, _) = judged(&fault(case).0).expect("judged");
        assert_eq!(driven.verdict().0, AttemptState::Failed, "{case}");
        assert_eq!(driven.retry_at(), None, "{case}");
    }
}

/// The recorded claude 2.1.318 help (the read-only profile's declarations, docs/adr/ADR-032.md decision 3).
const RECORDED_HELP: &str =
    include_str!("../../../../tests/fixtures/protocol/claude/2.1.318/help.txt");

fn read_only_launch() -> Launch {
    Launch {
        help: RECORDED_HELP.to_owned(),
        profile: crate::Profile::ReadOnly,
        ..launch()
    }
}

/// `help` without the declaration line of `flag` (its description lines stay, as another option's text).
fn without_declaration(help: &str, flag: &str) -> String {
    help.lines()
        .filter(|line| crate::adapter::declared(line).is_none_or(|names| !names.contains(&flag)))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn the_read_only_argv_is_decision_3s_row() {
    // ADR-032 decision 3: the read-only profile replaces only where the YOLO flag stands; the rest is today's rules.
    let a = argv(&read_only_launch()).expect("argv");
    assert_eq!(
        a.args,
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--tools",
            "Read,Grep,Glob",
            "--allowedTools",
            "Read,Grep,Glob",
            "--permission-mode",
            "dontAsk",
            "--strict-mcp-config"
        ]
    );
    let mut l = read_only_launch();
    l.partial = true;
    l.resume = Some("9fcce4a1".to_owned());
    l.model = Some("opus".to_owned());
    assert_eq!(
        argv(&l).expect("argv").args,
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            PARTIAL_FLAG,
            "--tools",
            "Read,Grep,Glob",
            "--allowedTools",
            "Read,Grep,Glob",
            "--permission-mode",
            "dontAsk",
            "--strict-mcp-config",
            "--resume",
            "9fcce4a1",
            "--model",
            "opus"
        ]
    );
    assert!(!argv(&l).expect("argv").args.iter().any(|x| x == YOLO_FLAG));
}

#[test]
fn the_read_only_profile_is_blocked_without_any_of_its_options() {
    // ADR-032 decision 2: no fallback; each missing declaration (one change to the recorded help) blocks, naming it.
    for (help, missing) in [
        (without_declaration(RECORDED_HELP, "--tools"), "--tools"),
        (
            without_declaration(RECORDED_HELP, "--allowedTools"),
            "--allowedTools",
        ),
        (
            without_declaration(RECORDED_HELP, "--strict-mcp-config"),
            "--strict-mcp-config",
        ),
        (
            without_declaration(RECORDED_HELP, "--permission-mode"),
            "--permission-mode",
        ),
        (RECORDED_HELP.replace("\"dontAsk\", ", ""), "dontAsk"),
    ] {
        let l = Launch {
            help,
            ..read_only_launch()
        };
        assert!(
            matches!(argv(&l), Err(Blocked(ref why)) if why.contains(missing)),
            "{missing}: {:?}",
            argv(&l)
        );
    }
}

#[test]
fn the_read_only_profile_runs_as_root_and_the_yolo_profile_does_not() {
    let l = Launch {
        root: true,
        ..read_only_launch()
    };
    assert!(argv(&l).is_ok());
    let l = Launch {
        profile: crate::Profile::Yolo,
        ..l
    };
    assert!(matches!(argv(&l), Err(Blocked(why)) if why.contains("root")));
}

#[test]
fn a_read_only_turn_records_the_session_a_yolo_turn_records() {
    // ADR-032 decision 4: the session row says which profiles the session can start in, whatever the turn's profile,
    // so a Commander's session stays fresh; read_only_workspace is what the probe found, where the read-only argv
    // builds for this help.
    let probed: crate::capabilities::Probed = [
        ("structured_events", Some(true)),
        ("resume", Some(true)),
        ("command_execution", Some(true)),
        ("read_only_workspace", Some(true)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let read_only = Launch {
        capabilities: probed.clone(),
        ..read_only_launch()
    };
    let yolo = Launch {
        profile: crate::Profile::Yolo,
        ..read_only.clone()
    };
    let caps = session_capabilities(&read_only).expect("capabilities");
    assert_eq!(caps, session_capabilities(&yolo).expect("capabilities"));
    assert_eq!(caps["read_only_workspace"], true, "{caps}");
    assert_eq!(caps["command_execution"], true, "{caps}");
    let without = Launch {
        help: without_declaration(RECORDED_HELP, "--strict-mcp-config"),
        ..yolo
    };
    let caps = session_capabilities(&without).expect("capabilities");
    assert_eq!(caps["read_only_workspace"], false, "{caps}");
}
