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
        root: false,
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
    assert_eq!(
        verdict(false, false, Some(&success), ok),
        (AttemptState::Settled, None)
    );
    let failed = |reason: &str| (AttemptState::Failed, Some(reason.to_owned()));
    let max_turns = ResultFrame {
        subtype: Some("error_max_turns".to_owned()),
        is_error: Some(true),
        text: None,
    };
    assert_eq!(
        verdict(false, false, Some(&max_turns), Some(Termination::Code(1))),
        failed("error_max_turns")
    );
    let error = ResultFrame {
        subtype: Some("success".to_owned()),
        is_error: Some(true),
        text: None,
    };
    assert_eq!(verdict(false, false, Some(&error), ok), failed("is_error"));
    assert_eq!(
        verdict(false, false, None, ok),
        failed("PROTOCOL_INCOMPLETE")
    );
    assert_eq!(
        verdict(false, false, Some(&success), Some(Termination::Code(1))),
        failed("exit code 1")
    );
    assert_eq!(
        verdict(false, true, Some(&success), ok),
        failed("PROTOCOL_FRAME_TOO_LARGE")
    );
    // After SIGINT Claude still prints result(error_during_execution) and exits 0: cancelled all the same.
    let interrupted = ResultFrame {
        subtype: Some("error_during_execution".to_owned()),
        is_error: Some(true),
        text: None,
    };
    assert_eq!(
        verdict(true, false, Some(&interrupted), ok),
        (AttemptState::Cancelled, None)
    );
    assert_eq!(
        verdict(true, false, Some(&success), ok),
        (AttemptState::Cancelled, None)
    );
}
