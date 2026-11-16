//! The Workspace with a host (docs/adr/ADR-031.md decisions 12 and 13, slice S4's supporting tests; test plan
//! "顶栏的 baseline" and "配置" for bare `hoh`): list_runs' goal and accepted candidate, bare `hoh`'s start-up (an
//! invalid configuration 78 and a host that cannot start 69, both before the terminal changes; a signal while it waits
//! for its host), Enter that starts a run and attaches it, the `/` commands, `/resume` and attach's selector, the
//! status bar's event count and the top bar's baseline. The world's plain host has no runner: nothing works or wakes,
//! so a run moves only by the commands of the test. These are not the TEST-001-01/02 or TEST-005-01 acceptance tests
//! (slice S5's, check tui-pty).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use pty::{Pty, R};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::time::{Duration, Instant};
use world::World;

/// How long a test waits for the TUI or the host to get where it looks.
const WAIT: Duration = Duration::from_secs(20);

/// A git commit or tree of 40 hex digits made of `c`.
fn hex40(c: char) -> String {
    c.to_string().repeat(40)
}

/// The runs, candidates and events of the baseline tests: C1 and C2 COMPLETED with ACCEPTED candidates (C1's later
/// one accepted at a smaller seq than C2's), C3 COMPLETED with none accepted, R RUNNING with an ACCEPTED candidate
/// accepted last of all; C1 has a StartRun with its goal.
const BASELINE_WORLD: &str = "
INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES
  ('C1', :p, 'COMPLETED', 'AUTO', 3, '2026-11-01T00:00:00Z'),
  ('C2', :p, 'COMPLETED', 'AUTO', 3, '2026-11-02T00:00:00Z'),
  ('C3', :p, 'COMPLETED', 'AUTO', 3, '2026-11-03T00:00:00Z'),
  ('R', :p, 'RUNNING', 'AUTO', 2, '2026-11-04T00:00:00Z');
INSERT INTO commands(command_id, run_id, idempotency_key, kind, payload_json, payload_sha256, expected_revision,
  result_json, accepted_at) VALUES
  ('k1', 'C1', 'k1', 'StartRun', '{\"goal\":\"目标一\",\"mode\":\"AUTO\",\"interactive\":false}', printf('%064d', 1), 0,
   NULL, '2026-11-01T00:00:00Z');
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at) VALUES
  ('c1a', 'C1', <a>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'ACCEPTED', '2026-11-01T01:00:00Z'),
  ('c1b', 'C1', <b>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'ACCEPTED', '2026-11-01T02:00:00Z'),
  ('c1r', 'C1', <f>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'REJECTED', '2026-11-01T03:00:00Z'),
  ('c2a', 'C2', <c>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'ACCEPTED', '2026-11-02T01:00:00Z'),
  ('c3r', 'C3', <e>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'REJECTED', '2026-11-03T01:00:00Z'),
  ('ra', 'R', <d>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'ACCEPTED', '2026-11-04T01:00:00Z');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  (:p, 'e1', 'C1', 'candidate.accepted', '{\"candidate_id\":\"c1a\",\"join_id\":\"j1\"}', '2026-11-01T01:00:00Z'),
  (:p, 'e2', 'C1', 'candidate.accepted', '{\"candidate_id\":\"c1b\",\"join_id\":\"j2\"}', '2026-11-01T02:00:00Z'),
  (:p, 'e3', 'C2', 'candidate.accepted', '{\"candidate_id\":\"c2a\",\"join_id\":\"j3\"}', '2026-11-02T01:00:00Z'),
  (:p, 'e4', 'C1', 'candidate.accepted', '{\"candidate_id\":\"c1r\",\"join_id\":\"j4\"}', '2026-11-01T03:00:00Z'),
  (:p, 'e5', 'R', 'candidate.accepted', '{\"candidate_id\":\"ra\",\"join_id\":\"j5\"}', '2026-11-04T01:00:00Z');
";

/// [`BASELINE_WORLD`] in the world's store.
fn seed_baseline(world: &World) -> R {
    let mut sql = BASELINE_WORLD.to_owned();
    for (name, c) in [
        ("<a>", 'a'),
        ("<b>", 'b'),
        ("<c>", 'c'),
        ("<d>", 'd'),
        ("<e>", 'e'),
        ("<f>", 'f'),
        ("<t>", '7'),
    ] {
        sql = sql.replace(name, &format!("'{}'", hex40(c)));
    }
    world.exec(&sql)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_runs_gives_each_runs_goal_and_a_completed_runs_latest_accepted_candidate() -> R {
    // Decision 13 (the top bar's baseline): list_runs' items carry the run's goal (its StartRun's) and, for a
    // COMPLETED run only, `accepted` — its ACCEPTED candidate with the greatest `candidate.accepted` seq, or null.
    let world = World::new()?;
    let host = world.start_plain_host(None).await?;
    seed_baseline(&world)?;
    let seq = |event: &str| -> R<i64> {
        Ok(world
            .query(&format!(
                "SELECT seq FROM events WHERE event_id = '{event}'"
            ))?
            .parse()?)
    };
    let (status, page) = world::exchange(&host, "GET", "/v1/runs", None).await?;
    assert_eq!(status, 200, "{page}");
    let item = |run: &str| -> R<Value> {
        Ok(page["items"]
            .as_array()
            .ok_or("no items")?
            .iter()
            .find(|i| i["run_id"] == run)
            .ok_or(format!("no item of {run}: {page}"))?
            .clone())
    };
    assert_eq!(
        item("C1")?["accepted"],
        json!({"candidate_id": "c1b", "git_commit": hex40('b'), "seq": seq("e2")?}),
        "C1's ACCEPTED candidate accepted last; a REJECTED one's later event does not count"
    );
    assert_eq!(
        item("C2")?["accepted"],
        json!({"candidate_id": "c2a", "git_commit": hex40('c'), "seq": seq("e3")?})
    );
    let c3 = item("C3")?;
    assert_eq!(
        c3.get("accepted"),
        Some(&Value::Null),
        "a COMPLETED run with no ACCEPTED candidate: null"
    );
    let r = item("R")?;
    assert_eq!(
        r.get("accepted"),
        None,
        "a run that is not COMPLETED carries none, whatever its candidates: {r}"
    );
    assert_eq!(item("C1")?["goal"], "目标一");
    for run in ["C2", "C3", "R"] {
        assert_eq!(item(run)?.get("goal"), Some(&Value::Null), "{run}");
    }
    host.shutdown().await;
    Ok(())
}

/// `hoh` (bare) in the world's project, its stdin and stdout the PTY (its controlling terminal) and its stderr the
/// file `err`.
fn bare_hoh_with_stderr_in(world: &World, pty: &Pty, err: &std::path::Path) -> R<pty::Hoh> {
    world.spawn_pty(
        pty,
        OsStr::new("bash"),
        &[
            "-c",
            "exec \"$0\" 2>\"$1\"",
            world::HOH,
            err.to_str().ok_or("path")?,
        ],
        true,
    )
}

#[test]
fn bare_hoh_with_an_invalid_configuration_exits_78_before_the_terminal_or_a_host() -> R {
    // Decision 13 (start-up), test plan "配置": an invalid configuration is 78 before anything changes the terminal;
    // nothing on the terminal, the file and its key on stderr, no host and no database.
    let world = World::new()?;
    world.configure(&world::TEAM, "[project]\ndefault_mode = \"auto\"\n")?;
    let pty = Pty::new(30, 100)?;
    let before = pty.stty()?;
    let err = world.root().join("err");
    let mut hoh = bare_hoh_with_stderr_in(&world, &pty, &err)?;
    let status = pty::exit_within(&mut hoh, WAIT)?;
    pty.drained(WAIT)?;
    let stderr = std::fs::read_to_string(&err)?;
    assert_eq!(status.code(), Some(78), "{stderr}");
    assert!(
        pty.output().is_empty(),
        "{:?}",
        String::from_utf8_lossy(&pty.output())
    );
    assert_eq!(pty.stty()?, before, "terminal attributes");
    assert!(
        stderr.contains("config.toml") && stderr.contains("project.default_mode"),
        "{stderr}"
    );
    assert!(world.hosts()?.is_empty(), "a host was started");
    assert!(!world.db().exists());
    Ok(())
}

#[test]
fn bare_hoh_exits_69_without_the_tui_when_no_host_can_start() -> R {
    // Decision 13, REQ-001 failure mode 2: a host that cannot start (its `.hoh` is a file) is 69 with the host's
    // reason on stderr, and the TUI never opens.
    let world = World::new()?;
    std::fs::remove_dir_all(world.project().join(".hoh"))?;
    std::fs::write(world.project().join(".hoh"), b"not a directory")?;
    let pty = Pty::new(30, 100)?;
    let before = pty.stty()?;
    let err = world.root().join("err");
    let mut hoh = bare_hoh_with_stderr_in(&world, &pty, &err)?;
    let status = pty::exit_within(&mut hoh, WAIT)?;
    pty.drained(WAIT)?;
    let stderr = std::fs::read_to_string(&err)?;
    assert_eq!(status.code(), Some(69), "{stderr}");
    assert!(
        pty.output().is_empty(),
        "the TUI opened: {:?}",
        String::from_utf8_lossy(&pty.output())
    );
    assert_eq!(pty.stty()?, before, "terminal attributes");
    assert!(stderr.contains(".hoh"), "{stderr}");
    assert!(world.hosts()?.is_empty());
    Ok(())
}

/// Whether process `pid` has `dir` open (one of its descriptors names it).
fn holds_open(pid: u32, dir: &std::path::Path) -> R<bool> {
    let fds = match std::fs::read_dir(format!("/proc/{pid}/fd")) {
        Ok(fds) => fds,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    Ok(fds
        .flatten()
        .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|target| target == dir)))
}

#[test]
fn a_signal_while_bare_hoh_waits_for_its_host_ends_it_with_its_code_and_leaves_the_terminal() -> R {
    // Decision 13 (start-up): the host is ensured on a worker thread while the main thread looks at the signal flags
    // every 100 ms: SIGTERM 143, SIGINT 130, SIGHUP 129, at once, without touching the terminal. Here the host cannot
    // be had: the test holds the lock on the project directory that a client starting a host waits for (10 s).
    let world = World::new()?;
    let directory = std::fs::File::open(world.project())?;
    rustix::fs::flock(&directory, rustix::fs::FlockOperation::LockExclusive)?;
    for (signal, code) in [
        (rustix::process::Signal::TERM, 143),
        (rustix::process::Signal::INT, 130),
        (rustix::process::Signal::HUP, 129),
    ] {
        let pty = Pty::new(30, 100)?;
        let before = pty.stty()?;
        let err = world.root().join("err");
        let mut hoh = bare_hoh_with_stderr_in(&world, &pty, &err)?;
        // Waiting for the lock once it holds the project directory open (its signals were registered before).
        let end = Instant::now() + WAIT;
        while !holds_open(hoh.0.id(), &world.project())? {
            assert!(hoh.0.try_wait()?.is_none(), "{signal:?}: hoh did not wait");
            assert!(
                Instant::now() < end,
                "{signal:?}: hoh never opened the project"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let sent = Instant::now();
        pty::signal(&hoh, signal)?;
        let status = pty::exit_within(&mut hoh, Duration::from_secs(5))?;
        let took = sent.elapsed();
        pty.drained(WAIT)?;
        assert_eq!(
            status.code(),
            Some(code),
            "{signal:?}: {}",
            std::fs::read_to_string(&err)?
        );
        assert!(took < Duration::from_secs(2), "{signal:?}: took {took:?}");
        assert!(
            pty.output().is_empty(),
            "{signal:?}: the terminal was written: {:?}",
            String::from_utf8_lossy(&pty.output())
        );
        assert_eq!(pty.stty()?, before, "{signal:?}: terminal attributes");
    }
    assert!(world.hosts()?.is_empty());
    Ok(())
}

/// Bare `hoh` in a PTY of `rows` x `cols`, once its Workspace is drawn with the host connected.
fn open(world: &World, rows: u16, cols: u16, args: &[&str], drawn: &str) -> R<(Pty, pty::Hoh)> {
    let pty = Pty::new(rows, cols)?;
    let hoh = world.hoh_pty(&pty, args)?;
    pty.wait_for(drawn, 0, WAIT)?;
    Ok((pty, hoh))
}

/// Ctrl+C on an empty editor, then the first choice (脱离): the exit code.
fn detach(pty: &mut Pty, hoh: &mut pty::Hoh) -> R<Option<i32>> {
    let from = pty.output().len();
    pty.send(b"\x03")?;
    pty.wait_for("脱离", from, WAIT)?;
    pty.send(b"\r")?;
    Ok(pty::exit_within(hoh, WAIT)?.code())
}

/// The rows of `pty`'s screen (`rows` x `cols`) as a terminal shows them.
fn screen(pty: &Pty, rows: u16, cols: u16) -> R<Vec<String>> {
    Ok(pty::snapshot(&pty.output(), rows, cols)?
        .split('\n')
        .map(str::to_owned)
        .collect())
}

/// Wait until the screen shows `text` (as a terminal shows it now, not as it was once drawn).
fn wait_on_screen(pty: &Pty, rows: u16, cols: u16, text: &str) -> R<Vec<String>> {
    let end = Instant::now() + WAIT;
    loop {
        let shown = screen(pty, rows, cols)?;
        if shown.iter().any(|r| r.contains(text)) {
            return Ok(shown);
        }
        if Instant::now() > end {
            return Err(format!("{text:?} not on the screen: {shown:#?}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn enter_starts_a_run_and_attaches_it_and_a_slash_command_sends_nothing() -> R {
    // Decision 13 (Enter, unattached): text starting with `/` is the Workspace's own (`/resume`, else "M3 前未提供"),
    // never a StartRun (E11); any other text is one StartRun{goal, the top bar's mode, interactive}, after which the
    // Workspace is attached to the new run (its Ctrl+C choice offers 暂停 and 取消任务). Detaching sends nothing.
    let world = World::new()?;
    world.configure(&world::TEAM, "[project]\ndefault_mode = \"BUILD\"\n")?;
    let runtime = tokio::runtime::Runtime::new()?;
    let host = runtime.block_on(world.start_plain_host(None))?;
    // The host's own rows (its team from the configuration file, a ConfigureTeam) are there before the Workspace.
    let commands = || world.query("SELECT group_concat(kind, ' ') FROM commands");
    let before = commands()?;
    let (mut pty, mut hoh) = open(&world, 30, 100, &[], "host 已连接")?;
    pty.wait_for("模式 BUILD", 0, WAIT)?;
    let from = pty.output().len();
    pty.send(b"/plan x\r")?;
    pty.wait_for("M3 前未提供", from, WAIT)?;
    assert_eq!(commands()?, before);
    pty.send("你好 world\r".as_bytes())?;
    pty.wait_for("已附着", from, WAIT)?;
    let rows = world
        .rows("SELECT kind, run_id, payload_json FROM commands WHERE kind <> 'ConfigureTeam'")?;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["kind"], "StartRun");
    let payload: Value = serde_json::from_str(rows[0]["payload_json"].as_str().ok_or("payload")?)?;
    assert_eq!(
        payload,
        json!({"goal": "你好 world", "mode": "BUILD", "interactive": true})
    );
    let run = rows[0]["run_id"].as_str().ok_or("run_id")?.to_owned();
    wait_on_screen(&pty, 30, 100, &format!("run {run}"))?;
    let from = pty.output().len();
    pty.send(b"\x03")?;
    pty.wait_for("取消任务", from, WAIT)?;
    pty.send(b"\r")?;
    assert_eq!(pty::exit_within(&mut hoh, WAIT)?.code(), Some(0));
    assert_eq!(
        commands()?,
        format!("{before} StartRun"),
        "one StartRun; a detach sends nothing"
    );
    runtime.block_on(host.shutdown());
    Ok(())
}

/// Runs A (RUNNING), B (PAUSED, newer) and C (COMPLETED, newest), each with a goal.
const THREE_RUNS: &str = "
INSERT INTO runs(run_id, project_id, state, mode, revision, resume_state, created_at) VALUES
  ('aaaaaaaa-0000-4000-8000-000000000001', :p, 'RUNNING', 'AUTO', 1, NULL, '2026-11-01T00:00:00Z'),
  ('bbbbbbbb-0000-4000-8000-000000000002', :p, 'PAUSED', 'AUTO', 7, 'RUNNING', '2026-11-02T00:00:00Z'),
  ('cccccccc-0000-4000-8000-000000000003', :p, 'COMPLETED', 'AUTO', 4, NULL, '2026-11-03T00:00:00Z');
INSERT INTO commands(command_id, run_id, idempotency_key, kind, payload_json, payload_sha256, expected_revision,
  result_json, accepted_at) VALUES
  ('ka', 'aaaaaaaa-0000-4000-8000-000000000001', 'ka', 'StartRun', '{\"goal\":\"目标甲\",\"mode\":\"AUTO\",\"interactive\":true}',
   printf('%064d', 1), 0, NULL, '2026-11-01T00:00:00Z'),
  ('kb', 'bbbbbbbb-0000-4000-8000-000000000002', 'kb', 'StartRun', '{\"goal\":\"目标乙\",\"mode\":\"AUTO\",\"interactive\":true}',
   printf('%064d', 1), 0, NULL, '2026-11-02T00:00:00Z'),
  ('kc', 'cccccccc-0000-4000-8000-000000000003', 'kc', 'StartRun', '{\"goal\":\"目标丙\",\"mode\":\"AUTO\",\"interactive\":true}',
   printf('%064d', 1), 0, NULL, '2026-11-03T00:00:00Z');
";

#[test]
fn resume_chooses_among_the_runs_not_ended_and_attaches_the_one_chosen() -> R {
    // Decision 13: the Workspace lists the runs, those not ended first and the newest first in each group, one line
    // each (short id, label, goal); `/resume` opens the selector of the runs not ended, and the one chosen is
    // attached. Listing, choosing and attaching write no row (E11).
    let world = World::new()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let host = runtime.block_on(world.start_plain_host(None))?;
    world.exec(THREE_RUNS)?;
    let rows_before = world.query(
        "SELECT (SELECT count(*) FROM commands) || ' ' || (SELECT count(*) FROM input_queue) || ' ' || \
         (SELECT group_concat(state || revision) FROM runs)",
    )?;
    let (mut pty, mut hoh) = open(&world, 30, 100, &[], "host 已连接")?;
    let shown = wait_on_screen(&pty, 30, 100, "cccccccc")?;
    let at = |id: &str| shown.iter().position(|r| r.starts_with(id));
    let (b, a, c) = (at("bbbbbbbb"), at("aaaaaaaa"), at("cccccccc"));
    assert!(
        b.is_some() && b < a && a < c,
        "B, then A, then C: {shown:#?}"
    );
    let row = |i: Option<usize>| i.map(|i| shown[i].clone()).unwrap_or_default();
    assert_eq!(row(b), "bbbbbbbb  PAUSED  目标乙");
    assert_eq!(row(a), "aaaaaaaa  RUNNING  目标甲");
    assert_eq!(row(c), "cccccccc  COMPLETED  目标丙");
    let from = pty.output().len();
    pty.send(b"/resume\r")?;
    pty.wait_for("选择 Run", from, WAIT)?;
    let picking = wait_on_screen(&pty, 30, 100, "选择 Run")?;
    let listed: Vec<&String> = picking.iter().filter(|r| r.contains("目标")).collect();
    assert_eq!(listed.len(), 2, "only the runs not ended: {picking:#?}");
    assert!(listed[0].contains("bbbbbbbb") && listed[1].contains("aaaaaaaa"));
    pty.send(b"\x1b[B")?;
    std::thread::sleep(Duration::from_millis(100));
    pty.send(b"\r")?;
    wait_on_screen(&pty, 30, 100, "run aaaaaaaa-0000-4000-8000-000000000001")?;
    assert_eq!(detach(&mut pty, &mut hoh)?, Some(0));
    assert_eq!(
        world.query(
            "SELECT (SELECT count(*) FROM commands) || ' ' || (SELECT count(*) FROM input_queue) || ' ' || \
             (SELECT group_concat(state || revision) FROM runs)",
        )?,
        rows_before
    );
    runtime.block_on(host.shutdown());
    Ok(())
}

#[test]
fn attach_with_two_runs_on_a_terminal_chooses_one_and_esc_leaves_with_0() -> R {
    // Decision 12: `hoh attach` with no id, two runs not ended and a terminal opens the TUI's selector (the one of
    // `/resume`): Esc leaves with 0 and sends nothing; the run chosen is attached.
    let world = World::new()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let host = runtime.block_on(world.start_plain_host(None))?;
    world.exec(THREE_RUNS)?;
    let commands = || world.query("SELECT count(*) FROM commands");
    let before = commands()?;
    let (pty, mut hoh) = open(&world, 30, 100, &["attach"], "选择 Run")?;
    let picking = wait_on_screen(&pty, 30, 100, "选择 Run")?;
    assert!(
        !picking.iter().any(|r| r.contains("cccccccc")),
        "an ended run is not offered: {picking:#?}"
    );
    let mut pty = pty;
    pty.send(b"\x1b")?;
    assert_eq!(pty::exit_within(&mut hoh, WAIT)?.code(), Some(0));
    let (mut pty, mut hoh) = open(&world, 30, 100, &["attach"], "选择 Run")?;
    pty.send(b"\r")?;
    wait_on_screen(
        &pty,
        30,
        100,
        "run bbbbbbbb-0000-4000-8000-000000000002 PAUSED",
    )?;
    assert_eq!(detach(&mut pty, &mut hoh)?, Some(0));
    assert_eq!(commands()?, before);
    runtime.block_on(host.shutdown());
    Ok(())
}

#[test]
fn the_status_bar_counts_the_attached_runs_events_and_the_top_bar_shows_the_baseline() -> R {
    // Decision 13: attached, the status bar shows "事件 <events received> · #<latest seq>", each event counted once;
    // the top bar's baseline is the commit of the candidate accepted last among the COMPLETED runs' ACCEPTED ones
    // (C2's here; R's, accepted later, is not a COMPLETED run's), its first 12 hex digits, attached or not.
    let world = World::new()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let host = runtime.block_on(world.start_plain_host(None))?;
    seed_baseline(&world)?;
    world.exec(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           (:p, 'r1', 'R', 'task.created', '{}', '2026-11-04T02:00:00Z'),
           (:p, 'r2', 'R', 'task.ready', '{}', '2026-11-04T03:00:00Z');",
    )?;
    let (n, last) = (
        world.query("SELECT count(*) FROM events WHERE run_id = 'R'")?,
        world.query("SELECT max(seq) FROM events WHERE run_id = 'R'")?,
    );
    assert_eq!(n, "3");
    let baseline = format!("baseline {}", &hex40('c')[..12]);
    let (mut pty, mut hoh) = open(&world, 30, 120, &[], "host 已连接")?;
    wait_on_screen(&pty, 30, 120, &baseline)?;
    assert_eq!(detach(&mut pty, &mut hoh)?, Some(0));
    let (mut pty, mut hoh) = open(&world, 30, 120, &["attach", "R"], "run R")?;
    let shown = wait_on_screen(&pty, 30, 120, &format!("事件 {n} · #{last}"))?;
    assert!(shown[0].contains(&baseline), "{shown:#?}");
    assert_eq!(detach(&mut pty, &mut hoh)?, Some(0));
    runtime.block_on(host.shutdown());
    Ok(())
}
