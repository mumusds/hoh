//! The argv and verdict rules of the Claude adapter.

use super::*;

const HELP: &str = "Usage: claude [options]\n  -p, --print\n  --output-format <format>  text, json, stream-json\n  --verbose\n  --include-partial-messages\n  --resume [sessionId]\n  --dangerously-skip-permissions\n  --permission-mode <mode>\n";

fn launch() -> Launch {
    Launch {
        program: PathBuf::from("claude"),
        help: HELP.to_owned(),
        cwd: PathBuf::from("/tmp"),
        yolo: true,
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
    l.yolo = false;
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
