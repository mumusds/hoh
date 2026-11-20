//! The command surface, argument by argument.

use super::*;

fn parse_words(words: &[&str]) -> Result<Invocation, UsageError> {
    let args: Vec<OsString> = words.iter().map(OsString::from).collect();
    parse(&args)
}

fn ok(words: &[&str]) -> Invocation {
    parse_words(words).unwrap_or_else(|e| panic!("{words:?}: {e:?}"))
}

fn refused(words: &[&str]) -> String {
    match parse_words(words) {
        Err(UsageError(message)) => message,
        Ok(invocation) => panic!("{words:?} was accepted as {invocation:?}"),
    }
}

#[test]
fn help_and_version() {
    for words in [&["--help"][..], &["help"], &[]] {
        let expected = if words.is_empty() {
            Invocation::Workspace { project: None }
        } else {
            help_on(Topic::Top)
        };
        assert_eq!(ok(words), expected, "{words:?}");
    }
    assert_eq!(ok(&["attach", "--help"]), help_on(Topic::Attach));
    assert_eq!(ok(&["help", "web"]), help_on(Topic::Web));
    assert_eq!(ok(&["doctor", "--live", "--help"]), help_on(Topic::Doctor));
    assert_eq!(ok(&["--version"]), Invocation::Version);
    refused(&["attach", "--version"]);
    // Help and version do not make an invalid command line valid (M1T06R-01).
    for words in [
        &["--resume", "r1", "--version"][..],
        &["--project", "/tmp", "--version"],
        &["-p", "x", "--version"],
        &["doctor", "--detach", "--help"],
        &["-p", "", "--help"],
        &["--output", "json", "--help"],
        &["attach", "r1", "r2", "--help"],
    ] {
        refused(words);
    }
    assert_eq!(ok(&["-p", "x", "--help"]), help_on(Topic::Top));
    assert_eq!(ok(&["attach", "r1", "--help"]), help_on(Topic::Attach));
    // `help` takes no option, --version included (M1T06R-03); --help keeps --project for main to check (M1T06R-04).
    refused(&["help", "--version"]);
    refused(&["help", "attach", "--version"]);
    assert_eq!(
        ok(&["--project", "/nonexistent", "-p", "x", "--help"]),
        Invocation::Help {
            topic: Topic::Top,
            project: Some(PathBuf::from("/nonexistent"))
        }
    );
    refused(&["help", "plan"]);
    refused(&["help", "--live"]);
}

#[test]
fn a_prompt_is_one_non_empty_text() {
    assert_eq!(
        ok(&["-p", "做个X", "--output", "json"]),
        Invocation::Prompt {
            text: "做个X".to_owned(),
            output: Output::Json,
            detach: false,
            resume: None,
            project: None
        }
    );
    // The argument after -p is the text, whatever it looks like.
    assert!(
        matches!(ok(&["-p", "--version"]), Invocation::Prompt { text, .. } if text == "--version")
    );
    assert!(matches!(
        ok(&["--resume", "r1", "-p", "采用方案A", "--detach", "--output=stream-json", "--project", "/tmp"]),
        Invocation::Prompt { resume: Some(r), detach: true, output: Output::StreamJson, project: Some(_), .. } if r == "r1"
    ));
    for words in [
        &["-p"][..],
        &["-p", ""],
        &["-p", "  "],
        &["-p", "a", "-p", "b"],
        &["-p", "a", "b"],
    ] {
        refused(words);
    }
}

#[test]
fn subcommands_take_only_their_options() {
    assert_eq!(
        ok(&["attach", "r1", "--output", "stream-json"]),
        Invocation::Attach {
            run_id: Some("r1".to_owned()),
            output: Output::StreamJson,
            project: None
        }
    );
    assert!(
        matches!(ok(&["attach", "--resume", "r2"]), Invocation::Attach { run_id: Some(r), .. } if r == "r2")
    );
    assert!(matches!(
        ok(&["attach"]),
        Invocation::Attach { run_id: None, .. }
    ));
    assert_eq!(
        ok(&["web", "--project", "/srv/p"]),
        Invocation::Web {
            project: Some(PathBuf::from("/srv/p"))
        }
    );
    assert_eq!(
        ok(&["doctor", "--live", "--output", "json"]),
        Invocation::Doctor {
            live: true,
            output: Output::Json,
            project: None
        }
    );
    for words in [
        &["attach", "r1", "r2"][..],
        &["attach", "r1", "--resume", "r1"],
        &["attach", "--detach"],
        &["attach", "-p", "x"],
        &["web", "--output", "json"],
        &["web", "extra"],
        &["web", "--resume", "r1"],
        &["doctor", "--output", "stream-json"],
        &["doctor", "x"],
        &["--live"],
        &["--output", "json"],
        &["--output", "yaml", "-p", "x"],
        &["--output="],
        &["--project"],
        &["--project", "/a", "--project", "/b"],
        &["--detach", "--detach", "-p", "x"],
        &["-h"],
        &["--hel"],
        &["--help=yes"],
    ] {
        refused(words);
    }
}

#[test]
fn undefined_commands_and_positionals_are_usage_errors() {
    // AC-003-02's table (the /nonexistent project is refused by main, which checks the directory).
    for words in [
        &["plan"][..],
        &["mode", "plan"],
        &["run"],
        &["resume"],
        &["init"],
        &["做个X"],
        &["--resume", "r1"],
        &["--detach"],
    ] {
        let message = refused(words);
        if words[0] == "plan" || words[0] == "mode" {
            assert!(message.contains("/plan"), "{words:?}: {message}");
        }
    }
    assert!(matches!(
        ok(&["--project", "/nonexistent", "-p", "x"]),
        Invocation::Prompt { project: Some(p), .. } if p == std::path::Path::new("/nonexistent")
    ));
}

#[test]
fn arguments_that_are_not_utf8_are_refused() {
    use std::os::unix::ffi::OsStringExt;
    let args = vec![OsString::from("-p"), OsString::from_vec(vec![0xff, 0xfe])];
    assert!(parse(&args).is_err());
}
