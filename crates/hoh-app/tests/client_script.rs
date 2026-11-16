//! `hoh -p --resume` against a scripted host (docs/adr/ADR-031.md decisions 9, 10 and 16; test plan "客户端" and
//! "`--resume` 到 INCOMPLETE"): the client's side of SendInput and of the ResumeRun after it, whose host side (S2's
//! SendInput acceptance, S1d's ResumeRun from INCOMPLETE) is not built yet. A test host on the project's socket
//! answers getRun, the commands and the event stream from a script and records every command; these are mock tests of
//! the client's protocol, not of a host.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const HOH: &str = env!("CARGO_BIN_EXE_hoh");

/// How long one `hoh` command of these tests may take.
const LIMIT: Duration = Duration::from_secs(30);

/// The events of the script once the commands are known: (seq, the SSE data, raw).
type Events = Box<dyn Fn(&[Value]) -> Vec<(i64, String)> + Send>;

/// What the test host answers.
struct Script {
    /// getRun's successive bodies (the last repeats).
    runs: VecDeque<Value>,
    /// The successive answers to POST /v1/commands: (status, body); a 202 body gets the command's id and run.
    replies: VecDeque<(u16, Value)>,
    /// Every command posted, in order.
    commands: Vec<Value>,
    events: Events,
}

/// A test host serving `script` for the project at `project`, named by its `host.json`.
struct Fake {
    script: Arc<Mutex<Script>>,
    _dir: tempfile::TempDir,
}

impl Fake {
    fn start(project: &Path, script: Script) -> R<Fake> {
        // A short socket path (at most 107 bytes).
        let dir = tempfile::Builder::new().prefix("hohs").tempdir_in("/tmp")?;
        let socket = dir.path().join("s");
        let listener = UnixListener::bind(&socket)?;
        std::fs::create_dir_all(project.join(".hoh"))?;
        std::fs::write(project.join(".hoh/state.db"), b"")?;
        std::fs::write(
            project.join(".hoh/host.json"),
            json!({"pid": std::process::id(), "socket": socket, "token": "t", "port": 0, "host_epoch": 1})
                .to_string(),
        )?;
        let script = Arc::new(Mutex::new(script));
        let shared = script.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = shared.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, &shared);
                });
            }
        });
        Ok(Fake { script, _dir: dir })
    }

    fn commands(&self) -> Vec<Value> {
        self.script
            .lock()
            .map(|s| s.commands.clone())
            .unwrap_or_default()
    }
}

fn respond(stream: &mut UnixStream, status: u16, body: &Value) -> std::io::Result<()> {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn serve(mut stream: UnixStream, script: &Mutex<Script>) -> R {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
        if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = n.trim().parse()?;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let target = first.split(' ').nth(1).unwrap_or_default().to_owned();
    if target == "/healthz" {
        return Ok(respond(&mut stream, 200, &json!({"ok": true}))?);
    }
    if target.starts_with("/v1/runs/") {
        let run = {
            let mut s = script.lock().map_err(|_| "poisoned")?;
            if s.runs.len() > 1 {
                s.runs.pop_front()
            } else {
                s.runs.front().cloned()
            }
        };
        match run {
            Some(run) => respond(&mut stream, 200, &run)?,
            None => respond(
                &mut stream,
                404,
                &json!({"code": "RUN_NOT_FOUND", "message": "no run", "retryable": false, "current_revision": null}),
            )?,
        }
        return Ok(());
    }
    if target == "/v1/commands" {
        let command: Value = serde_json::from_slice(&body)?;
        let (status, mut reply) = {
            let mut s = script.lock().map_err(|_| "poisoned")?;
            s.commands.push(command.clone());
            s.replies
                .pop_front()
                .ok_or("a command the script has no answer for")?
        };
        if status == 202 {
            reply["command_id"] = command["command_id"].clone();
            reply["run_id"] = command["run_id"].clone();
            reply["status"] = json!("ACCEPTED");
        }
        return Ok(respond(&mut stream, status, &reply)?);
    }
    if let Some(query) = target.strip_prefix("/v1/events?") {
        let after: i64 = query
            .split('&')
            .find_map(|p| p.strip_prefix("after="))
            .unwrap_or("0")
            .parse()?;
        let events = {
            let s = script.lock().map_err(|_| "poisoned")?;
            (s.events)(&s.commands)
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n"
        )?;
        for (seq, data) in events.into_iter().filter(|(seq, _)| *seq > after) {
            let frame = format!("id: {seq}\ndata: {data}\n\n");
            write!(stream, "{:x}\r\n{frame}\r\n", frame.len())?;
        }
        // Open until the client goes: a keep-alive comment now and then.
        let end = Instant::now() + LIMIT;
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(200));
            let frame = ": keep-alive\n\n";
            if write!(stream, "{:x}\r\n{frame}\r\n", frame.len()).is_err() {
                break;
            }
        }
        return Ok(());
    }
    Ok(respond(&mut stream, 404, &json!({"code": "NOT_FOUND"}))?)
}

/// A protocol Event of run `run`.
fn ev(run: &str, seq: i64, kind: &str, payload: Value) -> (i64, String) {
    (
        seq,
        json!({"schema_version": "1.0", "project_id": "p", "seq": seq, "event_id": format!("e{seq}"), "run_id": run,
               "attempt_id": null, "at": "2026-11-15T00:00:00Z", "kind": kind, "payload": payload})
        .to_string(),
    )
}

/// A `run.result` of run `run` in `state` at its own seq.
fn result(
    run: &str,
    seq: i64,
    state: &str,
    blocked: Option<&str>,
    remaining: &[&str],
) -> (i64, String) {
    let exit = match state {
        "COMPLETED" => 0,
        "FAILED" => 1,
        "BLOCKED" => 3,
        "INCOMPLETE" => 4,
        "CANCELLED" => 5,
        _ => 6,
    };
    ev(
        run,
        seq,
        "run.result",
        json!({"run_id": run, "state": state, "exit_code": exit, "blocked_reason": blocked, "spec_revision": 1,
               "accepted": 2 - remaining.len(), "total": 2, "last_event_seq": seq, "cost_usd": null,
               "cost_status": "unknown", "remaining_requirements": remaining}),
    )
}

/// getRun's body for run `run`.
fn run(run: &str, state: &str, revision: i64, watermark: i64, projection: Value) -> Value {
    json!({"run_id": run, "state": state, "revision": revision, "watermark": watermark, "projection": projection})
}

/// The command `kind` among `commands`, the `n`-th of it.
fn nth<'a>(commands: &'a [Value], kind: &str, n: usize) -> Option<&'a Value> {
    commands.iter().filter(|c| c["kind"] == kind).nth(n)
}

/// The id of the `n`-th command `kind` posted (or one that matches none before it is posted).
fn id_of(commands: &[Value], kind: &str, n: usize) -> Value {
    nth(commands, kind, n).map_or(json!("not posted"), |c| c["command_id"].clone())
}

fn hoh(project: &Path, args: &[&str]) -> R<Output> {
    let child = Command::new(HOH)
        .args(args)
        .current_dir(project)
        .env_clear()
        .env("HOME", project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    finish(child)
}

fn finish(child: Child) -> R<Output> {
    let pid = child.id();
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(child.wait_with_output());
    });
    match got.recv_timeout(LIMIT) {
        Ok(out) => Ok(out?),
        Err(_) => {
            if let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
            Err(format!("hoh still running after {LIMIT:?}").into())
        }
    }
}

fn project() -> R<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::Builder::new().prefix("hohc").tempdir_in("/tmp")?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Ok((dir, project))
}

/// The history of a run before the client's commands: seqs 1..=10, one an older result.
fn history(name: &str, state: &str) -> Vec<(i64, String)> {
    let mut events: Vec<(i64, String)> = (1..=9)
        .map(|seq| ev(name, seq, "run.noted", json!({"n": seq})))
        .collect();
    events.push(result(name, 10, state, None, &["REQ-2"]));
    events
}

#[test]
fn an_incomplete_run_takes_the_input_and_a_resume_refused_by_its_stop_ends_with_4() -> R {
    // Decision 9: SendInput at the revision read (stale once: the run is read again), then ResumeRun at the revision
    // of SendInput's 202, refused because its stop still holds; the follow ends at the result after the input's
    // acceptance, with 4, and says where the input went, the stop, the requirements left and "新建 Run".
    let (_dir, project) = project()?;
    let fake = Fake::start(
        &project,
        Script {
            runs: [
                run("I1", "INCOMPLETE", 3, 10, json!({})),
                run("I1", "INCOMPLETE", 4, 10, json!({})),
            ]
            .into(),
            replies: [
                (409, json!({"code": "STALE_REVISION", "message": "stale", "retryable": true, "current_revision": 4})),
                (202, json!({"revision": 5})),
                (409, json!({"code": "ILLEGAL_TRANSITION", "message": "Run 的预算已耗尽", "retryable": false,
                             "current_revision": 5, "details": {"state": "INCOMPLETE", "stop": "iterations"}})),
            ]
            .into(),
            commands: Vec::new(),
            events: Box::new(|commands| {
                let mut events = history("I1", "INCOMPLETE");
                events.push(ev(
                    "I1",
                    11,
                    "input.accepted",
                    json!({"command_id": id_of(commands, "SendInput", 1), "question_id": "q1"}),
                ));
                events.push(result("I1", 12, "INCOMPLETE", None, &["REQ-2"]));
                events
            }),
        },
    )?;
    let out = hoh(&project, &["-p", "采用方案A", "--resume", "I1"])?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(4), "{stderr}");
    assert_eq!(String::from_utf8(out.stdout)?, "I1 INCOMPLETE 剩余 REQ-2\n");
    for said in ["已答复问题 q1", "iterations", "REQ-2", "新建 Run"] {
        assert!(stderr.contains(said), "{said}: {stderr}");
    }
    let commands = fake.commands();
    let kinds: Vec<(&str, i64)> = commands
        .iter()
        .map(|c| {
            (
                c["kind"].as_str().unwrap_or_default(),
                c["expected_revision"].as_i64().unwrap_or(-1),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [("SendInput", 3), ("SendInput", 4), ("ResumeRun", 5)]
    );
    for input in commands.iter().filter(|c| c["kind"] == "SendInput") {
        assert_eq!(
            (&input["run_id"], &input["payload"]),
            (
                &json!("I1"),
                &json!({"text": "采用方案A", "target_role": null, "input_kind": "REPLY"})
            )
        );
    }
    Ok(())
}

#[test]
fn a_resumed_incomplete_run_is_followed_past_the_inputs_result_to_the_one_after_the_resume() -> R {
    // E7: the result the input's own commit wrote (12) is before the ResumeRun's acceptance (14) and ends nothing; the
    // result after it (15) does. stream-json shows every event from the first; those before the input's acceptance
    // replay history (CR-022), every line an Event.
    let (_dir, project) = project()?;
    let fake = Fake::start(
        &project,
        Script {
            runs: [run("I2", "INCOMPLETE", 3, 10, json!({}))].into(),
            replies: [(202, json!({"revision": 4})), (202, json!({"revision": 5}))].into(),
            commands: Vec::new(),
            events: Box::new(|commands| {
                let mut events = history("I2", "INCOMPLETE");
                events.extend([
                    ev(
                        "I2",
                        11,
                        "input.accepted",
                        json!({"command_id": id_of(commands, "SendInput", 0), "question_id": null}),
                    ),
                    result("I2", 12, "INCOMPLETE", None, &["REQ-2"]),
                    ev("I2", 13, "reconcile.run", json!({"from": "INCOMPLETE"})),
                    ev(
                        "I2",
                        14,
                        "run.resumed",
                        json!({"command_id": id_of(commands, "ResumeRun", 0)}),
                    ),
                    result("I2", 15, "INCOMPLETE", None, &["REQ-2"]),
                    ev("I2", 16, "run.after", json!({})),
                ]);
                events
            }),
        },
    )?;
    let out = hoh(
        &project,
        &["-p", "继续", "--resume", "I2", "--output", "stream-json"],
    )?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(4), "{stderr}");
    assert!(stderr.contains("输入已排队"), "{stderr}");
    let stdout = String::from_utf8(out.stdout)?;
    let mut seqs = Vec::new();
    for line in stdout.split_terminator('\n') {
        let event: Value = serde_json::from_str(line)?;
        hoh_core::schema::protocol()
            .validate_def("Event", &event)?
            .map_err(|v| format!("{v:?}: {line}"))?;
        let seq = event["seq"].as_i64().ok_or("no seq")?;
        assert_eq!(
            event.get("replay"),
            (seq < 11).then_some(&json!(true)),
            "{line}"
        );
        seqs.push(seq);
    }
    assert_eq!(seqs, (1..=15).collect::<Vec<_>>());
    let kinds: Vec<(String, i64)> = fake
        .commands()
        .iter()
        .map(|c| {
            (
                c["kind"].as_str().unwrap_or_default().to_owned(),
                c["expected_revision"].as_i64().unwrap_or(-1),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [("SendInput".to_owned(), 3), ("ResumeRun".to_owned(), 4)]
    );
    Ok(())
}

#[test]
fn a_detached_resume_prints_the_inputs_acceptance_and_says_the_resume_was_refused() -> R {
    // M2T13D-154: 0 (the input was accepted), stdout the SendInput's 202 body, stderr the refusal and its stop.
    let (_dir, project) = project()?;
    let fake = Fake::start(
        &project,
        Script {
            runs: [run("I1", "INCOMPLETE", 3, 10, json!({}))].into(),
            replies: [
                (202, json!({"revision": 4})),
                (409, json!({"code": "ILLEGAL_TRANSITION", "message": "stopped", "retryable": false,
                             "current_revision": 4, "details": {"state": "INCOMPLETE", "stop": "iterations"}})),
            ]
            .into(),
            commands: Vec::new(),
            events: Box::new(|_| Vec::new()),
        },
    )?;
    let out = hoh(
        &project,
        &["-p", "x", "--resume", "I1", "--detach", "--output", "json"],
    )?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let printed: Value = serde_json::from_slice(&out.stdout)?;
    let commands = fake.commands();
    let input = nth(&commands, "SendInput", 0).ok_or("no SendInput")?;
    assert_eq!(
        printed,
        json!({"revision": 4, "command_id": input["command_id"], "run_id": "I1", "status": "ACCEPTED"})
    );
    assert!(out.stdout.ends_with(b"}\n"));
    for said in ["ResumeRun", "iterations", "新建 Run"] {
        assert!(stderr.contains(said), "{said}: {stderr}");
    }
    Ok(())
}

#[test]
fn a_refused_input_ends_by_the_refusals_details_never_its_message() -> R {
    // M2T13D-54: the run ended between the read and the input (details.state CANCELLED) is 64; the same message with
    // an answer's precondition in its details is 1. Both say the code; json mode prints nothing.
    for (details, code) in [
        (json!({"state": "CANCELLED"}), 64),
        (
            json!({"question_id": "q1", "precondition": "handoff", "detail": "x"}),
            1,
        ),
    ] {
        let (_dir, project) = project()?;
        let _fake = Fake::start(
            &project,
            Script {
                runs: [run("K", "BLOCKED", 3, 10, json!({}))].into(),
                replies: [(409, json!({"code": "ILLEGAL_TRANSITION", "message": "Run 已结束", "retryable": false,
                                       "current_revision": 3, "details": details}))]
                .into(),
                commands: Vec::new(),
                events: Box::new(|_| Vec::new()),
            },
        )?;
        let out = hoh(&project, &["-p", "x", "--resume", "K", "--output", "json"])?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(code), "{details}: {stderr}");
        assert!(out.stdout.is_empty());
        assert!(stderr.contains("ILLEGAL_TRANSITION"), "{stderr}");
    }
    Ok(())
}

#[test]
fn a_run_blocked_on_a_decision_lists_its_questions_and_how_to_answer() -> R {
    // Decision 9: exit 3 on NEED_DECISION lists getRun's projection.questions; a host that gives none is said so.
    for questions in [
        Some(
            json!([{"question_id": "q1", "text": "选哪个方案？", "choices": [{"choice_id": "a", "label": "方案A"}]}]),
        ),
        None,
    ] {
        let (_dir, project) = project()?;
        let blocked = run(
            "K",
            "BLOCKED",
            4,
            12,
            questions
                .clone()
                .map_or(json!({}), |q| json!({"questions": q})),
        );
        let _fake = Fake::start(
            &project,
            Script {
                runs: [run("K", "BLOCKED", 3, 10, json!({})), blocked].into(),
                replies: [(202, json!({"revision": 4}))].into(),
                commands: Vec::new(),
                events: Box::new(|commands| {
                    vec![
                        ev(
                            "K",
                            11,
                            "input.accepted",
                            json!({"command_id": id_of(commands, "SendInput", 0), "question_id": null}),
                        ),
                        result("K", 12, "BLOCKED", Some("NEED_DECISION"), &[]),
                    ]
                }),
            },
        )?;
        let out = hoh(&project, &["-p", "x", "--resume", "K", "--output", "json"])?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{stderr}");
        let printed: Value = serde_json::from_slice(&out.stdout)?;
        assert_eq!(
            (&printed["state"], &printed["blocked_reason"]),
            (&json!("BLOCKED"), &json!("NEED_DECISION"))
        );
        let said: &[&str] = if questions.is_some() {
            &["q1", "选哪个方案？", "a：方案A"]
        } else {
            &["projection.questions"]
        };
        for s in said.iter().chain(&["hoh -p '<答复>' --resume K"]) {
            assert!(stderr.contains(s), "{s}: {stderr}");
        }
    }
    Ok(())
}

#[test]
fn an_event_that_cannot_be_read_ends_the_follow_with_69_and_its_id() -> R {
    // Decision 10: never a silent gap. A WAITING_EXTERNAL notice is said on the way.
    let (_dir, project) = project()?;
    let _fake = Fake::start(
        &project,
        Script {
            runs: [run("K", "BLOCKED", 3, 10, json!({}))].into(),
            replies: [(202, json!({"revision": 4}))].into(),
            commands: Vec::new(),
            events: Box::new(|commands| {
                vec![
                    ev(
                        "K",
                        11,
                        "input.accepted",
                        json!({"command_id": id_of(commands, "SendInput", 0), "question_id": null}),
                    ),
                    ev(
                        "K",
                        12,
                        "run.waiting",
                        json!({"next_retry_at": "2026-11-15T01:02:03Z", "waiting": ["t"]}),
                    ),
                    (13, "{\"seq\": 13, broken".to_owned()),
                    result("K", 14, "BLOCKED", Some("NEED_DECISION"), &[]),
                ]
            }),
        },
    )?;
    let out = hoh(&project, &["-p", "x", "--resume", "K", "--output", "json"])?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(69), "{stderr}");
    assert!(out.stdout.is_empty());
    assert!(stderr.contains("事件 13"), "{stderr}");
    assert!(
        stderr.contains("等待外部恢复，下次重试 2026-11-15T01:02:03Z"),
        "{stderr}"
    );
    Ok(())
}
