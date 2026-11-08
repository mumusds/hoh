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

/// The `hoh __host <project>` processes of `project`, by their command lines.
fn hosts_of(project: &Path) -> Vec<u32> {
    let wanted = format!("{HOH}\0__host\0{}\0", project.display());
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c) == wanted)
        })
        .collect()
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
        for pid in hosts_of(&self.0) {
            signal(pid, rustix::process::Signal::TERM);
        }
        let end = Instant::now() + Duration::from_secs(10);
        while !hosts_of(&self.0).is_empty() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        for pid in hosts_of(&self.0) {
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
    let [host] = hosts_of(&env.project())[..] else {
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
        hosts_of(&env.project()),
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
    assert!(hosts_of(&env.project()).is_empty(), "the host stopped");
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
        let hosts = hosts_of(&env.project());
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
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        "INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
           ('R1', :p, 'RUNNING', 'AUTO', '2026-11-07T00:00:00Z'),
           ('R2', :p, 'PAUSED', 'AUTO', '2026-11-07T00:00:01Z');",
    )?;
    let runs = query(&env.project(), "SELECT count(*) FROM runs")?;
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
    let second = env.hoh(&["attach", "run-does-not-exist"])?;
    assert_ne!(second.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&second.stderr).contains("RUN_NOT_FOUND"));
    assert_eq!(query(&env.project(), "SELECT count(*) FROM runs")?, runs);
    assert!(
        hosts_of(&env.project()).is_empty(),
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
    assert!(hosts_of(&env.project()).is_empty());
    assert_eq!(query(&env.project(), "SELECT count(*) FROM attempts")?, "0");
    host.shutdown().await;
    Ok(())
}
