//! The foreground `hoh -p` through the product binary against the world's in-process host (docs/adr/ADR-031.md
//! decisions 9 and 10, slice S3's supporting tests; test plan "客户端", "配置", "`--resume` 到 … FAILED、CANCELLED"):
//! the configuration before any host, the configured mode, the follow to the run's result in each output, signals that
//! only detach, a closed stdout, and `--resume` to a run that is not there or has ended. The plain host has no runner:
//! its runs move only by the commands the tests submit (a pause or a cancel lands at once with no attempt).
//!
//! TEST-002-01 and TEST-002-03 (AC-002-01, -03; M2-T13) run against an in-process host whose runner is the world's
//! scripted Team: its Commander asks a person in the specification phase and ends the run INCOMPLETE once answered.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;
mod pty;
mod world;

use hoh_app::host::Host;
use pty::R;
use serde_json::{Value, json};
use std::io::{BufRead as _, Read as _, Write as _};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};
use world::World;

/// How long a test waits for the product to get where it looks.
const WAIT: Duration = Duration::from_secs(30);

/// POST `body` (with fresh ids) to the commands of the project's host; its status and body.
fn post(project: &Path, mut body: Value) -> R<(u16, Value)> {
    let id = format!(
        "t-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    body["schema_version"] = json!("1.0");
    body["command_id"] = json!(id);
    body["idempotency_key"] = json!(id);
    let body = body.to_string();
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".hoh/host.json"))?)?;
    let mut stream =
        std::os::unix::net::UnixStream::connect(info["socket"].as_str().ok_or("no socket")?)?;
    stream.set_read_timeout(Some(WAIT))?;
    write!(
        stream,
        "POST /v1/commands HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nX-HoH-Token: {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        info["token"].as_str().ok_or("no token")?,
        body.len()
    )?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    let (head, body) = reply.split_once("\r\n\r\n").ok_or("no response head")?;
    let status: u16 = head.split(' ').nth(1).ok_or("no status")?.parse()?;
    Ok((status, serde_json::from_str(body).unwrap_or(Value::Null)))
}

/// The project's approved specification, which a new run adopts (docs/adr/ADR-031.md decision 1: with none, a new run
/// is BLOCKED on runtime:no_spec and `-p` ends 3 at once).
fn approved(world: &World) -> R {
    world.exec(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);",
    )
}

/// `kind` (PauseRun or CancelRun) for run `run` at its revision now, again at the revision the host names while it
/// answers STALE_REVISION (the world's runner moves the run on meanwhile; [`common::fresh_blocking`]); accepted.
fn control(world: &World, run: &str, kind: &str) -> R {
    let revision: i64 = world
        .query(&format!("SELECT revision FROM runs WHERE run_id = '{run}'"))?
        .parse()?;
    let (status, body) = common::fresh_blocking(json!(revision), |revision| {
        post(
            &world.project(),
            json!({"kind": kind, "run_id": run, "expected_revision": revision, "payload": {"reason": "test"}}),
        )
    })?;
    if status != 202 {
        return Err(format!("{kind}: {status} {body}").into());
    }
    Ok(())
}

/// The run StartRun `goal` made, once there is one.
fn run_of_goal(world: &World, goal: &str) -> R<String> {
    let sql = format!(
        "SELECT run_id FROM commands WHERE kind = 'StartRun' AND json_extract(payload_json, '$.goal') = '{goal}'"
    );
    let end = Instant::now() + WAIT;
    loop {
        let run = world.query(&sql)?;
        if !run.is_empty() {
            return Ok(run);
        }
        if Instant::now() > end {
            return Err(format!("no run for {goal}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `hoh args…` of the world, running: stdin a pipe kept open (never read by `-p`), stdout and stderr piped.
fn spawn(world: &World, args: &[&str]) -> R<Child> {
    Ok(world
        .command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?)
}

/// The run's events in the store, as (seq, kind).
fn events_of(world: &World, run: &str) -> R<Vec<(i64, String)>> {
    Ok(world
        .rows(&format!(
            "SELECT seq, kind FROM events WHERE run_id = '{run}' ORDER BY seq"
        ))?
        .iter()
        .map(|r| {
            (
                r["seq"].as_i64().unwrap_or(-1),
                r["kind"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect())
}

/// The payload of the run's latest `run.result`.
fn result_of(world: &World, run: &str) -> R<Value> {
    Ok(serde_json::from_str(&world.query(&format!(
        "SELECT payload_json FROM events WHERE run_id = '{run}' AND kind = 'run.result' ORDER BY seq DESC LIMIT 1"
    ))?)?)
}

/// Send `signal` to `pid`.
fn signal(pid: u32, signal: rustix::process::Signal) -> R {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or("no pid")?;
    rustix::process::kill_process(pid, signal)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_follows_its_new_run_to_the_result_and_prints_it_as_asked() -> R {
    // Decisions 9 and 10: StartRun{goal, the configured mode, not interactive}; the follow from the first event to
    // the run.result after its acceptance; stdout as asked (every Event / the RunResult / one line), stdin never read.
    let world = World::new()?;
    world.configure(&world::TEAM, "[project]\ndefault_mode = \"PLAN\"\n")?;
    let host = world.start_plain_host(None).await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        // The project has no approved specification yet (docs/adr/ADR-031.md decision 1): the new run is BLOCKED on its
        // question runtime:no_spec in StartRun's own commit, so the prompt ends 3 with that result.
        let goal = "没有规格";
        let out = world::finish(
            spawn(&world, &["-p", goal, "--output", "json"])?,
            world::LIMIT,
        )?;
        assert_eq!(
            out.status.code(),
            Some(3),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run = run_of_goal(&world, goal)?;
        let result = result_of(&world, &run)?;
        assert_eq!(
            (&result["state"], &result["blocked_reason"]),
            (&json!("BLOCKED"), &json!("NEED_DECISION"))
        );
        assert_eq!(serde_json::from_slice::<Value>(&out.stdout)?, result);
        assert_eq!(
            world.query(&format!(
                "SELECT json_extract(payload_json, '$.reason') FROM events WHERE run_id = '{run}' \
                 AND kind = 'question.asked'"
            ))?,
            "no_spec"
        );
        // Once it has one, a new run adopts it and works on.
        approved(&world)?;
        for (output, end, code, state) in [
            ("stream-json", "PauseRun", 6, "PAUSED"),
            ("json", "CancelRun", 5, "CANCELLED"),
            ("text", "PauseRun", 6, "PAUSED"),
        ] {
            let goal = format!("目标 {output}");
            let mut child = spawn(&world, &["-p", &goal, "--output", output])?;
            // Bytes on stdin go nowhere (REQ-002 behaviour 2).
            child.stdin.as_mut().ok_or("no stdin")?.write_all(b"y\n")?;
            let run = run_of_goal(&world, &goal)?;
            // Followed from its acceptance: the end comes after it.
            std::thread::sleep(Duration::from_millis(300));
            control(&world, &run, end)?;
            let out = world::finish(child, world::LIMIT)?;
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(code), "{output}: {stderr}");
            let result = result_of(&world, &run)?;
            assert_eq!(result["state"], state);
            let stdout = String::from_utf8(out.stdout)?;
            assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
            match output {
                "stream-json" => {
                    let mut lines = Vec::new();
                    for line in stdout.split_terminator('\n') {
                        let event: Value = serde_json::from_str(line)?;
                        hoh_core::schema::protocol()
                            .validate_def("Event", &event)?
                            .map_err(|v| format!("{v:?}: {line}"))?;
                        assert!(
                            event.get("replay").is_none(),
                            "a new run replays nothing: {line}"
                        );
                        lines.push(event);
                    }
                    assert!(stdout.ends_with('\n'));
                    let shown: Vec<(i64, String)> = lines
                        .iter()
                        .map(|e| {
                            (
                                e["seq"].as_i64().unwrap_or(-1),
                                e["kind"].as_str().unwrap_or_default().to_owned(),
                            )
                        })
                        .collect();
                    assert!(shown.windows(2).all(|w| w[0].0 < w[1].0), "{shown:?}");
                    assert_eq!(shown, events_of(&world, &run)?, "every event of the run");
                    let last = lines.last().ok_or("no line")?;
                    hoh_core::schema::protocol()
                        .validate_def("RunResultEvent", last)?
                        .map_err(|v| format!("{v:?}: {last}"))?;
                    assert_eq!(
                        (&last["kind"], &last["payload"]),
                        (&json!("run.result"), &result)
                    );
                }
                "json" => {
                    assert_eq!(stdout.matches('\n').count(), 1);
                    assert!(stdout.ends_with("}\n"), "{stdout:?}");
                    assert_eq!(serde_json::from_str::<Value>(&stdout)?, result);
                }
                _ => {
                    assert_eq!(stdout, format!("{run} {state}\n"));
                    let created = events_of(&world, &run)?
                        .into_iter()
                        .find(|(_, k)| k == "run.created")
                        .ok_or("no run.created")?;
                    assert!(
                        stderr.contains(&format!("#{} run.created", created.0)),
                        "{stderr}"
                    );
                }
            }
            let started = world.rows(&format!(
                "SELECT json(payload_json) AS p FROM commands WHERE run_id = '{run}' AND kind = 'StartRun'"
            ))?;
            let [started] = started.as_slice() else {
                return Err(format!("not one StartRun: {started:?}").into());
            };
            let payload: Value = serde_json::from_str(started["p"].as_str().ok_or("no payload")?)?;
            assert_eq!(
                payload,
                json!({"goal": goal, "mode": "PLAN", "interactive": false})
            );
        }
        assert_eq!(
            world.query("SELECT count(*) FROM commands WHERE payload_json LIKE '%\"y%'")?,
            "0",
            "stdin reached no command"
        );
        // A detached run takes the configured mode too.
        let out = world.hoh(&["-p", "分离", "--detach", "--output", "json"])?;
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let accepted: Value = serde_json::from_slice(&out.stdout)?;
        let run = accepted["run_id"].as_str().ok_or("no run_id")?;
        assert_eq!(
            world.query(&format!(
                "SELECT json_extract(payload_json, '$.mode') FROM commands WHERE run_id = '{run}'"
            ))?,
            "PLAN"
        );
        assert!(world.hosts()?.is_empty(), "the in-process host was used");
        Ok(())
    });
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signal_only_detaches_a_prompt_with_its_code_and_the_way_back() -> R {
    // REQ-004 failure mode 2, E8: 130/143/129, stderr names the run and `hoh attach <run>`; no command is sent and the
    // run does not move.
    let world = World::new()?;
    let host = world.start_plain_host(None).await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        approved(&world)?;
        for (sig, code) in [
            (rustix::process::Signal::INT, 130),
            (rustix::process::Signal::TERM, 143),
            (rustix::process::Signal::HUP, 129),
        ] {
            let goal = format!("信号 {code}");
            let mut child = spawn(&world, &["-p", &goal, "--output", "stream-json"])?;
            // Once it shows the run's first event, it follows.
            let mut stdout = std::io::BufReader::new(child.stdout.take().ok_or("no stdout")?);
            let mut first = String::new();
            stdout.read_line(&mut first)?;
            let run = run_of_goal(&world, &goal)?;
            assert!(first.contains(&run), "{first}");
            signal(child.id(), sig)?;
            let out = world::finish(child, world::LIMIT)?;
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(code), "{stderr}");
            assert!(stderr.contains(&format!("hoh attach {run}")), "{stderr}");
            assert_eq!(
                world.query(&format!(
                    "SELECT state || ' ' || (SELECT group_concat(kind) FROM commands WHERE run_id = '{run}') \
                     FROM runs WHERE run_id = '{run}'"
                ))?,
                "CREATED StartRun"
            );
        }
        Ok(())
    });
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_stdout_ends_a_prompt_with_74_and_no_panic() -> R {
    // REQ-004 failure mode 6: the reader went; the client stops following, says so once and exits 74; the run is
    // as it was and nothing was sent.
    let world = World::new()?;
    let host = world.start_plain_host(None).await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        approved(&world)?;
        let mut child = spawn(&world, &["-p", "管道", "--output", "stream-json"])?;
        drop(child.stdout.take());
        let run = run_of_goal(&world, "管道")?;
        let out = world::finish(child, world::LIMIT)?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(74), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert_eq!(
            world.query(&format!(
                "SELECT state || ' ' || (SELECT group_concat(kind) FROM commands WHERE run_id = '{run}') \
                 FROM runs WHERE run_id = '{run}'"
            ))?,
            "CREATED StartRun"
        );
        Ok(())
    });
    host.shutdown().await;
    outcome
}

#[test]
fn an_invalid_configuration_ends_a_prompt_with_78_before_any_host() -> R {
    // REQ-001 failure mode 4, REQ-004 failure mode 4: the file and its place on stderr, stdout empty, no host, no
    // database.
    let world = World::new()?;
    world.configure(&world::TEAM, "[project]\ndefault_mode = \"auto\"\n")?;
    for args in [
        &["-p", "x"][..],
        &["-p", "x", "--output", "json"],
        &["-p", "x", "--resume", "R1"],
        &["-p", "x", "--resume", "R1", "--output", "json"],
        &["-p", "x", "--detach", "--output", "json"],
    ] {
        let out = world.hoh(args)?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(78), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(
            stderr.contains("config.toml") && stderr.contains("project.default_mode"),
            "{args:?}: {stderr}"
        );
        assert!(world.hosts()?.is_empty(), "{args:?} started a host");
        assert!(!world.db().exists(), "{args:?}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_to_no_run_or_an_ended_one_is_64_and_submits_nothing() -> R {
    // REQ-002 failure modes 2 and 3, decision 9: decided from the run's state (or its absence), before any command;
    // FAILED and CANCELLED as COMPLETED. With no database nothing is started.
    let world = World::new()?;
    let out = world.hoh(&["-p", "x", "--resume", "R1"])?;
    assert_eq!(out.status.code(), Some(64));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("RUN_NOT_FOUND"));
    assert!(!world.db().exists() && world.hosts()?.is_empty());
    let host = world.start_plain_host(None).await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        world.exec(
            "INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES
               ('C', :p, 'COMPLETED', 'AUTO', 5, '2026-11-15T00:00:00Z'),
               ('F', :p, 'FAILED', 'AUTO', 2, '2026-11-15T00:00:01Z'),
               ('X', :p, 'CANCELLED', 'AUTO', 3, '2026-11-15T00:00:02Z');",
        )?;
        let before = || {
            world.query(
                "SELECT (SELECT group_concat(run_id || ':' || state || ':' || revision, ' ') FROM runs) || ' / ' || \
                 (SELECT count(*) FROM commands)",
            )
        };
        let was = before()?;
        for run in ["NOPE", "C", "F", "X"] {
            for more in [
                &[][..],
                &["--output", "json"],
                &["--detach", "--output", "json"],
            ] {
                let mut args = vec!["-p", "x", "--resume", run];
                args.extend_from_slice(more);
                let out = world.hoh(&args)?;
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert_eq!(out.status.code(), Some(64), "{args:?}: {stderr}");
                assert!(out.stdout.is_empty(), "{args:?}");
                let says = if run == "NOPE" {
                    "RUN_NOT_FOUND"
                } else {
                    "已结束"
                };
                assert!(stderr.contains(says), "{args:?}: {stderr}");
            }
        }
        assert_eq!(before()?, was, "no run, state, revision or command changed");
        Ok(())
    });
    host.shutdown().await;
    outcome
}

/// The question the scripted Commander of TEST-002-01 and TEST-002-03 asks.
const QUESTION: &str = "登录要支持哪些方式？\"OAuth\" 还是 密码\\短信";

/// The scripted Commander of TEST-002-01 and TEST-002-03: a run's turn at CREATED moves it to SPECIFYING and asks
/// [`QUESTION`] (requires_user); a turn given the answer (a `need_decision{answered}` among its events) decides
/// INCOMPLETE (SPECIFYING -> INCOMPLETE, the run quiet); every other turn submits nothing.
fn asking_commander() -> std::sync::Arc<world::Team> {
    world::Team::new(|turn| {
        let answered = turn.summary["events"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|e| e["kind"] == "need_decision" && e["payload"]["reason"] == "answered")
        });
        match turn.state.as_str() {
            "CREATED" => {
                let mut decided = world::steps("CREATED", "SPECIFYING");
                decided.push(world::ask("q-login", QUESTION));
                decided
            }
            "SPECIFYING" if answered => vec![world::transition("INCOMPLETE")],
            _ => Vec::new(),
        }
    })
}

/// The rows of `table`.
fn count(world: &World, table: &str) -> R<i64> {
    Ok(world
        .query(&format!("SELECT count(*) FROM {table}"))?
        .parse()?)
}

/// The commands accepted after the first `before`, in order: kind, run_id and payload.
fn commands_after(world: &World, before: i64) -> R<Vec<Value>> {
    world.rows(&format!(
        "SELECT kind, run_id, payload_json FROM commands ORDER BY rowid LIMIT -1 OFFSET {before}"
    ))
}

/// The alternate screen's start, which a `-p` never writes.
const ENTER_ALT: &[u8] = b"\x1b[?1049h";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_002_01_a_question_in_the_spec_phase_blocks_the_run_and_exits_3() -> R {
    // AC-002-01: a mock Git project with an approved specification, its host in this process, its runner the scripted
    // Team: the run's Commander moves it to SPECIFYING and asks a person (requires_user). With no terminal and stdin
    // /dev/null, `hoh -p '实现登录功能' --output json`, within 60 s.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    let team = asking_commander();
    let host = world.team_host(team.clone(), None).await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        let spec = world.seed_spec()?;
        let goal = "实现登录功能";
        let (runs, commands) = (count(&world, "runs")?, count(&world, "commands")?);
        let child = world
            .command(&["-p", goal, "--output", "json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // The new run adopted the project's approved specification in StartRun's commit (decision 1); it is staffed
        // here: the scripted Team has no agent registry, so its host leaves the bindings vacant.
        let run = run_of_goal(&world, goal)?;
        world.staff(&run)?;
        assert_eq!(
            world.query(&format!("SELECT spec_id FROM runs WHERE run_id = '{run}'"))?,
            spec
        );
        let out = world::finish(child, Duration::from_secs(60))?;
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        // Exit 3.
        assert_eq!(out.status.code(), Some(3), "{stderr}");
        // stdout is one JSON object as a whole: BLOCKED, NEED_DECISION, the run's id.
        let result: Value = serde_json::from_slice(&out.stdout)?;
        assert!(result.is_object(), "{result}");
        assert_eq!(result["state"], "BLOCKED", "{result}");
        assert_eq!(result["blocked_reason"], "NEED_DECISION", "{result}");
        assert_eq!(result["run_id"], json!(run), "{result}");
        // Exactly one run more, BLOCKED.
        assert_eq!(count(&world, "runs")?, runs + 1);
        assert_eq!(
            world.query(&format!("SELECT state FROM runs WHERE run_id = '{run}'"))?,
            "BLOCKED"
        );
        // Exactly one command more, StartRun: its goal byte for byte, not interactive; no other command of the client.
        let sent = commands_after(&world, commands)?;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["kind"], "StartRun", "{sent:?}");
        assert_eq!(sent[0]["run_id"], json!(run), "{sent:?}");
        let payload: Value =
            serde_json::from_str(sent[0]["payload_json"].as_str().ok_or("no payload")?)?;
        assert_eq!(
            payload["goal"].as_str().map(str::as_bytes),
            Some(goal.as_bytes()),
            "{payload}"
        );
        assert_eq!(payload["interactive"], false, "{payload}");
        // The run's events hold the question as the Commander asked it.
        let asked: Vec<Value> = world
            .rows(&format!(
                "SELECT payload_json FROM events WHERE run_id = '{run}' AND kind = 'question.asked'"
            ))?
            .iter()
            .map(|r| serde_json::from_str(r["payload_json"].as_str().unwrap_or("null")))
            .collect::<Result<_, _>>()?;
        assert_eq!(
            asked
                .iter()
                .map(|a| a["question"].clone())
                .collect::<Vec<_>>(),
            [json!(QUESTION)],
            "{asked:?}"
        );
        // Neither stdout nor stderr enters the alternate screen.
        assert_eq!(pty::find(&out.stdout, ENTER_ALT, 0), None);
        assert_eq!(pty::find(&out.stderr, ENTER_ALT, 0), None, "{stderr}");
        Ok(())
    });
    host.shutdown().await;
    outcome?;
    assert!(team.refused().is_empty(), "{team:?}");
    Ok(())
}

/// TEST-002-03's run C, COMPLETED at revision 5 (its id as a host gives one, a UUID).
const RUN_C: &str = "c5e2a7d1-0b3f-4e8a-9c6d-2f1e4b7a9d03";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_002_03_usage_errors_and_a_reply_by_resume() -> R {
    // AC-002-03: run C COMPLETED at revision 5 and run K BLOCKED (NEED_DECISION) on its Commander's question; their
    // host in this process, its runner the scripted Team, whose Commander decides INCOMPLETE once given the answer.
    // K is made through the product (StartRun, which adopts the approved specification, its staffing, its Commander's
    // turn to SPECIFYING and its request): the rows the test plan seeds by SQL (its specification, its Commander's
    // binding and session, the PROPOSED request_user_decision, the claim `commander:<id>`, question.asked,
    // resume_state SPECIFYING, no task, no attempt unfinished), written by their own writers. C is seeded by SQL.
    // Then, one after the other: `hoh -p`, `-p ''`, `-p '  '`, `-p x --resume NOPE`, `-p x --resume <C>`,
    // `-p '采用方案A' --resume <K>`.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    let team = asking_commander();
    let host = world.team_host(team.clone(), None).await?;
    let outcome = async {
        let spec = world.seed_spec()?;
        let k = world::start_run(&host, "k", "实现登录功能").await?;
        world.staff(&k)?;
        assert_eq!(
            world.query(&format!("SELECT spec_id FROM runs WHERE run_id = '{k}'"))?,
            spec
        );
        world
            .until(
                &format!("SELECT state || '/' || blocked_reason || '/' || resume_state FROM runs WHERE run_id = '{k}'"),
                "BLOCKED/NEED_DECISION/SPECIFYING",
                Duration::from_secs(60),
            )
            .await?;
        // K is quiet: no attempt unfinished, no task but its Commander's turns, its request PROPOSED.
        world
            .until(
                &format!(
                    "SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = '{k}' \
                     AND a.state NOT IN ('SETTLED', 'FAILED', 'CANCELLED', 'UNKNOWN_EFFECT')"
                ),
                "0",
                WAIT,
            )
            .await?;
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM tasks WHERE run_id = '{k}' AND role <> 'commander'"
            ))?,
            "0"
        );
        let request = world.query(&format!(
            "SELECT decision_id || '/' || state FROM decisions WHERE run_id = '{k}' \
             AND json_extract(payload_json, '$.kind') = 'request_user_decision'"
        ))?;
        let (request, proposed) = request.rsplit_once('/').ok_or("no request")?;
        assert_eq!(proposed, "PROPOSED");
        world.exec(&format!(
            "INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES
               ('{RUN_C}', :p, 'COMPLETED', 'AUTO', 5, '2026-11-15T00:00:00Z');"
        ))?;
        let request = request.to_owned();
        tokio::task::block_in_place(|| -> R {
            let c = || world.query(&format!("SELECT state || '/' || revision FROM runs WHERE run_id = '{RUN_C}'"));
            let runs = count(&world, "runs")?;
            // The first five: 64, nothing on stdout, a message on stderr; the runs as they were, C COMPLETED at 5.
            for args in [
                &["-p"][..],
                &["-p", ""],
                &["-p", "  "],
                &["-p", "x", "--resume", "NOPE"],
                &["-p", "x", "--resume", RUN_C],
            ] {
                let out = world.hoh(args)?;
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                assert_eq!(out.status.code(), Some(64), "{args:?}: {stderr}");
                assert_eq!(out.stdout.len(), 0, "{args:?}");
                assert!(!stderr.trim().is_empty(), "{args:?}");
                assert_eq!(count(&world, "runs")?, runs, "{args:?}");
                assert_eq!(c()?, "COMPLETED/5", "{args:?}");
            }
            // The sixth: no run more, exactly one command more (SendInput to K), 4, K INCOMPLETE.
            let commands = count(&world, "commands")?;
            let out = world.hoh(&["-p", "采用方案A", "--resume", &k])?;
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            assert_eq!(count(&world, "runs")?, runs);
            let sent = commands_after(&world, commands)?;
            assert_eq!(sent.len(), 1, "{sent:?}; stderr: {stderr}");
            assert_eq!(
                (&sent[0]["kind"], &sent[0]["run_id"]),
                (&json!("SendInput"), &json!(k)),
                "{sent:?}"
            );
            assert_eq!(out.status.code(), Some(4), "{stderr}");
            assert_eq!(
                world.query(&format!("SELECT state FROM runs WHERE run_id = '{k}'"))?,
                "INCOMPLETE"
            );
            // The request answered: APPLIED, and the answer is K's question.answered's.
            assert_eq!(
                world.query(&format!("SELECT state FROM decisions WHERE decision_id = '{request}'"))?,
                "APPLIED"
            );
            let answers: Vec<String> = world
                .rows(&format!(
                    "SELECT json_extract(payload_json, '$.answer') AS answer FROM events \
                     WHERE run_id = '{k}' AND kind = 'question.answered'"
                ))?
                .iter()
                .map(|r| r["answer"].as_str().unwrap_or_default().to_owned())
                .collect();
            assert_eq!(answers, ["采用方案A"]);
            Ok(())
        })
    }
    .await;
    host.shutdown().await;
    outcome?;
    assert!(team.refused().is_empty(), "{team:?}");
    Ok(())
}

/// The product's world of TEST-002-02, -004-02, -004-03, -004-04 and -042-04 Ra (docs/adr/ADR-031.md decision 14): the
/// committed fixture project (REQ-901, AC-901-01, whose check passes) with `team` and `toml` in its configuration, its
/// fake CLIs registered, its in-process product host started and the specification approved; QA runs the planned
/// check.
async fn product_world(world: &World, team: &[(&str, &str)], toml: &str) -> R<Host> {
    world.configure(team, toml)?;
    world.commit_project(&world::fixture_spec())?;
    world.verifier(
        "qa",
        &world::Verifier {
            checks: vec![world::fixture_spec()[0].criteria[0].check_id()],
            ..world::Verifier::default()
        },
    )?;
    world.register()?;
    let host = world.start_host().await?;
    if let Err(e) = world.seed_spec() {
        host.shutdown().await;
        return Err(e);
    }
    Ok(host)
}

/// Every string in `value`, at any depth.
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(strings).collect(),
        Value::Object(o) => o.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// The run StartRun made of the `n`-th StartRun after the first `before` commands, once there is one.
fn nth_started(world: &World, before: i64, n: usize) -> R<String> {
    let end = Instant::now() + WAIT;
    loop {
        let started: Vec<Value> = commands_after(world, before)?
            .into_iter()
            .filter(|c| c["kind"] == "StartRun")
            .collect();
        if let Some(run) = started.get(n).and_then(|c| c["run_id"].as_str()) {
            return Ok(run.to_owned());
        }
        if Instant::now() > end {
            return Err(format!("no StartRun #{n} after {before}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What run `run` decided: its decisions' kind, state, to_state and basis.
fn decided(world: &World, run: &str) -> R<Vec<Value>> {
    world.rows(&format!(
        "SELECT json_extract(payload_json, '$.kind') AS kind, state, json_extract(payload_json, '$.to_state') AS \
         to_state, json_extract(payload_json, '$.basis') AS basis FROM decisions WHERE run_id = '{run}' ORDER BY rowid"
    ))
}

/// The pinned script's outcome on run `run` (M2T13D-52): at least one APPLIED record_assumption with a basis, and no
/// APPLIED transition_run to VERIFYING, COMPLETED, FAILED or INCOMPLETE (its end is the pace's budget stop).
fn assumed_and_never_moved_past_running(world: &World, run: &str) -> R {
    let decided = decided(world, run)?;
    assert!(
        decided.iter().any(|d| d["kind"] == "record_assumption"
            && d["state"] == "APPLIED"
            && d["basis"].as_str().is_some_and(|b| !b.trim().is_empty())),
        "{run}: {decided:?}"
    );
    assert!(
        !decided.iter().any(|d| d["kind"] == "transition_run"
            && d["state"] == "APPLIED"
            && ["VERIFYING", "COMPLETED", "FAILED", "INCOMPLETE"]
                .contains(&d["to_state"].as_str().unwrap_or("-"))),
        "{run}: {decided:?}"
    );
    Ok(())
}

/// Why run `run` stopped: its `run.incomplete`'s stop and how many iterations it completed.
fn stopped_by(world: &World, run: &str) -> R<(String, i64)> {
    let stop = world.query(&format!(
        "SELECT json_extract(payload_json, '$.stop') FROM events WHERE run_id = '{run}' AND kind = 'run.incomplete'"
    ))?;
    let iterations: i64 = world
        .query(&format!(
            "SELECT count(*) FROM events WHERE run_id = '{run}' AND kind = 'iteration.completed'"
        ))?
        .parse()?;
    Ok((stop, iterations))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_002_02_defaults_become_assumptions_and_the_iteration_budget_ends_incomplete() -> R {
    // AC-002-02: the product's world with `[budget] max_iterations = 1`; the pinned fake Commander records an
    // assumption, plans the developer task and moves the run to RUNNING, never further; the Developer commits, QA and
    // the Reviewer pass (one join, one iteration), so the run ends only by the pace's budget stop: INCOMPLETE. (a) `hoh
    // -p 'fix bug'` in a PTY, `y` and Enter typed once it runs; (b) the same with stdin a pipe never written nor closed.
    let world = World::new()?;
    let host = product_world(&world, &world::TEAM, "[budget]\nmax_iterations = 1\n").await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        let goal = "fix bug";
        let (runs, commands) = (count(&world, "runs")?, count(&world, "commands")?);
        // (a) In a PTY.
        let pty = pty::Pty::new(30, 100)?;
        let started = Instant::now();
        let mut a = world.hoh_pty(&pty, &["-p", goal])?;
        let run_a = nth_started(&world, commands, 0)?;
        let mut typed = pty;
        typed.send(b"y\n")?;
        let status_a = pty::exit_within(&mut a, Duration::from_secs(60))?;
        let took_a = started.elapsed();
        typed.drained(WAIT)?;
        let shown_a = typed.output();
        assert_eq!(count(&world, "runs")?, runs + 1, "one run more for (a)");
        // (b) stdin a pipe, never written nor closed.
        let started = Instant::now();
        let mut b = world
            .command(&["-p", goal])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let open_stdin = b.stdin.take();
        let out_b = world::finish(b, Duration::from_secs(60))?;
        let took_b = started.elapsed();
        drop(open_stdin);
        let run_b = nth_started(&world, commands, 1)?;
        assert_eq!(count(&world, "runs")?, runs + 2, "one run more for (b)");
        // Both 4, both runs INCOMPLETE.
        assert_eq!(
            status_a.code(),
            Some(4),
            "{}",
            String::from_utf8_lossy(&shown_a)
        );
        assert_eq!(
            out_b.status.code(),
            Some(4),
            "{}",
            String::from_utf8_lossy(&out_b.stderr)
        );
        for run in [&run_a, &run_b] {
            assert_eq!(
                world.query(&format!("SELECT state FROM runs WHERE run_id = '{run}'"))?,
                "INCOMPLETE",
                "{run}"
            );
            assumed_and_never_moved_past_running(&world, run)?;
            // The fixture as pinned: one join counted the one iteration, and the pace stopped the run on it.
            assert_eq!(
                stopped_by(&world, run)?,
                ("max_iterations".to_owned(), 1),
                "{run}"
            );
        }
        // The PTY never showed the alternate screen.
        assert_eq!(pty::find(&shown_a, ENTER_ALT, 0), None);
        // No command's or input's payload holds the `y` typed: each field compared, the goals `fix bug`.
        for row in world.rows("SELECT kind, payload_json FROM commands UNION ALL SELECT kind, payload_json FROM input_queue")? {
            let payload: Value = serde_json::from_str(row["payload_json"].as_str().unwrap_or("null"))?;
            assert!(
                !strings(&payload).iter().any(|s| s.trim_end() == "y"),
                "{row}"
            );
            if row["kind"] == "StartRun" {
                assert_eq!(payload["goal"], goal, "{row}");
            }
        }
        // (b) ended within its 60 s although its stdin stayed open.
        assert!(took_b < Duration::from_secs(60), "{took_b:?}");
        eprintln!("(a) {took_a:?}, (b) {took_b:?}");
        Ok(())
    });
    host.shutdown().await;
    outcome
}

/// Every value under a key `statement` in `value`, at any depth, a string that is itself a JSON document (a bridge
/// line an event carries) looked into as well.
fn statements(value: &Value) -> Vec<String> {
    match value {
        Value::Object(o) => o
            .iter()
            .flat_map(|(k, v)| {
                let mut found = statements(v);
                if k == "statement"
                    && let Some(s) = v.as_str()
                {
                    found.push(s.to_owned());
                }
                found
            })
            .collect(),
        Value::Array(a) => a.iter().flat_map(statements).collect(),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(|v| v.is_object() || v.is_array())
            .map(|v| statements(&v))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// How many times `needle` occurs in `hay`.
fn occurrences(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_004_02_stream_json_is_every_event_of_the_run_escaped_and_ends_with_run_result() -> R {
    // AC-004-02: the product's world on TEST-002-02's pinned script (max_iterations = 1: the end can only be
    // INCOMPLETE); the Commander's assumption in Chinese with an emoji ZWJ sequence, U+2028, U+2029 and a line feed
    // inside the string; the Developer writes 10 MiB of marked noise to its stderr. `hoh -p 'x' --output stream-json`,
    // its stdout read one line at a time with 50 ms between lines, its stderr drained alongside.
    let statement = "按默认规格实现 👩\u{200d}👩\u{200d}👧\u{2028}第二段\u{2029}第三段\n第四行";
    let marker = "HOH-004-02-NOISE";
    let world = World::new()?;
    world.commander(&world::Commander {
        statement: statement.to_owned(),
        ..world::Commander::default()
    })?;
    world.developer(&world::Developer {
        stderr_bytes: 10 << 20,
        stderr_marker: marker.to_owned(),
        ..world::Developer::default()
    })?;
    let host = product_world(&world, &world::TEAM, "[budget]\nmax_iterations = 1\n").await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        let commands = count(&world, "commands")?;
        let mut child = world
            .command(&["-p", "x", "--output", "stream-json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut err = child.stderr.take().ok_or("no stderr")?;
        let stderr = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = err.read_to_end(&mut bytes);
            bytes
        });
        // The slow reader: one line, then 50 ms, until the end of stdout.
        let mut out = std::io::BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let (mut all, mut line) = (Vec::new(), Vec::new());
            while out.read_until(b'\n', &mut line)? > 0 {
                all.extend_from_slice(&line);
                line.clear();
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(all)
        });
        let end = Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() > end {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the follow did not end within 120 s".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let stdout = reader.join().map_err(|_| "the stdout reader panicked")??;
        let stderr = stderr.join().map_err(|_| "the stderr reader panicked")?;
        let run = nth_started(&world, commands, 0)?;
        // Every line (framed by 0x0A) parses and is an Event; the last one a RunResultEvent.
        let lines: Vec<&[u8]> = stdout
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        let (mut unparsed, mut invalid, mut events) = (0, 0, Vec::new());
        for raw in &lines {
            match serde_json::from_slice::<Value>(raw) {
                Ok(event) => {
                    if hoh_core::schema::protocol()
                        .validate_def("Event", &event)?
                        .is_err()
                    {
                        invalid += 1;
                    }
                    events.push(event);
                }
                Err(_) => unparsed += 1,
            }
        }
        assert_eq!(
            (unparsed, invalid),
            (0, 0),
            "{}",
            String::from_utf8_lossy(&stdout)
        );
        let last = events.last().ok_or("no line")?;
        let valid = hoh_core::schema::protocol().validate_def("RunResultEvent", last)?;
        assert!(valid.is_ok(), "{last}: {valid:?}");
        // Seqs strictly increasing, and the set of them the run's events in the store once the client is gone.
        let seqs: Vec<i64> = events
            .iter()
            .map(|e| e["seq"].as_i64().unwrap_or(-1))
            .collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
        let stored: Vec<i64> = world
            .rows(&format!(
                "SELECT seq FROM events WHERE run_id = '{run}' ORDER BY seq"
            ))?
            .iter()
            .map(|r| r["seq"].as_i64().unwrap_or(-2))
            .collect();
        assert_eq!(seqs, stored);
        // U+2028 (and U+2029) escaped, never raw; decoded, the statement is the Commander's own.
        assert_eq!(occurrences(&stdout, "\u{2028}".as_bytes()), 0);
        assert_eq!(occurrences(&stdout, "\u{2029}".as_bytes()), 0);
        assert!(occurrences(&stdout, b"\\u2028") > 0);
        assert!(occurrences(&stdout, b"\\u2029") > 0);
        let said: Vec<String> = events.iter().flat_map(statements).collect();
        assert!(said.iter().any(|s| s == statement), "{said:?}");
        // No escape byte and none of the noise on stdout; no line marked a replay.
        assert_eq!(occurrences(&stdout, b"\x1b"), 0);
        assert_eq!(occurrences(&stdout, marker.as_bytes()), 0);
        assert!(events.iter().all(|e| e.get("replay").is_none()));
        // The last line is the run.result, exit_code 4, and the process exits 4.
        assert_eq!(last["kind"], "run.result", "{last}");
        assert_eq!(last["payload"]["exit_code"], 4, "{last}");
        assert_eq!(
            status.code(),
            Some(4),
            "{}",
            String::from_utf8_lossy(&stderr[..stderr.len().min(2000)])
        );
        // The fixture: the noise was written to the native stderr (the client forwards it).
        assert!(occurrences(&stderr, marker.as_bytes()) > 0);
        eprintln!("{} lines, stderr {} bytes", lines.len(), stderr.len());
        Ok(())
    });
    host.shutdown().await;
    outcome
}

/// `hoh args…` of the world (`env` added) with stdout redirected to file `out` and stderr piped, run to its end
/// (killed after 60 s): its status, stdout (the file's bytes) and stderr.
fn to_file(
    world: &World,
    args: &[&str],
    env: &[(&str, &str)],
    out: &Path,
) -> R<(Option<i32>, Vec<u8>, Vec<u8>)> {
    let mut command = world.command(args);
    command.envs(env.iter().copied());
    let child = command
        .stdout(std::fs::File::create(out)?)
        .stderr(Stdio::piped())
        .spawn()?;
    let done = world::finish(child, Duration::from_secs(60))?;
    Ok((done.status.code(), std::fs::read(out)?, done.stderr))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_004_03_json_text_and_detach_outputs() -> R {
    // AC-004-03: TEST-002-02's pinned script (each foreground run ends INCOMPLETE, 4), the Developer's stderr marked
    // noise; stdout redirected to a file: `--output json`, `--output text`, `NO_COLOR=1 … --output text`, then
    // `--detach --output json`.
    let marker = "HOH-004-03-NOISE";
    let world = World::new()?;
    world.developer(&world::Developer {
        stderr_bytes: 256 << 10,
        stderr_marker: marker.to_owned(),
        ..world::Developer::default()
    })?;
    let host = product_world(&world, &world::TEAM, "[budget]\nmax_iterations = 1\n").await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        let file = world.root().join("stdout");
        let commands = count(&world, "commands")?;
        // json: one JSON value with exactly one LF after it, REQ-004 behaviour 1's eight keys, its exit_code the
        // process's, 4.
        let (code, stdout, stderr) = to_file(&world, &["-p", "j", "--output", "json"], &[], &file)?;
        let said = String::from_utf8_lossy(&stderr[..stderr.len().min(2000)]).into_owned();
        let result: Value = serde_json::from_slice(&stdout)?;
        assert!(
            stdout.ends_with(b"\n") && !stdout.ends_with(b"\n\n"),
            "{stdout:?}"
        );
        assert_eq!(
            stdout.iter().filter(|b| **b == b'\n').count(),
            1,
            "{stdout:?}"
        );
        for key in [
            "run_id",
            "state",
            "exit_code",
            "blocked_reason",
            "spec_revision",
            "accepted",
            "total",
            "last_event_seq",
        ] {
            assert!(result.get(key).is_some(), "{key} not in {result}");
        }
        assert_eq!(code, Some(4), "{said}");
        assert_eq!(
            result["exit_code"].as_i64(),
            code.map(i64::from),
            "{result}"
        );
        let mut foreground = vec![(stdout, stderr)];
        // text, twice (the second with NO_COLOR=1): no ESC, the run's id and INCOMPLETE.
        for (n, env) in [(1, &[][..]), (2, &[("NO_COLOR", "1")][..])] {
            let (code, stdout, stderr) =
                to_file(&world, &["-p", "t", "--output", "text"], env, &file)?;
            let run = nth_started(&world, commands, n)?;
            let text = String::from_utf8(stdout.clone())?;
            assert_eq!(
                code,
                Some(4),
                "{env:?}: {}",
                String::from_utf8_lossy(&stderr[..stderr.len().min(2000)])
            );
            assert_eq!(occurrences(&stdout, b"\x1b"), 0, "{env:?}: {text:?}");
            assert!(
                text.contains(&run) && text.contains("INCOMPLETE"),
                "{env:?}: {text:?}"
            );
            foreground.push((stdout, stderr));
        }
        // The three foreground runs: the noise on stderr, never on stdout.
        for (stdout, stderr) in &foreground {
            assert!(occurrences(stderr, marker.as_bytes()) > 0);
            assert_eq!(occurrences(stdout, marker.as_bytes()), 0);
        }
        // detach: 0, ACCEPTED, and its run is in the store once its id is read.
        let (code, stdout, stderr) = to_file(
            &world,
            &["-p", "d", "--detach", "--output", "json"],
            &[],
            &file,
        )?;
        let accepted: Value = serde_json::from_slice(&stdout)?;
        let run = accepted["run_id"].as_str().ok_or("no run_id")?;
        let stored = world.query(&format!("SELECT count(*) FROM runs WHERE run_id = '{run}'"))?;
        assert_eq!(code, Some(0), "{}", String::from_utf8_lossy(&stderr));
        assert_eq!(accepted["status"], "ACCEPTED", "{accepted}");
        assert_eq!(stored, "1");
        Ok(())
    });
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_004_04_auth_blocks_with_3_and_sigint_detaches_with_130() -> R {
    // AC-004-04, two projects on TEST-002-02's pinned script. P1: the developer bound to codex, whose fake replays an
    // unauthorized turn (codex_adapter's synthesis), the Commander to claude: `hoh -p 'x' --output json`. P2: the
    // Developer works on and on: `hoh -p 'y' --output stream-json`, SIGINT once a line was read and the run works.
    let p1 = World::new()?;
    let mut team = world::TEAM.to_vec();
    team[0] = ("commander", "claude");
    team[2] = ("developer", "codex");
    p1.unauthorized("codex", &["developer"])?;
    let p2 = World::new()?;
    p2.developer(&world::Developer {
        seconds: 600.0,
        ..world::Developer::default()
    })?;
    let host1 = product_world(&p1, &team, "").await?;
    let host2 = match product_world(&p2, &world::TEAM, "").await {
        Ok(host) => host,
        Err(e) => {
            host1.shutdown().await;
            return Err(e);
        }
    };
    let outcome = tokio::task::block_in_place(|| -> R {
        // P1: 3, BLOCKED for BLOCKED_AUTH, never COMPLETED.
        let out = p1.hoh(&["-p", "x", "--output", "json"])?;
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(3), "{said}");
        let result: Value = serde_json::from_slice(&out.stdout)?;
        assert_eq!(result["state"], "BLOCKED", "{result}");
        assert_eq!(result["blocked_reason"], "BLOCKED_AUTH", "{result}");
        assert_ne!(result["state"], "COMPLETED", "{result}");
        // P2: once a line was read and the run works, SIGINT.
        let commands = count(&p2, "commands")?;
        let mut child = p2
            .command(&["-p", "y", "--output", "stream-json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut err = child.stderr.take().ok_or("no stderr")?;
        let stderr = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = err.read_to_end(&mut bytes);
            bytes
        });
        let read = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut out = std::io::BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let lines = read.clone();
        let stdout = std::thread::spawn(move || {
            let mut line = String::new();
            while out.read_line(&mut line).unwrap_or(0) > 0 {
                lines.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                line.clear();
            }
        });
        let run = nth_started(&p2, commands, 0)?;
        let end = Instant::now() + Duration::from_secs(60);
        while read.load(std::sync::atomic::Ordering::SeqCst) == 0
            || p2.query(&format!(
                "SELECT r.state || '/' || (SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE t.run_id = r.run_id AND t.role = 'developer' AND a.state = 'RUNNING') FROM runs r \
                 WHERE r.run_id = '{run}'"
            ))? != "RUNNING/1"
        {
            if Instant::now() > end {
                let _ = child.kill();
                return Err("P2's run did not work within 60 s".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        signal(child.id(), rustix::process::Signal::INT)?;
        let sent = Instant::now();
        let status = child.wait()?;
        let _ = stdout.join();
        let stderr =
            String::from_utf8_lossy(&stderr.join().map_err(|_| "the stderr reader panicked")?)
                .into_owned();
        // 130; stderr names the run and `hoh attach`, and no panic.
        assert_eq!(status.code(), Some(130), "{stderr}");
        assert!(
            stderr.contains(&run) && stderr.contains("hoh attach"),
            "{stderr}"
        );
        assert!(!stderr.contains("panicked at"), "{stderr}");
        // For 5 s from the SIGINT the run works on, RUNNING, and no CancelRun was sent.
        while sent.elapsed() < Duration::from_secs(5) {
            assert_eq!(
                p2.query(&format!("SELECT state FROM runs WHERE run_id = '{run}'"))?,
                "RUNNING"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            p2.query("SELECT count(*) FROM commands WHERE kind = 'CancelRun'")?,
            "0"
        );
        Ok(())
    });
    host1.shutdown().await;
    host2.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_042_04_iterations_and_deadline_end_incomplete_with_unknown_cost() -> R {
    // AC-042-04. Ra: the product's world, `max_iterations = 2`, no price for any model (CR-002: the cost is unknown),
    // TEST-002-02's pinned Commander; each round QA ends COMPLETED with a FAIL report (CR-018) and the Reviewer
    // COMPLETED with a PASS one, so each join fails, counts an iteration and sends the work back: `hoh -p
    // '完成fixture目标' --output json`. Rb: an in-process host on a FakeClock, its runner the scripted Team, a run moved
    // to RUNNING with no attempt in flight and `deadline_at = T0 + 60 s`; the clock moved on to T0 + 60 s.
    let ra = World::new()?;
    let host = product_world(&ra, &world::TEAM, "[budget]\nmax_iterations = 2\n").await?;
    let outcome = async {
        ra.verifier(
            "qa",
            &world::Verifier {
                report: "FAIL".to_owned(),
                checks: vec![world::fixture_spec()[0].criteria[0].check_id()],
                ..world::Verifier::default()
            },
        )?;
        tokio::task::block_in_place(|| -> R {
            let commands = count(&ra, "commands")?;
            let out = ra.hoh(&["-p", "完成fixture目标", "--output", "json"])?;
            let said = String::from_utf8_lossy(&out.stderr).into_owned();
            let run = nth_started(&ra, commands, 0)?;
            let result: Value = serde_json::from_slice(&out.stdout)?;
            assert_eq!(out.status.code(), Some(4), "{said}");
            // Two iterations, each a failed join.
            let iterations = ra.rows(&format!(
                "SELECT seq, json_extract(payload_json, '$.iteration') AS n, json_extract(payload_json, '$.verdict') \
                 AS verdict FROM events WHERE run_id = '{run}' AND kind = 'iteration.completed' ORDER BY seq"
            ))?;
            assert_eq!(
                iterations.iter().map(|i| i["n"].clone()).collect::<Vec<_>>(),
                [json!(1), json!(2)],
                "{iterations:?}"
            );
            assert_eq!(count_where(&ra, "iterations", &format!("run_id = '{run}'"))?, 2);
            // Each round's QA attempt SETTLED with a COMPLETED result, no re-verification, and its report FAIL.
            let qa = ra.rows(&format!(
                "SELECT a.attempt_id, a.state, (SELECT json_extract(e.payload_json, '$.result_status') FROM events e \
                 WHERE e.kind = 'verification.result' AND json_extract(e.payload_json, '$.attempt_id') = a.attempt_id) \
                 AS result, (SELECT r.status FROM independent_reports r WHERE r.attempt_id = a.attempt_id) AS report \
                 FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = '{run}' AND t.role = 'qa' \
                 ORDER BY a.rowid"
            ))?;
            assert_eq!(qa.len(), 2, "{qa:?}");
            for a in &qa {
                assert_eq!(
                    (&a["state"], &a["result"], &a["report"]),
                    (&json!("SETTLED"), &json!("COMPLETED"), &json!("FAIL")),
                    "{qa:?}"
                );
            }
            assert_eq!(
                count_where(&ra, "events", &format!("run_id = '{run}' AND kind = 'task.reverify'"))?,
                0
            );
            // After the second iteration no dispatch more.
            let second = iterations[1]["seq"].as_i64().ok_or("no seq")?;
            assert_eq!(
                count_where(
                    &ra,
                    "outbox",
                    &format!("run_id = '{run}' AND event_seq > {second} AND json_extract(payload_json, '$.kind') = 'dispatch'")
                )?,
                0
            );
            // INCOMPLETE, the requirements of the specification not accepted left, the cost unknown, never COMPLETED.
            let spec: Vec<Value> = ra
                .rows(&format!(
                    "SELECT q.requirement_id AS id FROM requirements q JOIN runs r ON r.spec_id = q.spec_id \
                     WHERE r.run_id = '{run}' ORDER BY q.requirement_id"
                ))?
                .into_iter()
                .map(|r| r["id"].clone())
                .collect();
            assert_eq!(result["state"], "INCOMPLETE", "{result}");
            assert_eq!(result["accepted"], 0, "{result}");
            assert_eq!(result["remaining_requirements"], json!(spec), "{result}");
            assert!(!spec.is_empty());
            assert_eq!(result["cost_usd"], Value::Null, "{result}");
            assert_eq!(result["cost_status"], "unknown", "{result}");
            assert_ne!(result["state"], "COMPLETED", "{result}");
            assert_eq!(
                ra.query(&format!("SELECT state FROM runs WHERE run_id = '{run}'"))?,
                "INCOMPLETE"
            );
            Ok(())
        })
    }
    .await;
    host.shutdown().await;
    outcome?;

    // Rb.
    let rb = World::new()?;
    rb.commit_project(&world::fixture_spec())?;
    let clock = std::sync::Arc::new(world::FakeClock::default());
    let team = world::Team::new(|turn| world::steps(&turn.state, "RUNNING"));
    let host = rb.team_host(team.clone(), Some(clock.clone())).await?;
    let outcome = async {
        rb.seed_spec()?;
        let run = world::start_run(&host, "rb", "完成fixture目标").await?;
        let t0 = hoh_core::delivery::Clock::now(clock.as_ref());
        let deadline = hoh_core::time::rfc3339_utc(t0 + Duration::from_secs(60));
        rb.exec(&format!("UPDATE runs SET deadline_at = '{deadline}' WHERE run_id = '{run}';"))?;
        rb.staff(&run)?;
        rb.until(
            &format!(
                "SELECT r.state || '/' || (SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE t.run_id = r.run_id AND a.state NOT IN ('SETTLED', 'FAILED', 'CANCELLED', 'UNKNOWN_EFFECT')) \
                 FROM runs r WHERE r.run_id = '{run}'"
            ),
            "RUNNING/0",
            WAIT,
        )
        .await?;
        clock.advance(Duration::from_secs(60));
        rb.until(
            &format!("SELECT state FROM runs WHERE run_id = '{run}'"),
            "INCOMPLETE",
            WAIT,
        )
        .await?;
        let result = result_of(&rb, &run)?;
        assert_eq!(result["state"], "INCOMPLETE", "{result}");
        assert_ne!(result["state"], "COMPLETED", "{result}");
        assert_eq!(
            rb.query(&format!(
                "SELECT json_extract(payload_json, '$.stop') FROM events WHERE run_id = '{run}' AND kind = 'run.incomplete'"
            ))?,
            "deadline_at"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome?;
    assert!(team.refused().is_empty(), "{team:?}");
    Ok(())
}

/// The rows of `table` where `condition` holds.
fn count_where(world: &World, table: &str, condition: &str) -> R<i64> {
    Ok(world
        .query(&format!("SELECT count(*) FROM {table} WHERE {condition}"))?
        .parse()?)
}
