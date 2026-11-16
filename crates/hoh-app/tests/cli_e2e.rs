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
        // The new run staffed (the scripted Team has no agent registry, so its host leaves the bindings vacant) and,
        // until slice S2b's StartRun adopts the project's latest approved specification itself, adopted here by SQL
        // (World::adopt; S2b replaces it, and then it writes nothing).
        let run = run_of_goal(&world, goal)?;
        world.staff(&run)?;
        world.adopt(&run, &spec)?;
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
    // K is made through the product (StartRun, its staffing, its Commander's turn to SPECIFYING and its request): the
    // rows the test plan seeds by SQL (its specification, its Commander's binding and session, the PROPOSED
    // request_user_decision, the claim `commander:<id>`, question.asked, resume_state SPECIFYING, no task, no attempt
    // unfinished), written by their own writers. C is seeded by SQL. Then, one after the other: `hoh -p`, `-p ''`,
    // `-p '  '`, `-p x --resume NOPE`, `-p x --resume <C>`, `-p '采用方案A' --resume <K>`.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    let team = asking_commander();
    let host = world.team_host(team.clone(), None).await?;
    let outcome = async {
        let spec = world.seed_spec()?;
        let k = world::start_run(&host, "k", "实现登录功能").await?;
        world.staff(&k)?;
        // Until slice S2b's StartRun adopts the specification itself (World::adopt; S2b replaces it).
        world.adopt(&k, &spec)?;
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
