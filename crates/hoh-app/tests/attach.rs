//! `hoh -p --detach` and `hoh attach` through the product binary (REQ-005, docs/adr/ADR-021.md decisions 1-6):
//! TEST-005-02 (concurrent attaches start exactly one host), TEST-005-03 (an attach without a run id never picks one
//! of several; an unknown id is RUN_NOT_FOUND), TEST-005-05 (a StartRun that does not commit prints nothing), and the
//! supplementary detach tests of CR-010 decision 2 (no TEST id: AC-005-01 belongs to M2-T13). TEST-003-03 (M2-T13,
//! docs/adr/ADR-031.md decision 12) runs in the M2-T13 test world (`world`).

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use hoh_app::host::{Host, HostConfig};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use hoh_core::persistence::{FaultHooks, PersistError};
use hoh_core::staffing::{Kind, Verdict};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const HOH: &str = env!("CARGO_BIN_EXE_hoh");

/// How long one `hoh` command of these tests may take: a host start is at most 10 s (ADR-021 decision 2).
const LIMIT: Duration = Duration::from_secs(30);

/// How long test_005_02 waits, after its 10 s, for staffing to have judged every binding of its run for good: the
/// registry's looks at the fake agents ([`FAKE_AGENT`]) and the judgments end within the host's first second unloaded,
/// long before the 10 s end, so the wait ends at its first check; the margin is for load, and covers even a fake's
/// first look that timed out (a probe command's limit is 20 s) and its look again 30 s after it (docs/adr/ADR-030.md
/// decision 2).
const SETTLE: Duration = Duration::from_secs(60);

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

/// The agents of this file's hosts and doctors (TEST-005-02's fixture names a FakeAgent; M2T15R-200): a fake `codex`,
/// `claude` and `pi` of each [`Env`]'s own, first on every PATH its commands set, so an agent installed on this machine
/// is never the one looked at. Each answers every command at once with this one line and exit 0: a version, and a help
/// that offers nothing HoH drives an agent by, so a look finds it Unsupported, a permanent failure (docs/adr/ADR-030.md
/// decision 2): while its file does not change it is never probed again, and its login is never checked nor a compat
/// suite run for it (both only for an agent whose probe succeeded), so every binding of it is judged once for good:
/// BLOCKED, AGENT_UNAVAILABLE, `<agent>:unsupported` (decisions 3 and 4).
const FAKE_AGENT: &str = "#!/bin/sh\necho 'fake agent 0.0.0 (hoh attach tests)'\n";

/// A project directory with a short path (a socket path has at most 107 bytes), a home of its own and the fake agents.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> R<Env> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
        std::fs::create_dir_all(dir.path().join("p"))?;
        std::fs::create_dir_all(dir.path().join("home"))?;
        std::fs::create_dir_all(dir.path().join("agents"))?;
        // Written once, before any host: their files never change after (the stamps a look compares).
        for agent in ["codex", "claude", "pi"] {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o755)
                .open(dir.path().join("agents").join(agent))?
                .write_all(FAKE_AGENT.as_bytes())?;
        }
        Ok(Env { dir })
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    /// The PATH of this test's commands: its fake agents, then `rest`.
    fn path(&self, rest: &std::ffi::OsStr) -> std::ffi::OsString {
        let mut path = self.dir.path().join("agents").into_os_string();
        if !rest.is_empty() {
            path.push(":");
            path.push(rest);
        }
        path
    }

    /// [`Env::command`] with XDG_RUNTIME_DIR a directory of its own (0700), where no user manager answers: a test's
    /// `hoh` reaches neither this machine's HOME, its agents nor its runtime directory (systemd, D-Bus).
    fn isolated(&self, args: &[&str]) -> R<Command> {
        use std::os::unix::fs::PermissionsExt as _;
        let runtime = self.dir.path().join("run");
        std::fs::create_dir_all(&runtime)?;
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
        let mut command = self.command(args);
        command.env("XDG_RUNTIME_DIR", runtime);
        Ok(command)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(HOH);
        command
            .args(args)
            .current_dir(self.project())
            .env_clear()
            .env("HOME", self.dir.path().join("home"))
            .env(
                "PATH",
                self.path(&std::env::var_os("PATH").unwrap_or_default()),
            )
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
    // CR-010 decision 2 (REQ-005 behaviours 1, 2, 3): the supplementary detach evidence, no TEST id. A new project has
    // no approved specification: its first run is BLOCKED on its question runtime:no_spec in StartRun's own commit, the
    // 202 body's revision 1 (docs/adr/ADR-031.md decision 1).
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
    assert_eq!(accepted["revision"], 1);
    assert!(accepted["command_id"].is_string());
    let run_id = accepted["run_id"].as_str().ok_or("no run_id")?.to_owned();
    assert_eq!(
        query(
            &env.project(),
            &format!(
                "SELECT state || ' ' || blocked_reason || ' ' || revision FROM runs WHERE run_id = '{run_id}'"
            )
        )?,
        "BLOCKED NEED_DECISION 1",
        "printed only after the commit"
    );
    assert_eq!(
        query(
            &env.project(),
            &format!(
                "SELECT json_extract(payload_json, '$.reason') FROM events WHERE run_id = '{run_id}' \
                 AND kind = 'question.asked'"
            )
        )?,
        "no_spec"
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
        (Some(run_id.as_str()), Some("BLOCKED"))
    );
    assert_eq!(counts()?, before);
    assert_eq!(
        hosts_of(&env.project())?,
        vec![host],
        "attach started no other host"
    );
    // Once the project has an approved specification, a detached run adopts it in StartRun's commit: CREATED at
    // revision 0, the 202 body's revision 0 (nothing moved it in that commit), its events run.created then
    // spec.changed{cause: adopted}. From then on its Commander is woken, so the host may move it at any time after the
    // commit (these fake agents are never available: BLOCKED, COMMANDER_UNAVAILABLE); the commit's own events say what
    // it was at its revision 0.
    execute(
        &env.project(),
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);",
    )?;
    let out = env.hoh(&["-p", "完成fixture目标", "--detach", "--output", "json"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let accepted: Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(
        (&accepted["status"], &accepted["revision"]),
        (&json!("ACCEPTED"), &json!(0))
    );
    let adopted = accepted["run_id"].as_str().ok_or("no run_id")?;
    assert_eq!(
        query(
            &env.project(),
            &format!(
                "SELECT spec_id || ' ' || (SELECT group_concat(kind, ' ') FROM (SELECT kind FROM events \
                 WHERE run_id = r.run_id AND seq <= (SELECT seq FROM events WHERE run_id = r.run_id \
                 AND kind = 'spec.changed') ORDER BY seq)) || ' ' || (SELECT json_extract(payload_json, '$.cause') \
                 FROM events WHERE run_id = r.run_id AND kind = 'spec.changed') FROM runs r WHERE run_id = '{adopted}'"
            )
        )?,
        "s1 run.created spec.changed adopted"
    );
    assert_eq!(
        query(
            &env.project(),
            &format!(
                "SELECT count(*) FROM events WHERE run_id = '{adopted}' AND kind = 'question.asked'"
            )
        )?,
        "0"
    );
    assert_eq!(
        hosts_of(&env.project())?,
        vec![host],
        "the same host took it"
    );
    Ok(())
}

#[test]
fn test_005_02_concurrent_attaches_start_exactly_one_host_and_see_the_same_events() -> R {
    // AC-005-02: R PAUSED with nothing running, its host stopped normally (host_epoch N), the agents fakes (Env); three
    // attaches without a TTY start within 50 ms and stay 10 s.
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
    // Each client's lines are read as they come, so what the three received can be compared while all are attached.
    type Lines = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
    let mut clients = Vec::new();
    for _ in 0..3 {
        let mut client = env
            .command(&["attach", &run_id, "--output", "stream-json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = client.stdout.take().ok_or("no stdout")?;
        let lines = Lines::default();
        let reader = {
            let lines = lines.clone();
            std::thread::spawn(move || {
                use std::io::BufRead as _;
                for line in std::io::BufReader::new(stdout)
                    .lines()
                    .map_while(Result::ok)
                {
                    lines
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(line);
                }
            })
        };
        clients.push((client, lines, reader));
    }
    let seqs = |lines: &Lines| -> R<Vec<i64>> {
        Ok(lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|l| serde_json::from_str::<Value>(l).map(|e| e["seq"].as_i64().unwrap_or(-1)))
            .collect::<Result<_, _>>()?)
    };
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
    // R's events after run.created: staffing's judgments of its bindings (docs/adr/ADR-030.md decisions 1 and 5), each
    // committed with the binding's state in one transaction once the registry has looked at the binding's agent. The
    // agents are this test's fakes (FAKE_AGENT), each found Unsupported at a host's first look, normally well within the
    // 10 s: every binding BLOCKED with one team.binding_blocked, by this host or already by the one the detach started
    // (a first look that timed out under load is a waiting probe_failed, looked at again 30 s after it). A
    // binding ACTIVE with a session, or BLOCKED by no verdict that waits (decision 3), is judged for good on the premise
    // that its agent's answers are fixed, as the fakes' are (the same at every look, their files unchanged): its next
    // looks write nothing. An agent whose answers change (a probe that keeps failing, a login check that times out
    // after a clear "logged out") could leave a binding waiting or change a judgment after this wait (M2T15R-200).
    // Nothing else writes R's events while it is PAUSED with no attempt: its supervisor holds (ADR-025 decision 3) and
    // the paces and rounds take no PAUSED run. So the clients stay, the hosts still sampled, until every binding is
    // judged for good and each client has received every event of R in the database; they are disconnected then, and
    // whatever this finds.
    let waits: Vec<String> = [
        Verdict::NotLookedAt,
        Verdict::ProbeFailed,
        Verdict::CompatPending,
    ]
    .iter()
    .inspect(|v| assert_eq!(v.kind(), Kind::Waiting, "{v:?}"))
    .map(Verdict::code)
    .collect();
    // R's bindings not judged for good, and R's events, read in one statement.
    let run_now = || -> R<(Vec<String>, Vec<i64>)> {
        let now: Value = serde_json::from_str(&query(
            &env.project(),
            &format!(
                "SELECT json_object('bindings', (SELECT json_group_array(json_array(role, state, session_id, \
                 json(missing_json))) FROM role_bindings WHERE run_id = '{run_id}'), \
                 'seqs', (SELECT json_group_array(seq) FROM events WHERE run_id = '{run_id}'))"
            ),
        )?)?;
        let unjudged = now["bindings"]
            .as_array()
            .ok_or("no bindings")?
            .iter()
            .filter(|b| match b[1].as_str() {
                Some("ACTIVE") => b[2].is_null(),
                Some("BLOCKED") => b[3].as_array().is_none_or(|missing| {
                    missing.iter().any(|line| {
                        line.as_str()
                            .and_then(|l| l.split_once(':'))
                            .is_none_or(|(_, code)| waits.iter().any(|w| w == code))
                    })
                }),
                _ => true,
            })
            .map(Value::to_string)
            .collect();
        let mut seqs: Vec<i64> = serde_json::from_value(now["seqs"].clone())?;
        seqs.sort_unstable();
        Ok((unjudged, seqs))
    };
    let settled = (|| -> R {
        let end = Instant::now() + SETTLE;
        loop {
            let hosts = hosts_of(&env.project())?;
            seen.extend(hosts.iter().copied());
            if hosts.len() > 1 || seen.len() > 1 {
                return Err(format!("not one host: {hosts:?}, {seen:?}").into());
            }
            // The clients' first, then the database's: what a client received was committed before, so it is the
            // start of R's events read after it, unless the client missed one.
            let received = clients
                .iter()
                .map(|(_, lines, _)| seqs(lines))
                .collect::<R<Vec<_>>>()?;
            let (unjudged, committed) = run_now()?;
            if let Some(k) = received.iter().position(|r| !committed.starts_with(r)) {
                return Err(format!(
                    "client {k} received {:?}, not R's events in order: {committed:?}",
                    received[k]
                )
                .into());
            }
            if unjudged.is_empty() && received.iter().all(|r| *r == committed) {
                return Ok(());
            }
            if Instant::now() > end {
                return Err(format!(
                    "after {SETTLE:?}: bindings not judged for good {unjudged:?}, R's events {committed:?}, \
                     received {received:?}"
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    // The three disconnected together.
    for (client, _, _) in &clients {
        if let Some(pid) = i32::try_from(client.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
    }
    let mut sequences = Vec::new();
    for (client, lines, reader) in clients {
        let out = finish(client, LIMIT)?;
        assert_eq!(
            out.status.code(),
            Some(143),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        reader.join().map_err(|_| "a client's reader panicked")?;
        sequences.push(seqs(&lines)?);
    }
    settled?;
    assert!(!sequences[0].is_empty());
    // Each in strictly increasing order: no event twice, none out of order (M2T06R-52).
    for seqs in &sequences {
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    }
    assert!(
        sequences.iter().all(|s| s == &sequences[0]),
        "{sequences:?}"
    );
    // And they are R's events: every one received, none committed after the clients had all of them.
    assert_eq!(sequences[0], run_now()?.1, "{sequences:?}");
    // And the agents were the fakes: R's events begin with run.created and bind no session, the last
    // team.binding_blocked of each binding of R's team names its agent unsupported (AGENT_UNAVAILABLE), and each binding
    // is as that event left it. Before it a binding may have had a judgment that waited (a first look that timed out
    // under load: probe_failed) and the detach's host may have written other events of R while R was CREATED (a
    // supervisor's deferral, M2T15R-250); those are R's events all the same and the clients' equality above covers them.
    let known: Value = serde_json::from_str(&query(
        &env.project(),
        &format!(
            "SELECT json_object('events', (SELECT json_group_array(json_array(seq, kind, json(payload_json))) \
             FROM events WHERE run_id = '{run_id}'), 'bindings', (SELECT json_group_array(json_array(role, agent, \
             state, blocked_reason, json(missing_json), session_id)) FROM role_bindings WHERE run_id = '{run_id}'))"
        ),
    )?)?;
    let mut events: Vec<&Value> = known["events"]
        .as_array()
        .ok_or("no events")?
        .iter()
        .collect();
    events.sort_by_key(|e| e[0].as_i64());
    assert_eq!(
        events.first().and_then(|e| e[1].as_str()),
        Some("run.created"),
        "{events:?}"
    );
    assert!(
        events.iter().all(|e| e[1] != "team.session_bound"),
        "{events:?}"
    );
    let team = [
        ("commander", "codex"),
        ("cto", "claude"),
        ("developer", "claude"),
        ("qa", "pi"),
        ("researcher", "pi"),
        ("reviewer", "codex"),
    ];
    let sorted = |mut items: Vec<Value>| {
        items.sort_by_key(Value::to_string);
        items
    };
    // Each role's last team.binding_blocked, later ones replacing earlier ones in seq order.
    let mut last = std::collections::BTreeMap::new();
    for e in events.iter().filter(|e| e[1] == "team.binding_blocked") {
        let p = &e[2];
        last.insert(
            p["role"].to_string(),
            serde_json::json!([p["role"], p["agent"], p["reason"], p["missing"]]),
        );
    }
    let blocked = sorted(last.into_values().collect());
    let want = sorted(
        team.iter()
            .map(|(role, agent)| {
                serde_json::json!([
                    role,
                    agent,
                    "AGENT_UNAVAILABLE",
                    [format!("{agent}:unsupported")]
                ])
            })
            .collect(),
    );
    assert_eq!(blocked, want, "{events:?}");
    let bindings = sorted(known["bindings"].as_array().ok_or("no bindings")?.clone());
    let want = sorted(
        team.iter()
            .map(|(role, agent)| {
                serde_json::json!([
                    role,
                    agent,
                    "BLOCKED",
                    "AGENT_UNAVAILABLE",
                    [format!("{agent}:unsupported")],
                    null
                ])
            })
            .collect(),
    );
    assert_eq!(bindings, want);
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
    let mut command = env.isolated(&["attach"])?;
    let attach = tokio::task::spawn_blocking(move || -> Result<Output, String> {
        let child = command
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

/// TEST-003-03's runs, their ids as a host gives them (UUIDs), the same in their first 8 characters, so only a whole id
/// tells them apart: R1 RUNNING at revision 4, R2 PAUSED at revision 6.
const R1: &str = "c3d9a0b2-7e41-4f6a-9b8c-2d5e7f103a64";
const R2: &str = "c3d9a0b2-1f8e-4c37-a5d2-96b0e4c71f58";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_003_03_an_attach_without_a_tty_names_both_runs_and_subscribes_to_nothing() -> R {
    // AC-003-03: two attachable runs, R1 RUNNING and R2 PAUSED, neither with a Commander binding or a specification
    // (unsupervised: the stall detector does not look at them; R1 has no task, so its pace has nothing to do); their
    // host runs (in this process, its runner the world's scripted Team). `hoh attach </dev/null >out 2>err` with no
    // terminal.
    let world = world::World::new()?;
    world.init_store().await?;
    world.exec(&format!(
        "INSERT INTO runs(run_id, project_id, state, mode, revision, resume_state, created_at) VALUES
           ('{R1}', :p, 'RUNNING', 'AUTO', 4, NULL, '2026-11-15T00:00:00Z'),
           ('{R2}', :p, 'PAUSED', 'AUTO', 6, 'RUNNING', '2026-11-15T00:00:01Z');"
    ))?;
    let team = world::Team::new(|_| Vec::new());
    let host = world.team_host(team.clone(), None).await?;
    let started = Instant::now();
    let outcome = async {
        let runs = || world.rows("SELECT run_id, state, revision FROM runs ORDER BY run_id");
        let before = runs()?;
        let subscribers = host.subscribers();
        let opened = host.streams_opened();
        let (out, err) = (world.root().join("out"), world.root().join("err"));
        let child = world
            .command(&["attach"])
            .stdout(std::fs::File::create(&out)?)
            .stderr(std::fs::File::create(&err)?)
            .spawn()?;
        let attach =
            tokio::task::spawn_blocking(move || world::finish(child, LIMIT).map_err(|e| e.to_string()));
        // Sampled while it runs: it subscribes to nothing, not even for a moment.
        let mut most = subscribers;
        while !attach.is_finished() {
            most = most.max(host.subscribers());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            (most, host.subscribers(), host.streams_opened()),
            (subscribers, subscribers, opened),
            "a subscription by the attach"
        );
        let status = attach.await??.status;
        let said = std::fs::read_to_string(&err)?;
        // 64; out 0 bytes; err names both runs by their whole ids.
        assert_eq!(status.code(), Some(64), "{said}");
        assert_eq!(std::fs::metadata(&out)?.len(), 0);
        assert!(said.contains(R1) && said.contains(R2), "{said}");
        // Both runs' state and revision as before.
        assert_eq!(runs()?, before);
        assert_eq!(
            before,
            [
                serde_json::json!({"run_id": R2, "state": "PAUSED", "revision": 6}),
                serde_json::json!({"run_id": R1, "state": "RUNNING", "revision": 4}),
            ]
        );
        // The host's ticks of at least 2 s (ten) asked neither run anything: no STALLED re-trigger, no stalled question.
        tokio::time::sleep(Duration::from_secs(2).saturating_sub(started.elapsed())).await;
        let stalled = world.rows(&format!(
            "SELECT run_id, seq, kind FROM events WHERE run_id IN ('{R1}', '{R2}') AND \
             ((kind = 'supervisor.retrigger' AND json_extract(payload_json, '$.reason') = 'STALLED') \
              OR (kind = 'question.asked' AND json_extract(payload_json, '$.reason') = 'stalled'))"
        ))?;
        assert!(stalled.is_empty(), "{stalled:?}");
        assert_eq!(runs()?, before);
        // The host of this process was the one used: no other was started.
        assert!(world.hosts()?.is_empty(), "{:?}", world.hosts());
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome?;
    assert!(
        team.turns().is_empty() && team.refused().is_empty(),
        "{team:?}"
    );
    Ok(())
}

/// A `run.result` payload of run `run` in `state` at its own seq, as a SQL `json(...)` value.
fn result_sql(run: &str, state: &str, exit: u8, seq_sql: &str) -> String {
    format!(
        "json_object('run_id', '{run}', 'state', '{state}', 'exit_code', {exit}, 'blocked_reason', NULL, \
         'spec_revision', NULL, 'accepted', 0, 'total', 0, 'last_event_seq', {seq_sql}, 'cost_usd', NULL, \
         'cost_status', 'unknown', 'remaining_requirements', json('[]'))"
    )
}

/// The stream-json lines of an attach as (seq, replays).
fn lines_of(stdout: &[u8]) -> R<Vec<(i64, bool)>> {
    let mut seen = Vec::new();
    for line in String::from_utf8(stdout.to_vec())?.split_terminator('\n') {
        let event: Value = serde_json::from_str(line)?;
        hoh_core::schema::protocol()
            .validate_def("Event", &event)?
            .map_err(|v| format!("{v:?}: {line}"))?;
        let replays = match event.get("replay") {
            None => false,
            Some(v) if v == &Value::Bool(true) => true,
            Some(other) => return Err(format!("replay {other} in {line}").into()),
        };
        seen.push((event["seq"].as_i64().unwrap_or(-1), replays));
    }
    Ok(seen)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_to_a_run_that_ended_shows_every_event_of_it_before_it_exits() -> R {
    // M2T06R-03 (REQ-005 behaviour 3), docs/adr/ADR-031.md decision 12: R ended CANCELLED before the attach, its events
    // between another run's, its run.result its last. The follow replays all of R's events, in order, each marked a
    // replay (all are at or before the watermark), and exits with the result's code at the result, not after a wait.
    let env = Env::new()?;
    // A product host this test's commands would start is stopped however the test ends (M2T06R-62).
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        &format!(
            "INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
               ('R', :p, 'CANCELLED', 'AUTO', '2026-11-07T00:00:00Z'),
               ('Q', :p, 'RUNNING', 'AUTO', '2026-11-07T00:00:01Z');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               (:p, 'e1', 'R', 'run.created', '{{}}', '2026-11-07T00:00:00Z'),
               (:p, 'e2', 'Q', 'run.created', '{{}}', '2026-11-07T00:00:01Z'),
               (:p, 'e3', 'R', 'run.cancel_requested', '{{}}', '2026-11-07T00:00:02Z'),
               (:p, 'e4', 'R', 'cancel.reaped', '{{}}', '2026-11-07T00:00:03Z'),
               (:p, 'e5', 'Q', 'run.paused', '{{}}', '2026-11-07T00:00:04Z'),
               (:p, 'e6', 'R', 'cancel.workspace', '{{}}', '2026-11-07T00:00:05Z'),
               (:p, 'e7', 'R', 'run.cancelled', '{{}}', '2026-11-07T00:00:06Z');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) SELECT
               :p, 'e8', 'R', 'run.result', {}, '2026-11-07T00:00:07Z'
               FROM (SELECT max(seq) + 1 AS next FROM events);",
            result_sql("R", "CANCELLED", 5, "next")
        ),
    )?;
    let want: Vec<(i64, bool)> = query(
        &env.project(),
        "SELECT group_concat(seq, ' ') FROM (SELECT seq FROM events WHERE run_id = 'R' ORDER BY seq)",
    )?
    .split(' ')
    .map(|s| s.parse().map(|seq| (seq, true)))
    .collect::<Result<_, _>>()?;
    let started = Instant::now();
    let out = env.hoh(&["attach", "R", "--output", "stream-json"])?;
    assert_eq!(
        out.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(lines_of(&out.stdout)?, want);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "ended at the result, not by the quiet wait: {:?}",
        started.elapsed()
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_to_a_run_that_ended_with_no_result_ends_after_the_quiet_wait() -> R {
    // Decision 12: a run that ended before run results were written keeps the quiet fallback: after its replay and a
    // quiet wait, its state ends the follow (text: the run and its state on stdout).
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        "INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
           ('R', :p, 'CANCELLED', 'AUTO', '2026-11-07T00:00:00Z');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           (:p, 'e1', 'R', 'run.created', '{}', '2026-11-07T00:00:00Z'),
           (:p, 'e2', 'R', 'run.cancelled', '{}', '2026-11-07T00:00:01Z');",
    )?;
    let out = env.hoh(&["attach", "R"])?;
    assert_eq!(
        out.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout)?, "R CANCELLED\n");
    host.shutdown().await;
    Ok(())
}

/// POST `body` to the commands of the project's host over its socket; the response's status.
fn post(project: &Path, body: &Value) -> R<u16> {
    use std::io::{Read as _, Write as _};
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".hoh/host.json"))?)?;
    let mut stream =
        std::os::unix::net::UnixStream::connect(info["socket"].as_str().ok_or("no socket")?)?;
    let body = body.to_string();
    write!(
        stream,
        "POST /v1/commands HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nX-HoH-Token: {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        info["token"].as_str().ok_or("no token")?,
        body.len()
    )?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply.split(' ').nth(1).ok_or("no status")?.parse()?)
}

/// `kind` for run `run` at its revision now, command `id`; its status.
fn control(project: &Path, run: &str, kind: &str, id: &str) -> R<u16> {
    let revision: i64 = query(
        project,
        &format!("SELECT revision FROM runs WHERE run_id = '{run}'"),
    )?
    .parse()?;
    post(
        project,
        &serde_json::json!({"schema_version": "1.0", "command_id": id, "idempotency_key": id,
            "expected_revision": revision, "kind": kind, "run_id": run, "payload": {"reason": "test"}}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_ends_at_a_result_past_its_watermark_never_at_an_older_stop() -> R {
    // Decision 12 (ADR-021:87): a PAUSED run's old result, in the replay, does not end the follow: the run may go on.
    // Resumed and paused again, its new result (past W0) does, with 6. Lines up to W0 replay history; later ones not.
    // An INCOMPLETE run's old result does not end it either; a signal ends that follow, which sends nothing.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        &format!(
            "INSERT INTO runs(run_id, project_id, state, mode, revision, resume_state, created_at) VALUES
               ('P', :p, 'PAUSED', 'AUTO', 2, 'RUNNING', '2026-11-07T00:00:00Z'),
               ('I', :p, 'INCOMPLETE', 'AUTO', 4, 'RUNNING', '2026-11-07T00:00:01Z');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               (:p, 'p1', 'P', 'run.created', '{{}}', '2026-11-07T00:00:00Z'),
               (:p, 'i1', 'I', 'run.created', '{{}}', '2026-11-07T00:00:00Z');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) SELECT
               :p, 'p2', 'P', 'run.result', {}, '2026-11-07T00:00:01Z' FROM (SELECT max(seq) + 1 AS next FROM events);
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) SELECT
               :p, 'i2', 'I', 'run.result', {}, '2026-11-07T00:00:01Z' FROM (SELECT max(seq) + 1 AS next FROM events);",
            result_sql("P", "PAUSED", 6, "next"),
            result_sql("I", "INCOMPLETE", 4, "next")
        ),
    )?;
    let w0: i64 = query(&env.project(), "SELECT max(seq) FROM events")?.parse()?;
    let spawn = |run: &str| {
        env.command(&["attach", run, "--output", "stream-json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    let paused = spawn("P")?;
    let incomplete = spawn("I")?;
    let end = Instant::now() + LIMIT;
    while host.subscribers() < 2 && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut paused = paused;
    let mut incomplete = incomplete;
    assert!(
        paused.try_wait()?.is_none(),
        "an old PAUSED result ended the attach"
    );
    assert!(
        incomplete.try_wait()?.is_none(),
        "an old INCOMPLETE result ended the attach"
    );
    let project = env.project();
    let posted = tokio::task::block_in_place(|| -> R<(u16, u16)> {
        let resumed = control(&project, "P", "ResumeRun", "c-resume")?;
        let end = Instant::now() + LIMIT;
        while query(&project, "SELECT state FROM runs WHERE run_id = 'P'")? == "PAUSED"
            && Instant::now() < end
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok((resumed, control(&project, "P", "PauseRun", "c-pause")?))
    })?;
    assert_eq!(posted, (202, 202));
    let out = tokio::task::block_in_place(|| finish(paused, LIMIT))?;
    assert_eq!(
        out.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = lines_of(&out.stdout)?;
    let want: Vec<(i64, bool)> = query(
        &env.project(),
        "SELECT group_concat(seq, ' ') FROM (SELECT seq FROM events WHERE run_id = 'P' ORDER BY seq)",
    )?
    .split(' ')
    .map(|s| s.parse().map(|seq: i64| (seq, seq <= w0)))
    .collect::<Result<_, _>>()?;
    assert_eq!(lines, want);
    assert!(lines.last().is_some_and(|(seq, _)| *seq > w0));
    let pid = rustix::process::Pid::from_raw(i32::try_from(incomplete.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::INT)?;
    let out = tokio::task::block_in_place(|| finish(incomplete, LIMIT))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(130), "{stderr}");
    assert!(stderr.contains("hoh attach I"), "{stderr}");
    assert_eq!(
        query(
            &env.project(),
            "SELECT state || ' ' || revision || ' ' || (SELECT count(*) FROM commands WHERE run_id = 'I') FROM runs \
             WHERE run_id = 'I'"
        )?,
        "INCOMPLETE 4 0"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_in_json_prints_the_runs_result_as_of_now_and_exits_0() -> R {
    // Decision 12: one object, REQ-004 behaviour 1's keys and the label, revision, requirements left and cost; the
    // exit code null for a run at work and the state's for a stopped one; last_event_seq the watermark. A snapshot:
    // 0 whatever the state.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        "INSERT INTO runs(run_id, project_id, state, mode, revision, blocked_reason, created_at) VALUES
           ('W', :p, 'RUNNING', 'AUTO', 1, NULL, '2026-11-07T00:00:00Z'),
           ('B', :p, 'BLOCKED', 'AUTO', 3, 'BLOCKED_AUTH', '2026-11-07T00:00:01Z');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           (:p, 'w1', 'W', 'run.created', '{}', '2026-11-07T00:00:00Z');",
    )?;
    let watermark: i64 = query(&env.project(), "SELECT max(seq) FROM events")?.parse()?;
    for (run, state, exit, reason, label) in [
        ("W", "RUNNING", Value::Null, Value::Null, "RUNNING"),
        (
            "B",
            "BLOCKED",
            serde_json::json!(3),
            serde_json::json!("BLOCKED_AUTH"),
            "BLOCKED",
        ),
    ] {
        let out = env.hoh(&["attach", run, "--output", "json"])?;
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout)?;
        assert!(
            text.ends_with("}\n") && text.matches('\n').count() == 1,
            "{text:?}"
        );
        let shown: Value = serde_json::from_str(&text)?;
        let keys: Vec<&str> = shown
            .as_object()
            .ok_or("no object")?
            .keys()
            .map(String::as_str)
            .collect();
        let mut keys = keys;
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "accepted",
                "blocked_reason",
                "cost_status",
                "cost_usd",
                "exit_code",
                "label",
                "last_event_seq",
                "remaining_requirements",
                "revision",
                "run_id",
                "spec_revision",
                "state",
                "total"
            ]
        );
        assert_eq!(
            (
                &shown["run_id"],
                &shown["state"],
                &shown["exit_code"],
                &shown["blocked_reason"],
                &shown["label"]
            ),
            (
                &serde_json::json!(run),
                &serde_json::json!(state),
                &exit,
                &reason,
                &serde_json::json!(label)
            )
        );
        assert_eq!(
            (
                &shown["spec_revision"],
                &shown["accepted"],
                &shown["total"],
                &shown["last_event_seq"]
            ),
            (
                &Value::Null,
                &serde_json::json!(0),
                &serde_json::json!(0),
                &serde_json::json!(watermark)
            )
        );
    }
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_forwards_its_runs_native_stderr_to_stderr_only_and_a_full_stderr_never_holds_it()
-> R {
    // Decision 11: the attempts the run's events name (the event's attempt_id, from its payload) have their
    // `*stderr.log` forwarded to the client's stderr, never its stdout; with 10 MiB of it and a stderr nobody reads, the
    // follow still shows every event and exits at the result, the forwarding given at most its bound.
    let env = Env::new()?;
    let _daemons = Daemons(env.project());
    let host = Host::start(config(&env.project())).await?;
    execute(
        &env.project(),
        &format!(
            "INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES
               ('R', :p, 'CANCELLED', 'AUTO', '2026-11-07T00:00:00Z');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               (:p, 'e1', 'R', 'run.created', '{{}}', '2026-11-07T00:00:00Z'),
               (:p, 'e2', 'R', 'attempt.started', '{{\"attempt_id\": \"R/a1\"}}', '2026-11-07T00:00:01Z'),
               (:p, 'e3', 'R', 'run.cancelled', '{{}}', '2026-11-07T00:00:02Z');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) SELECT
               :p, 'e4', 'R', 'run.result', {}, '2026-11-07T00:00:03Z' FROM (SELECT max(seq) + 1 AS next FROM events);",
            result_sql("R", "CANCELLED", 5, "next")
        ),
    )?;
    let logs = hoh_app::host::product::attempt_logs(&env.project(), "R/a1");
    std::fs::create_dir_all(&logs)?;
    std::fs::write(logs.join("exec.stderr.log"), "NATIVE-MARKER one line\n")?;
    let out = env.hoh(&["attach", "R", "--output", "stream-json"])?;
    assert_eq!(
        out.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("NATIVE-MARKER one line"));
    let stdout = String::from_utf8(out.stdout)?;
    assert!(!stdout.contains("NATIVE-MARKER"));
    assert!(stdout.contains("\"attempt_id\":\"R/a1\""), "{stdout}");
    // 10 MiB of it, and a stderr pipe nobody reads until the client has exited.
    let noise = "NOISE-MARKER ".repeat(10 * 1024 * 1024 / 13 + 1);
    std::fs::write(logs.join("exec.stderr.log"), &noise)?;
    let mut child = env
        .command(&["attach", "R", "--output", "stream-json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let read = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > LIMIT {
            let _ = child.kill();
            return Err("the attach was held by its stderr".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(status.code(), Some(5));
    let stdout = read.join().map_err(|_| "the reader panicked")?;
    assert_eq!(lines_of(&stdout)?.len(), 4);
    let mut stderr = Vec::new();
    {
        use std::io::Read as _;
        child
            .stderr
            .take()
            .ok_or("no stderr")?
            .read_to_end(&mut stderr)?;
    }
    assert!(String::from_utf8_lossy(&stderr).contains("NOISE-MARKER"));
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

/// `env`'s `hoh` with the user's systemd manager reachable (XDG_RUNTIME_DIR, USER), PATH `env`'s fake agents and then
/// `path`.
fn with_manager(env: &Env, args: &[&str], path: &std::ffi::OsStr) -> Command {
    let mut command = env.command(args);
    for name in ["XDG_RUNTIME_DIR", "USER"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("PATH", env.path(path));
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
    let mut command = env.isolated(&["-p", "目标", "--detach", "--output", "json"])?;
    let out = tokio::task::spawn_blocking(move || -> Result<Output, String> {
        let child = command
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
