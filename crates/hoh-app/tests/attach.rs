//! `hoh -p --detach` and `hoh attach` through the product binary (REQ-005, docs/adr/ADR-021.md decisions 1-6):
//! TEST-005-02 (concurrent attaches start exactly one host), TEST-005-03 (an attach without a run id never picks one
//! of several; an unknown id is RUN_NOT_FOUND), TEST-005-05 (a StartRun that does not commit prints nothing), and the
//! supplementary detach tests of CR-010 decision 2 (no TEST id: AC-005-01 belongs to M2-T13).

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use hoh_core::persistence::{FaultHooks, PersistError};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const HOH: &str = env!("CARGO_BIN_EXE_hoh");

/// How long one `hoh` command of these tests may take: a host start is at most 10 s (ADR-021 decision 2).
const LIMIT: Duration = Duration::from_secs(30);

/// `child`'s output once it ends; past `limit` it is killed and this is an error, so a command that never ends (an
/// attach that follows a run it should not have picked) fails its test instead of hanging it.
fn finish(mut child: Child, limit: Duration) -> R<Output> {
    fn drain(
        pipe: Option<impl std::io::Read + Send + 'static>,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (sent, got) = std::sync::mpsc::channel();
        if let Some(mut pipe) = pipe {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                let _ = sent.send(bytes);
            });
        } else {
            let _ = sent.send(Vec::new());
        }
        got
    }
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let end = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            let stderr = stderr
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_default();
            return Err(format!(
                "still running after {limit:?}; stderr: {}",
                String::from_utf8_lossy(&stderr)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let pipe = |got: std::sync::mpsc::Receiver<Vec<u8>>| {
        got.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "a pipe stayed open after the command ended")
    };
    Ok(Output {
        status,
        stdout: pipe(stdout)?,
        stderr: pipe(stderr)?,
    })
}

fn probe() -> ProbeCommand {
    ProbeCommand {
        program: HOH.into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

fn config(project: &Path) -> HostConfig {
    let mut config = HostConfig::new(project);
    config.port = 0;
    config.store.wal_probe = Some(probe());
    config
}

/// A project directory with a short path (a socket path has at most 107 bytes) and a home of its own.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> R<Env> {
        let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
        std::fs::create_dir_all(dir.path().join("p"))?;
        std::fs::create_dir_all(dir.path().join("home"))?;
        Ok(Env { dir })
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(HOH);
        command
            .args(args)
            .current_dir(self.project())
            .env_clear()
            .env("HOME", self.dir.path().join("home"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::null());
        command
    }

    fn hoh(&self, args: &[&str]) -> R<Output> {
        let child = self
            .command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        finish(child, LIMIT)
    }
}

/// The `hoh __host <project>` processes of `project`, by their command lines; a copy a process forked and that has not
/// exec'd yet is none, and a failed listing is an error (`identity::running_with`; ADR-022 decision 10, v2).
fn hosts_of(project: &Path) -> R<Vec<u32>> {
    let wanted = format!("{HOH}\0__host\0{}\0", project.display());
    Ok(hoh_agents::identity::running_with(|c| {
        c == wanted.as_bytes()
    })?)
}

/// Stops the hosts the product started for a project when dropped, however the test ends: each checked by its
/// command line first, SIGTERM, then SIGKILL after 10 s.
struct Daemons(PathBuf);

impl Drop for Daemons {
    fn drop(&mut self) {
        let signal = |pid: u32, signal: rustix::process::Signal| {
            if let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, signal);
            }
        };
        // Cleanup only: a failed listing stops nothing and asserts nothing.
        for pid in hosts_of(&self.0).unwrap_or_default() {
            signal(pid, rustix::process::Signal::TERM);
        }
        let end = Instant::now() + Duration::from_secs(10);
        while hosts_of(&self.0).is_ok_and(|h| !h.is_empty()) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        for pid in hosts_of(&self.0).unwrap_or_default() {
            signal(pid, rustix::process::Signal::KILL);
        }
    }
}

fn python(db: &Path, script: &str, stdin: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn query(project: &Path, sql: &str) -> R<String> {
    Ok(python(
        &project.join(".hoh/state.db"),
        "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); print('' if r is None or r[0] is None else r[0])",
        sql,
    )?
    .trim()
    .to_owned())
}

fn execute(project: &Path, sql: &str) -> R {
    python(
        &project.join(".hoh/state.db"),
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); \
         p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
         c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit(); c.close()",
        sql,
    )
    .map(drop)
}

/// The session id and the controlling terminal of `pid` (`/proc/<pid>/stat` fields 6 and 7).
fn session_and_tty(pid: u32) -> R<(u32, i64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after = stat.rsplit_once(") ").ok_or("no stat")?.1;
    let fields: Vec<&str> = after.split(' ').collect();
    Ok((fields[3].parse()?, fields[4].parse()?))
}

#[test]
fn detach_starts_a_host_of_its_own_session_and_prints_one_object_after_the_commit() -> R {
    // CR-010 decision 2 (REQ-005 behaviours 1, 2, 3): the supplementary detach evidence, no TEST id.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let started = Instant::now();
    let out = env.hoh(&["-p", "完成fixture目标", "--detach", "--output", "json"])?;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout)?;
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    let accepted: Value = serde_json::from_str(&stdout)?;
    assert_eq!(accepted["status"], "ACCEPTED");
    assert_eq!(accepted["revision"], 0);
    assert!(accepted["command_id"].is_string());
    let run_id = accepted["run_id"].as_str().ok_or("no run_id")?.to_owned();
    assert_eq!(
        query(
            &env.project(),
            &format!("SELECT state FROM runs WHERE run_id = '{run_id}'")
        )?,
        "CREATED",
        "printed only after the commit"
    );
    // The host runs in a session of its own with no terminal.
    let [host] = hosts_of(&env.project())?[..] else {
        return Err("not exactly one host".into());
    };
    let (session, tty) = session_and_tty(host)?;
    assert_eq!((session, tty), (host, 0));
    // An attach adds no run, task or attempt.
    let counts = || {
        query(
            &env.project(),
            "SELECT (SELECT count(*) FROM runs) || ' ' || (SELECT count(*) FROM tasks) || ' ' || (SELECT count(*) FROM attempts)",
        )
    };
    let before = counts()?;
    let attached = env.hoh(&["attach", &run_id, "--output", "json"])?;
    assert_eq!(
        attached.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&attached.stderr)
    );
    let snapshot: Value = serde_json::from_slice(&attached.stdout)?;
    assert_eq!(
        (snapshot["run_id"].as_str(), snapshot["state"].as_str()),
        (Some(run_id.as_str()), Some("CREATED"))
    );
    assert_eq!(counts()?, before);
    assert_eq!(
        hosts_of(&env.project())?,
        vec![host],
        "attach started no other host"
    );
    Ok(())
}

#[test]
fn test_005_02_concurrent_attaches_start_exactly_one_host_and_see_the_same_events() -> R {
    // AC-005-02: R PAUSED with nothing running, its host stopped normally (host_epoch N); three attaches without a TTY
    // start within 50 ms and stay 10 s.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let out = env.hoh(&["-p", "x", "--detach", "--output", "json"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run: Value = serde_json::from_slice(&out.stdout)?;
    let run_id = run["run_id"].as_str().ok_or("no run_id")?.to_owned();
    drop(Daemons(env.project()));
    assert!(hosts_of(&env.project())?.is_empty(), "the host stopped");
    execute(
        &env.project(),
        &format!(
            "UPDATE runs SET state = 'PAUSED', resume_state = 'CREATED', revision = revision + 1 WHERE run_id = '{run_id}';"
        ),
    )?;
    let epoch: i64 = query(&env.project(), "SELECT host_epoch FROM projects")?.parse()?;
    let rows = || {
        query(
            &env.project(),
            "SELECT (SELECT count(*) FROM attempts) || ' ' || (SELECT count(*) FROM outbox)",
        )
    };
    let before = rows()?;
    let mut clients = Vec::new();
    for _ in 0..3 {
        clients.push(
            env.command(&["attach", &run_id, "--output", "stream-json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
    }
    // From the first ready on, exactly one host the whole time.
    let end = Instant::now() + Duration::from_secs(10);
    while query(&env.project(), "SELECT host_epoch FROM projects")? == epoch.to_string() {
        if Instant::now() > end {
            return Err("no host became ready".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let hold = Instant::now() + Duration::from_secs(10);
    let mut seen = std::collections::BTreeSet::new();
    while Instant::now() < hold {
        let hosts = hosts_of(&env.project())?;
        assert!(hosts.len() <= 1, "{hosts:?}");
        seen.extend(hosts);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(seen.len(), 1, "{seen:?}");
    let mut sequences = Vec::new();
    for client in clients {
        if let Some(pid) = i32::try_from(client.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        let out = finish(client, LIMIT)?;
        assert_eq!(
            out.status.code(),
            Some(143),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let seqs: Vec<i64> = String::from_utf8(out.stdout)?
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).map(|e| e["seq"].as_i64().unwrap_or(-1)))
            .collect::<Result<_, _>>()?;
        sequences.push(seqs);
    }
    assert!(!sequences[0].is_empty());
    // Each in strictly increasing order: no event twice, none out of order (M2T06R-52).
    for seqs in &sequences {
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    }
    assert!(
        sequences.iter().all(|s| s == &sequences[0]),
        "{sequences:?}"
    );
    assert_eq!(
        query(&env.project(), "SELECT host_epoch FROM projects")?,
        (epoch + 1).to_string()
    );
    assert_eq!(
        query(
            &env.project(),
            &format!("SELECT state FROM runs WHERE run_id = '{run_id}'")
        )?,
        "PAUSED"
    );
    assert_eq!(rows()?, before, "attaches add no attempt and no outbox row");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_005_03_an_attach_without_a_tty_lists_two_runs_and_an_unknown_id_is_run_not_found() -> R
{
    // AC-005-03: a host with two runs not ended; stdin and stdout are pipes.
    let env = Env::new()?;
    // A product host this test's commands would start is stopped however the test ends (M2T06R-62).
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        "INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
           ('R1', :p, 'RUNNING', 'AUTO', '2026-11-07T00:00:00Z'),
           ('R2', :p, 'PAUSED', 'AUTO', '2026-11-07T00:00:01Z');",
    )?;
    let runs = query(&env.project(), "SELECT count(*) FROM runs")?;
    let opened = host.streams_opened();
    let project = env.project();
    let attach = tokio::task::spawn_blocking(move || -> Result<Output, String> {
        let child = Command::new(HOH)
            .arg("attach")
            .current_dir(&project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        finish(child, LIMIT).map_err(|e| e.to_string())
    });
    // Sampled while it runs: it subscribes to nothing.
    let mut subscribed = 0;
    while !attach.is_finished() {
        subscribed = subscribed.max(host.subscribers());
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let first = attach.await??;
    assert_ne!(first.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(stderr.contains("R1") && stderr.contains("R2"), "{stderr}");
    assert!(first.stdout.is_empty());
    assert_eq!((subscribed, host.subscribers()), (0, 0));
    // Not even briefly (M2T06R-57).
    assert_eq!(host.streams_opened(), opened);
    let second = env.hoh(&["attach", "run-does-not-exist"])?;
    assert_ne!(second.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&second.stderr).contains("RUN_NOT_FOUND"));
    assert_eq!(query(&env.project(), "SELECT count(*) FROM runs")?, runs);
    assert!(
        hosts_of(&env.project())?.is_empty(),
        "the running host was used"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_to_a_run_that_ended_shows_every_event_of_it_before_it_exits() -> R {
    // M2T06R-03 (REQ-005 behaviour 3): R ended CANCELLED before the attach, its events between another run's. The
    // follow replays all of R's events, in order, and exits with the cancel's code only after R's ending event.
    let env = Env::new()?;
    // A product host this test's commands would start is stopped however the test ends (M2T06R-62).
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        "INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
           ('R', :p, 'CANCELLED', 'AUTO', '2026-11-07T00:00:00Z'),
           ('Q', :p, 'RUNNING', 'AUTO', '2026-11-07T00:00:01Z');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           (:p, 'e1', 'R', 'run.created', '{}', '2026-11-07T00:00:00Z'),
           (:p, 'e2', 'Q', 'run.created', '{}', '2026-11-07T00:00:01Z'),
           (:p, 'e3', 'R', 'run.cancel_requested', '{}', '2026-11-07T00:00:02Z'),
           (:p, 'e4', 'R', 'cancel.reaped', '{}', '2026-11-07T00:00:03Z'),
           (:p, 'e5', 'Q', 'run.paused', '{}', '2026-11-07T00:00:04Z'),
           (:p, 'e6', 'R', 'cancel.workspace', '{}', '2026-11-07T00:00:05Z'),
           (:p, 'e7', 'R', 'run.cancelled', '{}', '2026-11-07T00:00:06Z');",
    )?;
    let want: Vec<i64> = query(
        &env.project(),
        "SELECT group_concat(seq, ' ') FROM (SELECT seq FROM events WHERE run_id = 'R' ORDER BY seq)",
    )?
    .split(' ')
    .map(str::parse)
    .collect::<Result<_, _>>()?;
    let out = env.hoh(&["attach", "R", "--output", "stream-json"])?;
    assert_eq!(
        out.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen: Vec<i64> = String::from_utf8(out.stdout)?
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).map(|e| e["seq"].as_i64().unwrap_or(-1)))
        .collect::<Result<_, _>>()?;
    assert_eq!(seen, want);
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_without_a_run_id_looks_at_every_page_of_runs() -> R {
    // M2T06R-05 (REQ-005 failure mode 3): 501 runs, only the oldest and the newest not ended, a page holding 500. The
    // two are listed on stderr and none is picked.
    let env = Env::new()?;
    // A product host this test's commands would start is stopped however the test ends (M2T06R-62).
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        "INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
           ('R0', :p, 'PAUSED', 'AUTO', '2026-11-07T00:00:00Z');
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 499)
         INSERT INTO runs(run_id, project_id, state, mode, created_at)
           SELECT 'R' || i, :p, 'COMPLETED', 'AUTO', '2026-11-07T00:00:01Z' FROM n;
         INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
           ('R500', :p, 'PAUSED', 'AUTO', '2026-11-07T00:00:02Z');",
    )?;
    assert_eq!(query(&env.project(), "SELECT count(*) FROM runs")?, "501");
    let out = env.hoh(&["attach", "--output", "json"])?;
    assert_eq!(out.status.code(), Some(64));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let listed: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("  R"))
        .map(str::trim)
        .collect();
    assert_eq!(listed, ["R500  PAUSED", "R0  PAUSED"], "{stderr}");
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_to_an_id_that_names_no_run_is_run_not_found_whatever_its_characters() -> R {
    // M2T06R-54 (REQ-005 failure mode 2): R1 exists; ids that are R1 with URL syntax after it, or have a space, name no
    // run: RUN_NOT_FOUND, 64, nothing printed on stdout.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        "INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
           ('R1', :p, 'PAUSED', 'AUTO', '2026-11-07T00:00:00Z');",
    )?;
    for id in ["R1?x", "R1#x", "R1/..", "no such", "R1%3F"] {
        let out = env.hoh(&["attach", id, "--output", "json"])?;
        assert_eq!(out.status.code(), Some(64), "{id:?}");
        assert!(out.stdout.is_empty(), "{id:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("RUN_NOT_FOUND"), "{id:?}: {stderr}");
    }
    let out = env.hoh(&["attach", "R1", "--output", "json"])?;
    assert_eq!(out.status.code(), Some(0));
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_reports_how_the_running_host_tracks_attempts_not_what_a_start_would_try() -> R {
    // M2T06R-402 (docs/adr/ADR-021.md decision 8d, CR-011): a host runs in this process's cgroup, which is not
    // delegated here (a test run from a login session or CI); the user's systemd manager may well answer, so a check
    // before a start would expect a cgroup, but doctor reports the running host's own record: proc, with a warning.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    let record: Value = serde_json::from_str(&std::fs::read_to_string(
        env.project().join(".hoh/host.json"),
    )?)?;
    let mut command = env.command(&["doctor", "--output", "json"]);
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        command.env("XDG_RUNTIME_DIR", runtime);
    }
    let out = finish(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        Duration::from_secs(60),
    )?;
    let report: Value = serde_json::from_slice(&out.stdout)?;
    let reported = &report["runtime"]["host"];
    assert_eq!(reported["state"], "running", "{reported}");
    assert_eq!(
        reported["process_tracking"], record["process_tracking"],
        "{reported}"
    );
    // CR-012: the stated limits of the termination proof, in every mode.
    assert!(
        reported["process_tracking_limits"]
            .as_str()
            .is_some_and(|l| l.contains("CR-012")),
        "{reported}"
    );
    if record["process_tracking"] == "proc" {
        let warned = report["summary"]["warnings"].as_array().is_some_and(|w| {
            w.iter()
                .any(|w| w.as_str().is_some_and(|w| w.contains("tracking: proc")))
        });
        assert!(warned, "{}", report["summary"]);
        // M2T06R-557: the host's own finding, not a guess at how it was started.
        assert!(
            record["process_tracking_reason"].as_str().is_some_and(
                |why| !why.is_empty() && !why.contains("not started through systemd-run")
            ),
            "{record}"
        );
    }
    // M2T06R-555: the running host's detail, and the limits' two kinds.
    assert!(
        reported["detail"]
            .as_str()
            .is_some_and(|d| !d.contains("没有运行中的 host")),
        "{reported}"
    );
    let limits = reported["process_tracking_limits"]
        .as_str()
        .unwrap_or_default();
    assert!(
        limits.contains("早已运行的服务")
            && limits.contains("迁出 cgroup")
            && limits.contains("隐藏标记")
            && limits.contains("篡改 host 本身")
            && limits.contains("CR-013"),
        "{limits}"
    );
    // The text form says the same: the running host, its tracking and the limits (CR-012).
    let mut command = env.command(&["doctor"]);
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        command.env("XDG_RUNTIME_DIR", runtime);
    }
    let out = finish(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        Duration::from_secs(60),
    )?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mode = record["process_tracking"].as_str().ok_or("no tracking")?;
    assert!(
        text.contains(&format!("host    运行中，进程跟踪 {mode}")),
        "{text}"
    );
    assert!(
        text.contains("CR-012")
            && text.contains("早已运行的服务")
            && text.contains("隐藏标记")
            && text.contains("篡改 host 本身")
            && text.contains("CR-013"),
        "{text}"
    );
    host.shutdown().await;
    Ok(())
}

/// Whether a host the client starts here can run in a delegated scope (decision 2): the user's systemd manager answers,
/// the user lingers and `systemd-run` is on PATH.
fn scope_available() -> bool {
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return false;
    };
    let user = std::env::var("USER").unwrap_or_default();
    Path::new(&runtime).join("systemd/private").exists()
        && Path::new("/var/lib/systemd/linger").join(&user).exists()
        && std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .any(|d| d.join("systemd-run").is_file())
}

/// `env`'s `hoh` with the user's systemd manager reachable (XDG_RUNTIME_DIR, USER), PATH as given.
fn with_manager(env: &Env, args: &[&str], path: &std::ffi::OsStr) -> Command {
    let mut command = env.command(args);
    for name in ["XDG_RUNTIME_DIR", "USER"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("PATH", path);
    command
}

#[test]
fn a_host_the_client_starts_runs_in_a_delegated_scope_when_it_can_and_records_it() -> R {
    // M2T06R-454 (docs/adr/ADR-021.md decision 2, CR-011): `hoh -p --detach` starts the host through systemd-run when
    // the user's manager answers and the user lingers; the host is then in a scope of its own, delegated, and records
    // cgroup tracking. Where that cannot be had it is started directly and records the proc scan.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let path = std::env::var_os("PATH").unwrap_or_default();
    let out = finish(
        with_manager(&env, &["-p", "x", "--detach", "--output", "json"], &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        LIMIT,
    )?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let record: Value = serde_json::from_str(&std::fs::read_to_string(
        env.project().join(".hoh/host.json"),
    )?)?;
    let pid = record["pid"].as_u64().ok_or("no pid")?;
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    if scope_available() {
        assert_eq!(record["process_tracking"], "cgroup", "{record}");
        assert!(
            cgroup.contains("/run-") && cgroup.trim_end().ends_with(".scope"),
            "{cgroup}"
        );
    } else {
        assert_eq!(record["process_tracking"], "proc", "{record}");
    }
    Ok(())
}

#[test]
fn a_host_whose_scope_is_refused_is_started_directly_and_records_the_proc_scan() -> R {
    // M2T06R-454 (decision 2): systemd-run fails (here a stand-in that knows only --version); the client starts the
    // host directly, which records the degraded tracking; the detach still succeeds.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let bin = env.dir.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let fake = bin.join("systemd-run");
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$1\" = --version ] && echo 'systemd 259 (stand-in)' && exit 0\nexit 1\n",
    )?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    }
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))?;
    let out = finish(
        with_manager(&env, &["-p", "x", "--detach", "--output", "json"], &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        LIMIT,
    )?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let record: Value = serde_json::from_str(&std::fs::read_to_string(
        env.project().join(".hoh/host.json"),
    )?)?;
    assert_eq!(record["process_tracking"], "proc", "{record}");
    // CR12-03: the host records why it was started directly.
    assert!(
        record["process_tracking_reason"]
            .as_str()
            .is_some_and(|why| why.contains("systemd-run did not start it")),
        "{record}"
    );
    Ok(())
}

#[test]
fn a_host_is_not_started_in_the_users_manager_when_the_user_does_not_linger() -> R {
    // M2T06R-453 (decision 2): without linger the user's manager, and a host in one of its scopes, would stop after the
    // last session. Here a user name with no linger record: the host is started directly and records the proc scan;
    // doctor says why.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut command = with_manager(&env, &["-p", "x", "--detach", "--output", "json"], &path);
    command.env("USER", "hoh-test-no-such-user");
    let out = finish(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        LIMIT,
    )?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let record: Value = serde_json::from_str(&std::fs::read_to_string(
        env.project().join(".hoh/host.json"),
    )?)?;
    assert_eq!(record["process_tracking"], "proc", "{record}");
    assert!(
        record["process_tracking_reason"]
            .as_str()
            .is_some_and(|why| why.contains("does not linger")),
        "{record}"
    );
    // CR12-03: doctor, run while that host runs (as the user this test runs as, whose check before a start may well
    // pass), reports the running host's own reason.
    let mut doctor = with_manager(&env, &["doctor", "--output", "json"], &path);
    let out = finish(
        doctor
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        Duration::from_secs(60),
    )?;
    let report: Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(
        report["runtime"]["host"]["state"], "running",
        "{}",
        report["runtime"]["host"]
    );
    let said = report["summary"]["warnings"].as_array().is_some_and(|w| {
        w.iter()
            .any(|w| w.as_str().is_some_and(|w| w.contains("does not linger")))
    });
    assert!(said, "{}", report["summary"]);
    drop(_daemons);
    Ok(())
}

/// Whether the user's systemd manager answers here and the user lingers (decision 2's conditions before systemd-run).
fn manager_and_linger() -> bool {
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return false;
    };
    let user = std::env::var("USER").unwrap_or_default();
    !user.is_empty()
        && Path::new(&runtime).join("systemd/private").exists()
        && Path::new("/var/lib/systemd/linger").join(&user).is_file()
}

/// A directory with a stand-in `systemd-run` that never answers `--version` (it sleeps 45 s), first on PATH.
fn hanging_systemd_run(env: &Env) -> R<std::ffi::OsString> {
    let bin = env.dir.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let fake = bin.join("systemd-run");
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$1\" = --version ] && exec sleep 45\nexit 1\n",
    )?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(std::env::join_paths(std::iter::once(bin).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))?)
}

#[test]
fn doctor_bounds_the_systemd_run_version_probe_and_lists_it() -> R {
    // M2T06R-553 (REQ-006 behaviour 6, ADR-015 decisions 4 and 5): with no host, doctor's check before a start asks
    // `systemd-run --version`; one that never answers is ended at its limit, doctor ends in time, and the command is in
    // its probes.
    if !manager_and_linger() {
        // Visible, not a silent pass (M2T06R-656): straight to the stderr of the test process, which libtest's capture
        // of `eprintln!` would hide in a passing test's output (M2T06R-701).
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: no user systemd manager or no linger here"
        );
        return Ok(());
    }
    let env = Env::new()?;
    let path = hanging_systemd_run(&env)?;
    let started = Instant::now();
    let out = finish(
        with_manager(&env, &["doctor", "--output", "json"], &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        Duration::from_secs(60),
    )?;
    let took = started.elapsed();
    let report: Value = serde_json::from_slice(&out.stdout)?;
    assert!(took < Duration::from_secs(23), "doctor took {took:?}");
    // At most its own limit (2 s), ended there (M2T06R-652).
    let bounded = report["probes"].as_array().is_some_and(|p| {
        p.iter().any(|run| {
            run["argv"].as_array().is_some_and(|a| {
                a.first()
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.ends_with("/systemd-run"))
            }) && run["timed_out"] == true
                && run["duration_ms"].as_u64().is_some_and(|ms| ms <= 2_500)
        })
    });
    assert!(bounded, "{}", report["probes"]);
    let listed = report["probes"].as_array().is_some_and(|p| {
        p.iter().any(|run| {
            run["argv"].as_array().is_some_and(|a| {
                a.len() == 2
                    && a[0].as_str().is_some_and(|c| c.ends_with("/systemd-run"))
                    && a[1] == "--version"
            })
        })
    });
    assert!(listed, "{}", report["probes"]);
    assert_eq!(
        report["runtime"]["host"]["process_tracking"], "proc",
        "{}",
        report["runtime"]["host"]
    );
    Ok(())
}

#[test]
fn a_host_start_does_not_wait_on_a_systemd_run_that_does_not_answer() -> R {
    // M2T06R-553 (decision 2): the client's `systemd-run --version` is bounded; one that never answers gives a direct
    // start, recorded with why, within the detach's time.
    if !manager_and_linger() {
        // Visible, not a silent pass (M2T06R-656): straight to the stderr of the test process, which libtest's capture
        // of `eprintln!` would hide in a passing test's output (M2T06R-701).
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: no user systemd manager or no linger here"
        );
        return Ok(());
    }
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let path = hanging_systemd_run(&env)?;
    let started = Instant::now();
    let out = finish(
        with_manager(&env, &["-p", "x", "--detach", "--output", "json"], &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        LIMIT,
    )?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    let record: Value = serde_json::from_str(&std::fs::read_to_string(
        env.project().join(".hoh/host.json"),
    )?)?;
    assert_eq!(record["process_tracking"], "proc", "{record}");
    assert!(
        record["process_tracking_reason"]
            .as_str()
            .is_some_and(|why| why.contains("systemd-run --version")),
        "{record}"
    );
    Ok(())
}

#[test]
fn doctor_without_a_host_reports_what_a_start_would_have_and_warns_when_it_would_degrade() -> R {
    // M2T06R-454 (CR-011): no host runs. Without the user's manager the next host would be degraded: proc, with a
    // warning; with it (and lingering), cgroup is expected but not verified.
    let env = Env::new()?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let doctor = |command: &mut Command| -> R<Value> {
        let out = finish(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
            Duration::from_secs(60),
        )?;
        Ok(serde_json::from_slice(&out.stdout)?)
    };
    let mut bare = env.command(&["doctor", "--output", "json"]);
    let report = doctor(&mut bare)?;
    assert_eq!(
        report["runtime"]["host"]["process_tracking"], "proc",
        "{}",
        report["runtime"]["host"]
    );
    assert!(
        report["runtime"]["host"]["process_tracking_limits"]
            .as_str()
            .is_some_and(|l| l.contains("CR-012")),
        "{}",
        report["runtime"]["host"]
    );
    let warned = report["summary"]["warnings"].as_array().is_some_and(|w| {
        w.iter()
            .any(|w| w.as_str().is_some_and(|w| w.contains("tracking: proc")))
    });
    assert!(warned, "{}", report["summary"]);
    if scope_available() {
        let report = doctor(&mut with_manager(
            &env,
            &["doctor", "--output", "json"],
            &path,
        ))?;
        assert_eq!(
            report["runtime"]["host"]["process_tracking"], "unverified",
            "{}",
            report["runtime"]["host"]
        );
        // M2T06R-555: the check names every condition it found, linger included.
        assert!(
            report["runtime"]["host"]["process_tracking_detail"]
                .as_str()
                .is_some_and(|d| d.contains("linger")),
            "{}",
            report["runtime"]["host"]
        );
    }
    Ok(())
}

/// A StartRun's commit that fails (CR-006 F3: the test's fault hook; the product's does nothing).
#[derive(Debug)]
struct FullDisk;

impl FaultHooks for FullDisk {
    fn before_commit(&self) -> Result<(), PersistError> {
        Err(PersistError::Full(
            "injected: the file system is full".to_owned(),
        ))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_005_05_a_startrun_whose_commit_fails_prints_nothing_and_leaves_no_run() -> R {
    // AC-005-05: the host's StartRun commit returns an error.
    let env = Env::new()?;
    let mut config = config(&env.project());
    config.store.faults = std::sync::Arc::new(FullDisk);
    // A product host this test's commands would start is stopped however the test ends (M2T06R-62).
    let _daemons = Daemons(env.project());
    let host = Host::start(config).await?;
    let project = env.project();
    let out = tokio::task::spawn_blocking(move || -> Result<Output, String> {
        let child = Command::new(HOH)
            .args(["-p", "目标", "--detach", "--output", "json"])
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        finish(child, LIMIT).map_err(|e| e.to_string())
    })
    .await??;
    assert_ne!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DB_FULL"), "{stderr}");
    assert_eq!(query(&env.project(), "SELECT count(*) FROM runs")?, "0");
    // No host and no agent of this command: the running host took it, and it starts none.
    assert!(hosts_of(&env.project())?.is_empty());
    assert_eq!(query(&env.project(), "SELECT count(*) FROM attempts")?, "0");
    host.shutdown().await;
    Ok(())
}
