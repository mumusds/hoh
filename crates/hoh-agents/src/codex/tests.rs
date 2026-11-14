//! The argv, verdict and headless rules of the Codex adapter.

use super::*;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/codex/0.166.0"
);

fn help(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).expect("recorded help")
}

fn launch() -> Launch {
    Launch {
        program: PathBuf::from("codex"),
        app_server_help: Some(help("help-app-server.txt")),
        exec_help: help("help-exec.txt"),
        exec_resume_help: None,
        cwd: PathBuf::from("/tmp"),
        yolo: true,
        resume: None,
        model: None,
        known: BTreeSet::new(),
        requests: BTreeSet::new(),
        client_notifications: BTreeSet::new(),
        capabilities: crate::capabilities::Probed::new(),
    }
}

#[test]
fn app_server_runs_on_stdio_when_the_help_offers_it() {
    assert_eq!(
        app_server_args(&help("help-app-server.txt")),
        Ok(["app-server", "--listen", "stdio://"]
            .map(str::to_owned)
            .to_vec())
    );
    assert_eq!(
        app_server_args("Usage: codex app-server\n"),
        Ok(vec!["app-server".to_owned()])
    );
    // A --listen that does not list stdio:// cannot start a stdio App Server, and the probe says so (M1T07R-27).
    for other in ["pipe://", "stdio:///socket", "stdio://[::1]", "stdio://.."] {
        let no_stdio = help("help-app-server.txt").replace("`stdio://`", other);
        assert!(app_server_args(&no_stdio).is_err(), "{other}");
        assert!(app_server_launch_missing(&no_stdio).is_some(), "{other}");
    }
    assert_eq!(
        app_server_launch_missing(&help("help-app-server.txt")),
        None
    );
}

#[test]
fn exec_args_come_from_the_help() {
    let mut l = launch();
    assert_eq!(
        exec_args(&l).expect("argv"),
        ["exec", "--json", YOLO_FLAG, "-"]
    );
    l.model = Some("gpt-x".to_owned());
    assert_eq!(
        exec_args(&l).expect("argv"),
        ["exec", "--json", YOLO_FLAG, "--model", "gpt-x", "-"]
    );
    // The YOLO profile needs its flag; without YOLO it is not added.
    l.exec_help = l.exec_help.replace(YOLO_FLAG, "");
    assert!(matches!(exec_args(&l), Err(Blocked(d)) if d.contains(YOLO_FLAG)));
    l.yolo = false;
    assert!(!exec_args(&l).expect("argv").iter().any(|a| a == YOLO_FLAG));
    // No --json, no exec.
    l.exec_help = "Usage: codex exec [PROMPT]\n".to_owned();
    assert!(exec_args(&l).is_err());
    // A resumed exec turn needs the probed `exec resume` help.
    let mut l = launch();
    l.resume = Some("thread-1".to_owned());
    assert!(exec_args(&l).is_err());
    l.exec_resume_help = Some(l.exec_help.clone());
    assert_eq!(
        exec_args(&l).expect("argv"),
        ["exec", "resume", "--json", YOLO_FLAG, "thread-1", "-"]
    );
}

#[test]
fn known_notifications_are_read_from_the_schema() {
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{FIXTURES}/app-server-schema/ServerNotification.json"
        ))
        .expect("schema"),
    )
    .expect("json");
    let known = known_notifications(&schema);
    for m in [
        "turn/completed",
        "item/started",
        "error",
        "deprecationNotice",
        "thread/goal/cleared",
    ] {
        assert!(known.contains(m), "{m}");
    }
    assert!(!known.contains("hoh/unrecorded"));
    assert_eq!(known.len(), 85);
}

#[test]
fn headless_answers_decline_or_refuse_never_choose() {
    assert_eq!(
        headless_answer("item/commandExecution/requestApproval"),
        Ok(json!({"decision": "decline"}))
    );
    assert_eq!(
        headless_answer("item/fileChange/requestApproval"),
        Ok(json!({"decision": "decline"}))
    );
    assert_eq!(
        headless_answer("execCommandApproval"),
        Ok(json!({"decision": "denied"}))
    );
    assert_eq!(
        headless_answer("applyPatchApproval"),
        Ok(json!({"decision": "denied"}))
    );
    assert_eq!(
        headless_answer("mcpServer/elicitation/request"),
        Ok(json!({"action": "decline"}))
    );
    for m in [
        "item/tool/requestUserInput",
        "item/permissions/requestApproval",
        "item/tool/call",
        "account/chatgptAuthTokens/refresh",
        "attestation/generate",
        "something/new",
    ] {
        assert!(headless_answer(m).is_err(), "{m}");
    }
}

#[test]
fn the_turn_status_decides_and_a_cancel_wins() {
    let failed = |why: &str| (AttemptState::Failed, Some(why.to_owned()));
    let done = Ending {
        status: Some("completed"),
        ..Ending::default()
    };
    assert_eq!(verdict(&done), (AttemptState::Settled, None));
    assert_eq!(
        verdict(&Ending {
            status: Some("interrupted"),
            ..done
        }),
        (AttemptState::Cancelled, None)
    );
    let unauthorized = json!({"message": "x", "codexErrorInfo": "unauthorized"});
    let busy = json!({"message": "x", "codexErrorInfo": "serverOverloaded"});
    let limited = json!({"message": "x", "codexErrorInfo": "rateLimitExceeded"});
    let other = json!({"message": "x", "codexErrorInfo": "contextWindowExceeded"});
    let exec_401 = json!({"message": "unexpected status 401 Unauthorized: ..."});
    let plain = json!({"message": "The model is not supported"});
    for (error, why) in [
        (&unauthorized, "BLOCKED_AUTH"),
        (&busy, "TRANSIENT_EXTERNAL"),
        (&limited, "TRANSIENT_EXTERNAL"),
        (&other, "contextWindowExceeded"),
        (&exec_401, "BLOCKED_AUTH"),
        (&plain, "turn failed"),
    ] {
        assert_eq!(
            verdict(&Ending {
                status: Some("failed"),
                error: Some(error),
                ..done
            }),
            failed(why),
            "{error}"
        );
    }
    // exec: an earlier `error` event with 401 marks the failure as authentication.
    assert_eq!(
        verdict(&Ending {
            status: Some("failed"),
            error: Some(&plain),
            auth_hint: true,
            ..done
        }),
        failed("BLOCKED_AUTH")
    );
    // No turn/completed: unknown effect once a tool item had started, else a protocol failure.
    let cut = Ending {
        status: None,
        ..done
    };
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
    // exec's exit code counts; the App Server's does not (stdin is closed once the turn is done).
    let exec_done = Ending {
        exit_matters: true,
        exit: Some(Termination::Code(1)),
        ..done
    };
    assert_eq!(verdict(&exec_done), failed("exit code 1"));
    assert_eq!(
        verdict(&Ending {
            exit_matters: false,
            ..exec_done
        }),
        (AttemptState::Settled, None)
    );
    // Order: cancel > refused frame > deadline > raw logs > protocol failure > the turn.
    let all = Ending {
        too_large: true,
        deadline: true,
        logs_incomplete: true,
        failure: Some("thread/start: no"),
        ..done
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
            ..all
        }),
        failed("RAW_LOG_INCOMPLETE")
    );
    assert_eq!(
        verdict(&Ending {
            too_large: false,
            deadline: false,
            logs_incomplete: false,
            ..all
        }),
        failed("thread/start: no")
    );
}

#[test]
fn a_sessions_capabilities_are_those_of_the_transport_a_turn_would_take() {
    // docs/adr/ADR-022.md decision 9: the binder records what a turn of this launch would have; with a schema that does
    // not serve a turn the App Server is not used and the session's capabilities are exec's.
    let attempt = AttemptRef {
        attempt_id: String::new(),
        session_id: String::new(),
        role: String::new(),
        agent_version: String::new(),
    };
    let turn = |launch: Launch| Turn {
        launch,
        prompt: String::new(),
        logs: PathBuf::from("/tmp"),
        escalation: Turn::default_escalation(),
        handshake_timeout: Duration::from_secs(1),
        deadline: None,
    };
    let exec_only = turn(launch());
    assert_eq!(
        session_capabilities(&exec_only),
        Ok(session(&attempt, &exec_only, false, None).capabilities)
    );
    let mut served = launch();
    served.requests = APP_SERVER_REQUESTS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    served.client_notifications = APP_SERVER_CLIENT_NOTIFICATIONS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    served.known = APP_SERVER_NOTIFICATIONS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    let served = turn(served);
    assert_eq!(
        session_capabilities(&served),
        Ok(session(&attempt, &served, true, None).capabilities)
    );
    let mut nothing = launch();
    nothing.app_server_help = None;
    nothing.exec_help = String::new();
    assert!(session_capabilities(&turn(nothing)).is_err());
}

#[test]
fn a_yolo_session_runs_commands_on_either_transport_where_the_probe_found_it() {
    // docs/adr/ADR-030.md decision 12: on the App Server and on exec the adapter runs commands under the YOLO profile
    // only, and the session has what the probe found (left out where it could not tell).
    let served = |mut l: Launch| {
        l.requests = APP_SERVER_REQUESTS
            .iter()
            .map(|m| (*m).to_owned())
            .collect();
        l.client_notifications = APP_SERVER_CLIENT_NOTIFICATIONS
            .iter()
            .map(|m| (*m).to_owned())
            .collect();
        l.known = APP_SERVER_NOTIFICATIONS
            .iter()
            .map(|m| (*m).to_owned())
            .collect();
        l
    };
    for app_server in [true, false] {
        for (probed, yolo, expected) in [
            (Some(true), true, Some(true)),
            (Some(false), true, Some(false)),
            (None, true, None),
            (Some(true), false, Some(false)),
        ] {
            let mut l = if app_server {
                served(launch())
            } else {
                launch()
            };
            l.capabilities
                .insert("command_execution".to_owned(), probed);
            l.capabilities
                .insert("partial_messages".to_owned(), Some(true));
            l.yolo = yolo;
            let turn = Turn {
                launch: l,
                prompt: String::new(),
                logs: PathBuf::from("/tmp"),
                escalation: Turn::default_escalation(),
                handshake_timeout: Duration::from_secs(1),
                deadline: None,
            };
            let caps = session_capabilities(&turn).expect("capabilities");
            // The transport is the one meant: only the App Server streams deltas.
            assert_eq!(caps["partial_messages"], json!(app_server), "{caps}");
            assert_eq!(
                caps.get("command_execution").and_then(Value::as_bool),
                expected,
                "app server {app_server}, probed {probed:?}, yolo {yolo}: {caps}"
            );
        }
    }
}

/// The recorded App Server case `name`: its server frames as recorded and its client lines (stdin.txt).
fn recorded_app_server(name: &str) -> (String, Vec<Value>) {
    let frames = std::fs::read_to_string(format!("{FIXTURES}/{name}/stdout.jsonl"))
        .expect("recorded frames");
    let client = std::fs::read_to_string(format!("{FIXTURES}/{name}/stdin.txt"))
        .expect("recorded client lines")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON client line"))
        .collect();
    (frames, client)
}

#[tokio::test]
async fn the_driver_writes_what_the_request_builders_give() {
    // docs/adr/ADR-030.md decision 10, "同一构建函数": the recorded `app-server-turn` replayed by the fake App Server; every
    // line the driver writes to its stdin equals, one by one, what `app_server::requests` gives for the same start
    // (YOLO, no model, a new thread), so what the compat suite checks is what the driver sends.
    use app_server::requests;
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().expect("temp dir");
    let (frames, client) = recorded_app_server("app-server-turn");
    let frames_file = dir.path().join("frames.jsonl");
    std::fs::write(&frames_file, &frames).expect("frames");
    let methods: BTreeMap<String, String> = client
        .iter()
        .filter_map(|m| {
            Some((
                m["id"].as_i64()?.to_string(),
                m["method"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let requests_file = dir.path().join("requests.json");
    std::fs::write(&requests_file, json!(methods).to_string()).expect("requests");
    let fake_logs = dir.path().join("fake");
    let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/app_server.py");
    let mut command = std::process::Command::new("python3");
    command
        .arg(fake)
        .arg(&frames_file)
        .arg(&requests_file)
        .arg(&fake_logs);
    let mut process =
        crate::process::Process::spawn(command, crate::process::Config::default()).expect("fake");
    let prompt = client[3]["params"]["input"][0]["text"]
        .as_str()
        .expect("the recorded prompt")
        .to_owned();
    let thread = client[3]["params"]["threadId"]
        .as_str()
        .expect("the recorded thread")
        .to_owned();
    let turn = Turn {
        launch: launch(),
        prompt: prompt.clone(),
        logs: dir.path().join("logs"),
        escalation: Turn::default_escalation(),
        handshake_timeout: Duration::from_secs(20),
        deadline: None,
    };
    let attempt = AttemptRef {
        attempt_id: "attempt-1".to_owned(),
        session_id: "session-1".to_owned(),
        role: "builder".to_owned(),
        agent_version: "codex-cli 0.166.0".to_owned(),
    };
    let notify = tokio::sync::Notify::new();
    let (_steer, mut steers) = mpsc::channel::<Steer>(1);
    let drove = app_server::drive(
        None,
        &attempt,
        &turn,
        &mut process,
        Stop::Native(&notify),
        &mut steers,
        &mut Vec::new(),
        None,
    )
    .await
    .expect("driven");
    let app_server::Drove::Ended(driven) = drove else {
        panic!("the recorded turn did not degrade");
    };
    assert_eq!(driven.state, AttemptState::Settled);
    let written: Vec<Value> = std::fs::read_to_string(fake_logs.join("received.jsonl"))
        .expect("what the driver wrote")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    let built = vec![
        requests::initialize(0),
        requests::initialized(),
        requests::thread_start(1, true, None),
        requests::turn_start(2, Some(&thread), &prompt),
    ];
    assert_eq!(written.len(), built.len(), "{written:#?}");
    for (n, (written, built)) in written.iter().zip(&built).enumerate() {
        assert_eq!(written, built, "line {n} the driver wrote");
    }
    // The same start as recorded: the thread and turn requests are the recorder's own; only initialize's clientInfo
    // names another client.
    assert_eq!(built[1..], client[1..]);
    assert_eq!(built[0]["method"], client[0]["method"]);
}
