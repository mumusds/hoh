//! The M2-T13 test world on its own (docs/adr/ADR-031.md decision 14, slice S5): the fixtures the eleven TESTs of
//! M2-T13 share, checked before any TEST uses them. These are tests of the fixtures, not of an acceptance criterion:
//! the vt100 screen snapshot and its erase counter-example, the PTY and plain-child helpers, the fake CLIs' compat
//! suites, the pinned fake Commander on codex and on claude (and its create_tasks submitted again once refused), the
//! fake Developer and verifiers through the product executor, a codex turn that fails unauthorized, a host the world's
//! own `hoh` starts, and the scripted Team's Commander on an in-process host.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use hoh_app::host::Host;
use pty::R;
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::time::{Duration, Instant};
use world::World;

/// How long a smoke test waits for the product to get where it looks.
const WAIT: Duration = Duration::from_secs(120);

#[test]
fn the_screen_snapshot_shows_only_what_erases_and_overwrites_left() -> R {
    // Decision 14: the snapshot executes cursor moves, erases and overwrites, so text that is no longer on the screen
    // never satisfies an assertion. The helper passes its own counter-examples ...
    pty::erase_check(&pty::screen)?;
    // ... which a helper that only strips control sequences and joins the text (pty::squash) fails.
    let squash_like = |bytes: &[u8], _: u16, _: u16| -> R<String> { Ok(pty::squash(bytes)) };
    assert!(pty::erase_check(&squash_like).is_err());
    // A screen as a TUI draws it: a frame, then a redraw in place.
    let drawn = b"\x1b[?1049h\x1b[2J\x1b[1;1Hhoh  demo  PAUSED\x1b[2;1Hrun 0123456789\x1b[1;11HRUNNING\x1b[K";
    assert_eq!(
        pty::snapshot(drawn, 5, 40)?,
        "hoh  demo RUNNING\nrun 0123456789"
    );
    Ok(())
}

#[test]
fn a_world_directory_that_was_not_removed_is_reported_with_what_is_left_and_who_holds_it() -> R {
    // World::drop removes its directory itself and, when that fails, writes this report to stderr: the error, every
    // entry left with its size and last write, and every process with a file open there or working in it.
    let dir = tempfile::tempdir()?;
    let kept = dir.path().join("kept");
    std::fs::create_dir(&kept)?;
    std::fs::write(kept.join("file"), b"abc")?;
    let held = std::fs::File::open(kept.join("file"))?;
    let mut sleeper = pty::Hoh(
        std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&kept)
            .spawn()?,
    );
    // Its working directory set once it runs (before that the forked copy works where this process does).
    let end = Instant::now() + Duration::from_secs(10);
    while std::fs::read_link(format!("/proc/{}/cwd", sleeper.0.id())).ok() != Some(kept.clone()) {
        if Instant::now() > end {
            return Err("sleep never worked in the directory".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let report = world::leftover(dir.path(), &std::io::Error::other("Directory not empty"));
    sleeper.0.kill()?;
    sleeper.0.wait()?;
    drop(held);
    let line = |needle: &str| {
        report
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_default()
            .to_owned()
    };
    assert!(
        line("was not removed").contains("Directory not empty"),
        "{report}"
    );
    let file = line(&format!("{} ", kept.join("file").display()));
    assert_eq!(file.split_whitespace().nth(1), Some("3"), "{report}");
    let me = line(&format!("pid {} ", std::process::id()));
    assert!(
        me.contains(&format!("-> {}", kept.join("file").display())),
        "{report}"
    );
    let sleeping = line(&format!("pid {} ", sleeper.0.id()));
    assert!(
        sleeping.contains(&format!("cwd {}", kept.display())),
        "{report}"
    );
    Ok(())
}

#[test]
fn a_world_directory_made_again_just_after_its_removal_is_reported_too() -> R {
    // World::drop removes its directory (world::removed) and, when that succeeded, looks once more a moment later: a
    // directory something made again meanwhile (a late writer's create_dir_all) is reported as a failed removal is.
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("w");
    std::fs::create_dir_all(root.join("p/.hoh"))?;
    let again = root.join("p/.hoh/attempts");
    let late = {
        let again = again.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            std::fs::create_dir_all(&again)
        })
    };
    let report = world::removed(&root).ok_or("nothing reported")?;
    late.join().map_err(|_| "the late writer panicked")??;
    assert!(report.contains("made again"), "{report}");
    assert!(
        report.contains(&format!("{} ", again.display())),
        "{report}"
    );
    // A removal nothing undoes reports nothing.
    std::fs::create_dir_all(root.join("p"))?;
    std::fs::remove_dir_all(&again)?;
    assert_eq!(world::removed(&root), None);
    assert!(!root.exists());
    Ok(())
}

#[test]
fn the_pty_helper_runs_a_program_of_the_world_and_snapshots_its_screen() -> R {
    let world = World::new()?;
    let pty = pty::Pty::new(10, 60)?;
    let mut child = world.spawn_pty(
        &pty,
        OsStr::new("sh"),
        &[
            "-c",
            "printf 'erased\\033[2J\\033[1;1Hshown %s %s %s\\n' \"$HOME\" \"$TERM\" \"$(tty)\"",
        ],
        true,
    )?;
    let status = pty::exit_within(&mut child, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0));
    pty.drained(Duration::from_secs(5))?;
    let screen = pty::snapshot(&pty.output(), 10, 60)?;
    assert!(!screen.contains("erased"), "{screen:?}");
    let shown = screen.lines().next().ok_or("an empty screen")?;
    let mut words = shown.split(' ');
    assert_eq!(words.next(), Some("shown"));
    assert_eq!(words.next(), Some(world.home().to_str().ok_or("home")?));
    assert_eq!(words.next(), Some("xterm-256color"));
    assert!(
        words.next().is_some_and(|t| t.starts_with("/dev/pts/")),
        "its controlling terminal is the PTY: {shown:?}"
    );
    // A shell in the terminal, its session's leader, runs the `hoh` the world's PATH has (TEST-005-01's way).
    let mut pty = pty::Pty::new(10, 80)?;
    let mut shell = world.spawn_pty(&pty, OsStr::new("bash"), &["--noprofile", "--norc"], true)?;
    let (sid, tty) = world::session_and_tty(shell.0.id())?;
    assert_eq!(sid, shell.0.id());
    assert_ne!(tty, 0);
    pty.send(b"hoh --version > version.txt; exit 7\n")?;
    assert_eq!(
        pty::exit_within(&mut shell, Duration::from_secs(10))?.code(),
        Some(7)
    );
    assert!(std::fs::read_to_string(world.project().join("version.txt"))?.starts_with("hoh 0.0.0"));
    // hoh itself, as the TUI tests start it.
    let pty = pty::Pty::new(30, 100)?;
    let mut hoh = world.hoh_pty(&pty, &["--version"])?;
    assert_eq!(
        pty::exit_within(&mut hoh, Duration::from_secs(10))?.code(),
        Some(0)
    );
    pty.drained(Duration::from_secs(5))?;
    assert!(pty::snapshot(&pty.output(), 30, 100)?.starts_with("hoh 0.0.0"));
    Ok(())
}

#[test]
fn a_plain_hoh_command_runs_in_the_project_with_its_output_captured() -> R {
    let world = World::new()?;
    let out = world.hoh(&["--version"])?;
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8(out.stdout)?.starts_with("hoh 0.0.0"));
    let out = world.hoh(&["-p"])?;
    assert_eq!(out.status.code(), Some(64), "a usage error");
    assert!(out.stdout.is_empty() && !out.stderr.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_worlds_hoh_is_a_client_of_its_in_process_host() -> R {
    // `hoh` with the world's environment only finds the host this process runs for the project (attach.rs's way): its
    // detached run is the host's, and no host of its own is started.
    let world = World::new()?;
    let clock = std::sync::Arc::new(world::FakeClock::default());
    let host = world.start_plain_host(Some(clock)).await?;
    let outcome = async {
        let out = tokio::task::block_in_place(|| {
            world.hoh(&["-p", "x", "--detach", "--output", "json"])
        })?;
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let accepted: Value = serde_json::from_slice(&out.stdout)?;
        let run = accepted["run_id"].as_str().ok_or("no run_id")?;
        assert_eq!(
            world.query(&format!("SELECT count(*) FROM runs WHERE run_id = '{run}'"))?,
            "1"
        );
        assert!(world.hosts()?.is_empty(), "the in-process host was used");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[test]
fn the_worlds_fake_agents_pass_the_compat_suite_and_are_the_only_agents_on_its_path() -> R {
    let world = World::new()?;
    let started = Instant::now();
    world.register()?;
    let manifest: Value = serde_json::from_slice(&std::fs::read(
        world.project().join(".hoh/compat-manifest.json"),
    )?)?;
    for agent in ["codex", "claude", "pi"] {
        assert!(
            manifest.to_string().contains(&format!("\"{agent}\"")),
            "{agent} is not in {manifest}"
        );
        assert!(world.bin().join(agent).is_file());
        assert!(!world.tools().join(agent).exists());
    }
    eprintln!("registered in {:?}", started.elapsed());
    Ok(())
}

/// `world`'s committed project with its specification's rows, its agents registered and its in-process product host
/// started; a run started on it, which adopted the specification in StartRun's commit (docs/adr/ADR-031.md decision
/// 1): the host and the run's id.
async fn adopted_run(world: &World, key: &str) -> R<(Host, String)> {
    world.commit_project(&world::fixture_spec())?;
    world.register()?;
    let host = world.start_host().await?;
    let started = async {
        let spec = world.seed_spec()?;
        let run = world::start_run(&host, key, "完成fixture目标").await?;
        assert_eq!(
            world.query(&format!("SELECT spec_id FROM runs WHERE run_id = '{run}'"))?,
            spec
        );
        R::Ok(run)
    }
    .await;
    match started {
        Ok(run) => Ok((host, run)),
        Err(e) => {
            host.shutdown().await;
            Err(e)
        }
    }
}

/// Once the pinned Commander moved `run` to RUNNING: the developer task its create_tasks planned and the product
/// applied (`<run>-develop`, READY, for every requirement of the specification).
async fn planned(world: &World, run: &str) -> R<String> {
    world
        .until(
            &format!("SELECT state FROM runs WHERE run_id = '{run}'"),
            "RUNNING",
            WAIT,
        )
        .await?;
    let task = format!("{run}-develop");
    assert_eq!(
        world.query(&format!(
            "SELECT t.role || ':' || group_concat(q.requirement_id) FROM tasks t \
             JOIN task_requirements q ON q.task_id = t.task_id WHERE t.task_id = '{task}'"
        ))?,
        "developer:REQ-901"
    );
    Ok(task)
}

/// The pinned fake Commander (M2T13D-52) on `agent`: a run with the committed specification and a wake moves to
/// RUNNING by its decisions; its first batch is the whole script at consecutive revisions, every message valid; its
/// record_assumption is applied with its statement byte for byte; no decision of it moves a run past RUNNING.
async fn the_pinned_commander_moves_a_run_to_running_on(agent: &str) -> R {
    let world = World::new()?;
    let mut team = world::TEAM.to_vec();
    team[0] = ("commander", agent);
    world.configure(&team, "")?;
    let statement = "默认采用 fixture 规格 👩\u{200d}👩\u{200d}👧\u{2028}第二段\u{2029}第三段\n第四行 \"引号\" \\ 反斜杠";
    world.commander(&world::Commander {
        statement: statement.to_owned(),
        ..world::Commander::default()
    })?;
    let started = Instant::now();
    let (host, run) = adopted_run(&world, "s1").await?;
    let outcome = async {
        world
            .until(
                &format!("SELECT state FROM runs WHERE run_id = '{run}'"),
                "RUNNING",
                WAIT,
            )
            .await?;
        let sha = world.query(&format!(
            "SELECT s.sha256 FROM specs s JOIN runs r ON r.spec_id = s.spec_id WHERE r.run_id = '{run}'"
        ))?;
        let batches = world.log("commander.jsonl")?;
        let first = batches.first().ok_or("no batch")?;
        assert_eq!(first["agent"].as_str().map(|a| a.starts_with(agent)), Some(true), "{first}");
        assert_eq!(first["state"], "CREATED");
        let revision = first["revision"].as_i64().ok_or("no revision")?;
        let messages = first["messages"].as_array().ok_or("no messages")?;
        let steps: Vec<String> = messages
            .iter()
            .map(|m| {
                let p = &m["payload"];
                match p["to_state"].as_str() {
                    Some(to) => format!("{}:{to}", p["kind"].as_str().unwrap_or("?")),
                    None => p["kind"].as_str().unwrap_or("?").to_owned(),
                }
            })
            .collect();
        assert_eq!(
            steps,
            [
                "record_assumption",
                "transition_run:DISCOVERING",
                "transition_run:SPECIFYING",
                "create_tasks",
                "transition_run:READY",
                "transition_run:RUNNING"
            ]
        );
        for (i, m) in messages.iter().enumerate() {
            assert_eq!(m["expected_revision"].as_i64(), Some(revision + i64::try_from(i)?));
            assert_eq!(m["run_id"], json!(run));
            assert_eq!(m["spec_sha256"], json!(sha));
            let valid = hoh_core::schema::protocol().validate_def("Message", m)?;
            assert!(valid.is_ok(), "{m}: {valid:?}");
        }
        let plan = &messages[3]["payload"]["plan"]["tasks"];
        assert_eq!(plan[0]["task_id"], json!(format!("{run}-develop")));
        assert_eq!(plan[0]["requirement_ids"], json!(["REQ-901"]));
        // Applied as written: the assumption, and the first two steps (what follows depends on create_tasks).
        let applied = world.rows(&format!(
            "SELECT decision_id, json_extract(payload_json, '$.kind') AS kind, json_extract(payload_json, '$.statement') \
             AS statement, json_extract(payload_json, '$.to_state') AS to_state, state FROM decisions \
             WHERE run_id = '{run}' ORDER BY rowid"
        ))?;
        // A decision's id is its message's under the turn's attempt.
        let state_of = |id: &Value| {
            let id = id.as_str().unwrap_or_default();
            applied
                .iter()
                .find(|d| {
                    d["decision_id"]
                        .as_str()
                        .is_some_and(|d| d == id || d.ends_with(&format!("/{id}")))
                })
                .map(|d| d["state"].clone())
        };
        for m in &messages[..3] {
            assert_eq!(state_of(&m["message_id"]), Some(json!("APPLIED")), "{applied:?}");
        }
        assert_eq!(
            applied
                .iter()
                .filter(|d| d["kind"] == "record_assumption" && d["state"] == "APPLIED")
                .map(|d| d["statement"].clone())
                .collect::<Vec<_>>(),
            [json!(statement)]
        );
        // Never past RUNNING, in any batch.
        for batch in &batches {
            for m in batch["messages"].as_array().into_iter().flatten() {
                let to = m["payload"]["to_state"].as_str().unwrap_or("CREATED");
                assert!(
                    ["CREATED", "DISCOVERING", "SPECIFYING", "READY", "RUNNING"].contains(&to),
                    "{m}"
                );
            }
        }
        let turns = world.log("turns.jsonl")?;
        assert!(
            turns
                .iter()
                .filter(|t| t["role"] == "commander" && t["event"] == "end")
                .all(|t| t["failure"].is_null()),
            "{turns:?}"
        );
        // Each codex App Server was asked for one turn on a new thread or its session's thread again.
        let mut asked: std::collections::BTreeMap<i64, Vec<String>> = Default::default();
        for r in world.log("codex-requests.jsonl")? {
            if !r["id"].is_null() {
                asked
                    .entry(r["pid"].as_i64().ok_or("no pid")?)
                    .or_default()
                    .push(r["method"].as_str().unwrap_or("?").to_owned());
            }
        }
        for methods in asked.values() {
            assert!(
                methods == &["initialize", "thread/start", "turn/start"]
                    || methods == &["initialize", "thread/resume", "turn/start"],
                "{asked:?}"
            );
        }
        if agent == "codex" {
            assert!(!asked.is_empty());
        }
        eprintln!(
            "{agent}: RUNNING after {:?}, codex App Servers {asked:?}, {} batches: {}",
            started.elapsed(),
            batches.len(),
            batches
                .iter()
                .map(|b| format!("{}@{} x{}", b["state"], b["revision"], b["messages"].as_array().map_or(0, Vec::len)))
                .collect::<Vec<_>>()
                .join(", ")
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pinned_commander_moves_a_run_to_running_on_codex() -> R {
    the_pinned_commander_moves_a_run_to_running_on("codex").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pinned_commander_moves_a_run_to_running_on_claude() -> R {
    the_pinned_commander_moves_a_run_to_running_on("claude").await
}

/// Each message of `batches` (the fake Commander's log) as (kind, its decision's state and reason code, or none),
/// in the order written; `decisions` are the run's rows (a decision's id is its message's under the turn's attempt).
fn verdicts(batches: &[Value], decisions: &[Value]) -> Vec<(String, Option<(Value, Value)>)> {
    batches
        .iter()
        .flat_map(|b| b["messages"].as_array().cloned().unwrap_or_default())
        .map(|m| {
            let id = format!("/{}", m["message_id"].as_str().unwrap_or("?"));
            let verdict = decisions
                .iter()
                .find(|d| d["decision_id"].as_str().is_some_and(|d| d.ends_with(&id)))
                .map(|d| (d["state"].clone(), d["reason_code"].clone()));
            (
                m["payload"]["kind"].as_str().unwrap_or("?").to_owned(),
                verdict,
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pinned_commander_submits_a_refused_create_tasks_again_and_an_applied_assumption_never()
-> R {
    // The pinned Commander's "once" is by what became of a decision, not by its writing: its first create_tasks names a
    // stale revision (stale_once) and is REJECTED, so it is submitted again on the run's next turn; its
    // record_assumption, APPLIED in the first batch, is never submitted again. The second create_tasks is APPLIED and
    // the run reaches RUNNING.
    let world = World::new()?;
    world.commander(&world::Commander {
        stale_once: vec!["create_tasks".to_owned()],
        ..world::Commander::default()
    })?;
    let started = Instant::now();
    let (host, run) = adopted_run(&world, "s1").await?;
    let outcome = async {
        world
            .until(
                &format!("SELECT state FROM runs WHERE run_id = '{run}'"),
                "RUNNING",
                WAIT,
            )
            .await?;
        let batches = world.log("commander.jsonl")?;
        let decisions = world.rows(&format!(
            "SELECT decision_id, state, reason_code FROM decisions WHERE run_id = '{run}'"
        ))?;
        let all = verdicts(&batches, &decisions);
        let of = |kind: &str| -> Vec<Option<(Value, Value)>> {
            all.iter()
                .filter(|(k, _)| k == kind)
                .map(|(_, v)| v.clone())
                .collect()
        };
        let create = of("create_tasks");
        assert_eq!(create.len(), 2, "{all:?}");
        assert_eq!(
            create[0],
            Some((json!("REJECTED"), json!("STALE_REVISION"))),
            "{all:?}"
        );
        assert_eq!(create[1], Some((json!("APPLIED"), Value::Null)), "{all:?}");
        // Submitted again in a later batch than the refused one.
        let batch_of = |kind: &str, nth: usize| {
            batches
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    b["messages"]
                        .as_array()
                        .is_some_and(|m| m.iter().any(|m| m["payload"]["kind"] == kind))
                })
                .map(|(i, _)| i)
                .nth(nth)
        };
        assert!(
            batch_of("create_tasks", 0) < batch_of("create_tasks", 1),
            "{batches:?}"
        );
        assert_eq!(
            of("record_assumption"),
            [Some((json!("APPLIED"), Value::Null))],
            "{all:?}"
        );
        eprintln!(
            "RUNNING after {:?}, {} batches: {all:?}",
            started.elapsed(),
            batches.len()
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_scripted_teams_commander_moves_a_run_and_asks_a_person() -> R {
    // The scripted Team on an in-process host: the run, started, staffed and given the committed specification, wakes
    // its Commander; the script's first turn (at CREATED) moves it to SPECIFYING and asks a person, its messages valid
    // and at consecutive revisions from the turn's. The moves are APPLIED, the question is the run's decision of
    // request_user_decision, and the run waits BLOCKED (NEED_DECISION) for the answer; no work is taken.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    let question = "登录要支持哪些方式？\"OAuth\" 还是 密码\\短信";
    let team = world::Team::new(move |turn| {
        if turn.state == "CREATED" {
            let mut decided = world::steps("CREATED", "SPECIFYING");
            decided.push(world::ask("q-login", question));
            decided
        } else {
            Vec::new()
        }
    });
    let host = world.team_host(team.clone(), None).await?;
    let outcome = async {
        world.seed_spec()?;
        let run = world::start_run(&host, "s1", "实现登录功能").await?;
        world.staff(&run)?;
        world
            .until(
                &format!("SELECT state || '/' || blocked_reason FROM runs WHERE run_id = '{run}'"),
                "BLOCKED/NEED_DECISION",
                WAIT,
            )
            .await?;
        let turns = team.turns();
        let (first, messages) = turns.first().ok_or("no turn")?;
        assert_eq!((first.run_id.as_str(), first.state.as_str()), (run.as_str(), "CREATED"));
        let kinds: Vec<String> = messages
            .iter()
            .map(|m| {
                let p = &m["payload"];
                format!("{}:{}", p["kind"].as_str().unwrap_or("?"), p["to_state"].as_str().unwrap_or("-"))
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "transition_run:DISCOVERING",
                "transition_run:SPECIFYING",
                "request_user_decision:-"
            ]
        );
        for (m, i) in messages.iter().zip(0..) {
            assert_eq!(m["expected_revision"].as_i64(), Some(first.revision + i), "{m}");
            assert_eq!(m["causation_id"], json!(first.wake));
            let valid = hoh_core::schema::protocol().validate_def("Message", m)?;
            assert!(valid.is_ok(), "{m}: {valid:?}");
        }
        let decisions = world.rows(&format!(
            "SELECT json_extract(payload_json, '$.kind') AS kind, json_extract(payload_json, '$.to_state') AS \
             to_state, json_extract(payload_json, '$.question') AS question, state FROM decisions \
             WHERE run_id = '{run}' ORDER BY rowid"
        ))?;
        let applied: Vec<&Value> = decisions
            .iter()
            .filter(|d| d["kind"] == "transition_run" && d["state"] == "APPLIED")
            .map(|d| &d["to_state"])
            .collect();
        assert_eq!(applied, [&json!("DISCOVERING"), &json!("SPECIFYING")], "{decisions:?}");
        assert_eq!(
            decisions
                .iter()
                .filter(|d| d["kind"] == "request_user_decision")
                .map(|d| d["question"].clone())
                .collect::<Vec<_>>(),
            [json!(question)],
            "{decisions:?}"
        );
        assert!(team.refused().is_empty(), "{:?}", team.refused());
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_fake_developer_and_verifiers_run_a_round_through_the_product_executor() -> R {
    // The Developer works 2 s, writes 1 MiB of marked stderr noise and commits; QA runs every planned check and reports
    // FAIL in a COMPLETED result (CR-018), the Reviewer PASS: both reports are taken in, CURRENT, with those statuses,
    // in sessions of their own.
    let world = World::new()?;
    world.configure(&world::TEAM, "[budget]\nmax_iterations = 1\n")?;
    let marker = "SMOKE-NOISE-7f3c";
    world.developer(&world::Developer {
        seconds: 2.0,
        stderr_bytes: 1 << 20,
        stderr_marker: marker.to_owned(),
        ..world::Developer::default()
    })?;
    world.verifier(
        "qa",
        &world::Verifier {
            report: "FAIL".to_owned(),
            checks: vec![world::fixture_spec()[0].criteria[0].check_id()],
            ..world::Verifier::default()
        },
    )?;
    let started = Instant::now();
    let (host, run) = adopted_run(&world, "s1").await?;
    let outcome = async {
        let task = planned(&world, &run).await?;
        let attempt = format!("{task}/attempt-1");
        world
            .until(
                "SELECT group_concat(role || ':' || status || ':' || state, ',') FROM (SELECT role, status, state \
                 FROM independent_reports ORDER BY role)",
                "qa:FAIL:CURRENT,reviewer:PASS:CURRENT",
                WAIT,
            )
            .await
            .map_err(|e| {
                format!(
                    "{e}; attempts: {:?}; turns: {:?}",
                    world.rows("SELECT attempt_id, state, failure_reason FROM attempts"),
                    world.log("turns.jsonl")
                )
            })?;
        let round = Instant::now();
        assert_eq!(
            world.query(&format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'"))?,
            "SETTLED"
        );
        let calls = world.log("calls.jsonl")?;
        assert_eq!(calls.first().map(|c| c["attempt_id"].clone()), Some(json!(attempt)), "{calls:?}");
        let turns = world.log("turns.jsonl")?;
        let at = |event: &str| -> Option<i64> {
            turns
                .iter()
                .find(|t| t["attempt_id"] == json!(attempt) && t["event"] == event)
                .and_then(|t| t["at_ns"].as_i64())
        };
        let worked = at("end").zip(at("start")).map(|(e, s)| e - s).ok_or("no turn")?;
        assert!(worked >= 2_000_000_000, "{worked} ns");
        let stderr = std::fs::read(world::attempt_dir(&world.project(), &attempt).join("stderr.log"))?;
        assert!(stderr.len() >= 1 << 20, "{} bytes", stderr.len());
        assert!(String::from_utf8_lossy(&stderr).starts_with(marker));
        assert!(world.git(&["log", "--format=%s"])?.contains(&format!("fake work {attempt}")));
        let checks = world.log("checks.jsonl")?;
        assert_eq!(
            checks.iter().map(|c| (c["check"].clone(), c["exit"].clone())).collect::<Vec<_>>(),
            [(json!("check-ac-901-01"), json!(0))],
            "{checks:?}"
        );
        // Each report's scope is the in-scope criteria its prompt listed.
        for path in world.rows(
            "SELECT a.path FROM independent_reports r JOIN artifacts a ON a.artifact_id = r.report_artifact_id",
        )? {
            let report: Value = serde_json::from_slice(&std::fs::read(
                world.project().join(path["path"].as_str().ok_or("no path")?),
            )?)?;
            assert_eq!(report["scope"], json!(["AC-901-01"]), "{report}");
        }
        // Each verifier in a session of its own role, none of them the Developer's.
        let sessions = world.rows(
            "SELECT r.role, s.role AS session_role, s.native_id FROM independent_reports r \
             JOIN sessions s ON s.session_id = r.session_id ORDER BY r.role",
        )?;
        let developer = world.query(&format!(
            "SELECT s.native_id FROM attempts a JOIN sessions s ON s.session_id = a.session_id \
             WHERE a.attempt_id = '{attempt}'"
        ))?;
        for s in &sessions {
            assert_eq!(s["role"], s["session_role"], "{sessions:?}");
            assert_ne!(s["native_id"], json!(developer), "{sessions:?}");
        }
        eprintln!(
            "the round's reports after {:?} ({:?} since)",
            started.elapsed(),
            round.elapsed()
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_codex_developers_turn_fails_unauthorized() -> R {
    // The synthesis of hoh-agents' codex_adapter tests: turn/completed failed with codexErrorInfo unauthorized; the
    // attempt ends FAILED for BLOCKED_AUTH, and the fake did no work.
    let world = World::new()?;
    let mut team = world::TEAM.to_vec();
    team[0] = ("commander", "claude");
    team[2] = ("developer", "codex");
    world.configure(&team, "")?;
    world.unauthorized("codex", &["developer"])?;
    let (host, run) = adopted_run(&world, "s1").await?;
    let outcome = async {
        let task = planned(&world, &run).await?;
        world
            .until(
                &format!(
                    "SELECT state || '/' || coalesce(failure_reason, '') FROM attempts \
                     WHERE attempt_id = '{task}/attempt-1' AND state IN ('SETTLED', 'FAILED')"
                ),
                "FAILED/BLOCKED_AUTH",
                WAIT,
            )
            .await?;
        assert!(world.log("calls.jsonl")?.is_empty());
        let turns = world.log("turns.jsonl")?;
        assert!(
            turns
                .iter()
                .any(|t| t["agent"] == "codex-app-server" && t["failure"] == "unauthorized"),
            "{turns:?}"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_the_worlds_hoh_starts_has_the_worlds_environment_and_runs_its_fakes() -> R {
    // `hoh -p --detach` with no host running starts one (CR-010) with the world's environment only; it found the
    // registered fakes without a suite of its own, and its Commander turn is the fake's.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    world.register()?;
    world.init_store().await?;
    world.seed_spec()?;
    let started = Instant::now();
    let out = world.hoh(&["-p", "完成fixture目标", "--detach", "--output", "json"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let accepted: Value = serde_json::from_slice(&out.stdout)?;
    let run = accepted["run_id"].as_str().ok_or("no run_id")?.to_owned();
    let hosts = world.hosts()?;
    assert_eq!(hosts.len(), 1, "{hosts:?}");
    let environ = std::fs::read(format!("/proc/{}/environ", hosts[0]))?;
    // Besides what the client itself gives its host (`HOH_HOST_*`: why it was started without a scope of its own).
    let mut env: Vec<String> = environ
        .split(|b| *b == 0)
        .filter(|v| !v.is_empty() && !v.starts_with(b"HOH_HOST_"))
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .collect();
    env.sort();
    let mut want: Vec<String> = world
        .env()
        .iter()
        .map(|(k, v)| format!("{k}={}", v.to_string_lossy()))
        .collect();
    want.sort();
    assert_eq!(env, want, "the host has the world's environment only");
    world.wait(
        &format!("SELECT state FROM runs WHERE run_id = '{run}'"),
        "RUNNING",
        WAIT,
    )?;
    let turns = world.log("turns.jsonl")?;
    assert!(
        turns
            .iter()
            .any(|t| t["role"] == "commander" && t["agent"] == "codex-app-server"),
        "{turns:?}"
    );
    assert_eq!(
        world.query("SELECT count(*) FROM events WHERE kind LIKE 'compat.suite%'")?,
        "0",
        "the registered fakes needed no suite"
    );
    eprintln!("detached run RUNNING after {:?}", started.elapsed());
    world.stop();
    assert!(world.hosts()?.is_empty());
    assert!(world.processes().is_empty(), "{:?}", world.processes());
    Ok(())
}
