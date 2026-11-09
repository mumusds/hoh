//! Pause and cancel apart from the scheduler, on an in-process host with real agent processes (REQ-043,
//! docs/adr/ADR-021.md decisions 7-9): TEST-043-01 (a pause waits for its safe point, a resume reconciles first),
//! TEST-043-02 (a cancel escalates, records the workspace and only then is CANCELLED) and TEST-043-04 (both with the
//! Commander down).

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig};
use hoh_core::decision::Context;
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn probe() -> ProbeCommand {
    ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

fn config(project: &Path) -> HostConfig {
    let mut config = HostConfig::new(project);
    config.port = 0;
    config.store.wal_probe = Some(probe());
    config.jitter_seed = Some(0x0043_0001);
    // The /proc mode's tests stay in it whatever cgroup they run in; the cgroup mode's run in a delegated scope
    // (M2T06R-452).
    if std::env::var_os(IN_SCOPE).is_none() {
        config.hooks = Arc::new(ProcOnly);
    }
    config
}

/// A host that does not use a delegated cgroup even if it is in one (CR-006: a test's hook): its attempts are tracked by
/// the /proc scan.
#[derive(Debug)]
struct ProcOnly;

impl hoh_app::host::HostHooks for ProcOnly {
    fn cgroups_allowed(&self) -> bool {
        false
    }
}

/// Start a host in a project another host of this process just let go. A process that spawns children (a parallel
/// test's agents, or the WAL self-test's probe) shares every open file with each child until it execs, so a lock just
/// released can still be held for that instant: a refusal (69) is tried again for a moment (as tests/host.rs does).
async fn start_when_free(
    config: hoh_app::host::HostConfig,
) -> Result<hoh_app::host::Host, hoh_app::host::StartError> {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match hoh_app::host::Host::start(config.clone()).await {
            Err(e) if e.exit_code == 69 && std::time::Instant::now() < end => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

fn python(db: &Path, script: &str, stdin: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
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

fn db(project: &Path) -> PathBuf {
    project.join(".hoh/state.db")
}

fn seed(project: &Path, sql: &str) -> R {
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); \
                  p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
                  c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit(); c.close()";
    python(&db(project), script, sql).map(drop)
}

fn query(project: &Path, sql: &str) -> R<String> {
    Ok(python(
        &db(project),
        "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); print('' if r is None or r[0] is None else r[0])",
        sql,
    )?
    .trim()
    .to_owned())
}

/// Wait until `sql` gives `want`, or fail after `limit`.
async fn until(project: &Path, sql: &str, want: &str, limit: Duration) -> R {
    let end = Instant::now() + limit;
    loop {
        let got = query(project, sql)?;
        if got == want {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!("{sql}: {got:?}, not {want:?} after {limit:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// POST a command to the host over its TCP port; the status and the JSON body.
async fn command(host: &Host, body: &Value) -> R<(u16, Value)> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let body = body.to_string();
    let request = format!(
        "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-HoH-Token: {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
        port = host.port,
        token = host.token,
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8(raw)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    let status: u16 = head.split(' ').nth(1).ok_or("no status")?.parse()?;
    Ok((status, serde_json::from_str(body)?))
}

/// GET `path` of the host over its TCP port; the status and the JSON body.
async fn get(host: &Host, path: &str) -> R<(u16, Value)> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-HoH-Token: {token}\r\n\r\n",
        port = host.port,
        token = host.token,
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8(raw)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    let status: u16 = head.split(' ').nth(1).ok_or("no status")?.parse()?;
    Ok((status, serde_json::from_str(body)?))
}

/// A control command for run R at its current revision.
fn control(project: &Path, kind: &str, key: &str) -> R<Value> {
    let revision: i64 = query(project, "SELECT revision FROM runs WHERE run_id = 'R'")?.parse()?;
    Ok(json!({
        "schema_version": "1.0", "command_id": format!("c-{key}"), "idempotency_key": key,
        "expected_revision": revision, "kind": kind, "run_id": "R", "payload": {"reason": key},
    }))
}

fn context() -> Context {
    let caps = ["structured_events", "command_exec"].map(str::to_owned);
    Context {
        required_capabilities: [("developer".to_owned(), caps.iter().cloned().collect())].into(),
        ..Context::default()
    }
}

/// Run R RUNNING with writer tasks T1, T2, T3 READY (one writer slot: T1 runs first, the others wait), a developer
/// bound to a session with the developer's capabilities, and candidate C1 VERIFYING.
const WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', :p, 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's1', 'READY', 1, 'developer', 1, 1, 1),
  ('T2', 'R', 's1', 'READY', 1, 'developer', 1, 2, 1),
  ('T3', 'R', 's1', 'READY', 1, 'developer', 1, 3, 1);
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at) VALUES
  ('C1', 'R', printf('%040d', 1), printf('%040d', 2), printf('%064d', 3), printf('%064d', 4), printf('%064d', 5),
   '/repo', 'VERIFYING', '2026-11-07T00:00:00Z');
";

/// Runs each task's script as an agent, the leader of its own process group held at its start gate (as the
/// substrate starts agents). It records each native cancel and each call for the Commander, which is down: a
/// delivery or a start for role `commander` fails.
#[derive(Debug)]
struct Agents {
    scripts: BTreeMap<String, String>,
    leaders: Arc<Mutex<Vec<u32>>>,
    native: Arc<Mutex<Vec<String>>>,
    commander: Arc<AtomicUsize>,
}

impl Agents {
    fn new(scripts: &[(&str, &str)]) -> Agents {
        Agents {
            scripts: scripts
                .iter()
                .map(|(t, s)| ((*t).to_owned(), (*s).to_owned()))
                .collect(),
            leaders: Arc::new(Mutex::new(Vec::new())),
            native: Arc::new(Mutex::new(Vec::new())),
            commander: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl hoh_app::host::Executor for Agents {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        if work.role == "commander" {
            self.commander.fetch_add(1, Ordering::SeqCst);
            let _ = answer.send(Err("the Commander is down".to_owned()));
        } else {
            let _ = answer.send(Ok(()));
        }
        ack
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        if work.role == "commander" {
            self.commander.fetch_add(1, Ordering::SeqCst);
            return Err("the Commander is down".to_owned().into());
        }
        let script = self
            .scripts
            .get(&work.task_id)
            .cloned()
            .unwrap_or_else(|| "sleep 60".to_owned());
        let mut child = Command::new("sh")
            .args([
                "-c",
                &format!("read -r go; [ \"$go\" = go ] || exit 0; {script}"),
            ])
            .process_group(0)
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        if let Ok(mut leaders) = self.leaders.lock() {
            leaders.push(pid);
        }
        let stdin = child.stdin.take();
        let gate = hoh_app::host::Gate::new(move || {
            use std::io::Write as _;
            stdin
                .ok_or("no stdin")?
                .write_all(b"go\n")
                .map_err(|e| e.to_string())
        });
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended: reaping(child),
        })
    }

    fn cancel(&self, work: &hoh_app::host::Work) {
        // The agent's native interrupt: these agents ignore it.
        if let Ok(mut native) = self.native.lock() {
            native.push(work.attempt_id.clone());
        }
    }
}

/// The executor's end of an agent's process, in the substrate's order: the leader's exit seen without reaping it, a
/// SIGKILL to its group while the zombie holds the group's number, the leader reaped, then up to 5 s for the group
/// to be empty.
fn reaping(
    mut child: std::process::Child,
) -> tokio::sync::oneshot::Receiver<hoh_app::host::Outcome> {
    let (end, ended) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let pid = child.id();
        let raw = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw);
        if let Some(raw) = raw {
            let _ = rustix::process::waitid(
                rustix::process::WaitId::Pid(raw),
                rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOWAIT,
            );
            let _ = rustix::process::kill_process_group(raw, rustix::process::Signal::KILL);
        }
        let status = child.wait();
        let until = Instant::now() + Duration::from_secs(5);
        let mut group_gone = hoh_agents::identity::group_empty(pid);
        while !group_gone && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
            group_gone = hoh_agents::identity::group_empty(pid);
        }
        let _ = end.send(hoh_app::host::Outcome {
            success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
            detail: format!("{status:?}"),
            group_gone,
            class: hoh_core::retry::FailureClass::Transient { retry_after: None },
            observed: Default::default(),
            unknown_effect: false,
        });
    });
    ended
}

/// Kills the agents' groups when dropped, however the test ends.
struct Leaders(Arc<Mutex<Vec<u32>>>);

impl Drop for Leaders {
    fn drop(&mut self) {
        for pid in self.0.lock().map(|l| l.clone()).unwrap_or_default() {
            if let Some(pgid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
            }
        }
    }
}

/// A project with a host database (made by a first start) and the world seeded; the host started with `agents`.
async fn host(dir: &Path, agents: &Arc<Agents>) -> R<(PathBuf, Host)> {
    let project = dir.join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let mut config = config(&project);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: agents.clone(),
        context: context(),
    });
    Ok((project.clone(), start_when_free(config).await?))
}

fn seq(project: &Path, sql: &str) -> R<i64> {
    let text = query(project, sql)?;
    text.parse()
        .map_err(|e| format!("{sql}: {text:?}: {e}").into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_043_01_a_pause_waits_for_t1_to_end_dispatches_nothing_and_a_resume_reconciles_before_t2()
-> R {
    // AC-043-01: T1 runs 8 s; PauseRun at 2 s, ResumeRun at 12 s.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let agents = Arc::new(Agents::new(&[
        ("T1", "sleep 8"),
        ("T2", "sleep 30"),
        ("T3", "sleep 30"),
    ]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (status, _) = command(&host, &control(&project, "PauseRun", "pause")?).await?;
    assert_eq!(status, 202);
    assert_eq!(
        query(
            &project,
            "SELECT state || ' ' || resume_state FROM runs WHERE run_id = 'R'"
        )?,
        "PAUSE_REQUESTED RUNNING"
    );
    let dispatched = || query(&project, "SELECT count(*) FROM attempts");
    assert_eq!(dispatched()?, "1");
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "PAUSED",
        Duration::from_secs(15),
    )
    .await?;
    // T1 was not interrupted: it ran its 8 s and SETTLED, and only then was the run PAUSED.
    assert_eq!(
        query(&project, "SELECT state FROM attempts WHERE task_id = 'T1'")?,
        "SETTLED"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        seq(
            &project,
            "SELECT count(*) FROM events WHERE kind LIKE 'cancel.%'"
        )?,
        0,
        "no signal went"
    );
    // Nor its native interrupt (M2T06R-50: these agents ignore one, so only the record shows it).
    let interrupted = || agents.native.lock().map(|n| n.clone()).unwrap_or_default();
    assert_eq!(interrupted(), Vec::<String>::new(), "T1 was interrupted");
    tokio::time::sleep(Duration::from_secs(12).saturating_sub(started.elapsed())).await;
    assert_eq!(dispatched()?, "1", "no dispatch from 2 s to 12 s");
    assert_eq!(interrupted(), Vec::<String>::new(), "T1 was interrupted");
    let (status, _) = command(&host, &control(&project, "ResumeRun", "resume")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT count(*) FROM attempts WHERE task_id = 'T2'",
        "1",
        Duration::from_secs(10),
    )
    .await?;
    let reconciled = seq(
        &project,
        "SELECT seq FROM events WHERE kind = 'reconcile.run'",
    )?;
    let t2 = seq(
        &project,
        "SELECT event_seq FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
         AND json_extract(payload_json, '$.task_id') = 'T2'",
    )?;
    assert!(
        reconciled < t2,
        "reconcile {reconciled}, T2's dispatch {t2}"
    );
    assert_eq!(
        query(
            &project,
            "SELECT count(*) FROM attempts WHERE task_id = 'T1'"
        )?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

/// The agent of TEST-043-02: a child and a grandchild in its group; the leader itself ignores SIGTERM and lives on
/// (its own sleeps, also in the group, die of it) until SIGKILL.
const STUBBORN: &str = "trap '' TERM; sh -c 'sleep 60 & wait' & while :; do sleep 1; done";

fn git(project: &Path, args: &[&str]) -> R<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(project)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_043_02_a_cancel_escalates_records_the_workspace_and_only_then_is_cancelled() -> R {
    // AC-043-02: the agent's child and grandchild share its group and one of them ignores SIGTERM; the workspace has an
    // uncommitted change and an untracked file.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let agents = Arc::new(Agents::new(&[("T1", STUBBORN)]));
    let _leaders = Leaders(agents.leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    git(&project, &["init", "-q"])?;
    std::fs::write(project.join("a.txt"), "one\n")?;
    std::fs::write(project.join(".gitignore"), ".hoh/\n")?;
    git(&project, &["add", "a.txt", ".gitignore"])?;
    git(&project, &["commit", "-q", "-m", "first"])?;
    std::fs::write(project.join("a.txt"), "two\n")?;
    std::fs::write(project.join("untracked.txt"), "new\n")?;
    let (project, host) = host(dir.path(), &agents).await?;
    let before = (
        git(&project, &["status", "--porcelain", "-uall"])?,
        git(&project, &["rev-parse", "HEAD"])?,
        std::fs::read(project.join(".git/index"))?,
    );
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let leader: u32 = query(&project, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    let asked = Instant::now();
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    assert_eq!(
        query(&project, "SELECT state FROM runs WHERE run_id = 'R'")?,
        "CANCEL_REQUESTED"
    );
    // Shown as CANCELLING by getRun and listRuns (REQ-043 behaviour 3, M2T06R-59).
    let (status, run) = get(&host, "/v1/runs/R").await?;
    assert_eq!(
        (
            status,
            run["state"].as_str(),
            run["projection"]["label"].as_str()
        ),
        (200, Some("CANCEL_REQUESTED"), Some("CANCELLING"))
    );
    let (status, page) = get(&host, "/v1/runs").await?;
    assert_eq!(
        (
            status,
            page["items"][0]["run_id"].as_str(),
            page["items"][0]["label"].as_str()
        ),
        (200, Some("R"), Some("CANCELLING"))
    );
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(30),
    )
    .await?;
    let took = asked.elapsed();
    assert!(took <= Duration::from_secs(17), "{took:?}");
    // In order: requested, the native interrupt, SIGTERM, SIGKILL, the group reaped, the workspace, CANCELLED.
    let at = |kind: &str| {
        seq(
            &project,
            &format!("SELECT min(seq) FROM events WHERE kind = '{kind}'"),
        )
    };
    let order = [
        "run.cancel_requested",
        "cancel.native_cancel",
        "cancel.sigterm",
        "cancel.sigkill",
        "cancel.reaped",
        "cancel.workspace",
        "run.cancelled",
    ]
    .map(|k| at(k).map(|s| (s, k)));
    let order: Vec<(i64, &str)> = order.into_iter().collect::<Result<_, _>>()?;
    assert!(order.windows(2).all(|w| w[0].0 < w[1].0), "{order:?}");
    // Nothing of the group is left, zombies included, and the agent's own interrupt was asked first.
    assert!(hoh_agents::identity::group_empty(leader));
    assert_eq!(agents.native.lock().map(|n| n.len()).unwrap_or(0), 1);
    // The workspace is as it was: nothing reset, cleaned or removed; its record names it.
    let after = (
        git(&project, &["status", "--porcelain", "-uall"])?,
        git(&project, &["rev-parse", "HEAD"])?,
        std::fs::read(project.join(".git/index"))?,
    );
    assert_eq!(after, before);
    let record: Value = serde_json::from_str(&query(
        &project,
        "SELECT payload_json FROM events WHERE kind = 'cancel.workspace'",
    )?)?;
    assert_eq!(record["head"], json!(before.1.trim()));
    assert!(
        record["diff_sha256"]
            .as_str()
            .is_some_and(|h| h.len() == 64),
        "{record}"
    );
    assert_eq!(
        record["logs"][0]["attempt_id"],
        json!(query(&project, "SELECT attempt_id FROM attempts")?)
    );
    // The unfinished candidate is not marked verified; the tasks are CANCELLED.
    assert_eq!(
        query(
            &project,
            "SELECT state FROM candidates WHERE candidate_id = 'C1'"
        )?,
        "VERIFYING"
    );
    assert_eq!(
        query(&project, "SELECT group_concat(DISTINCT state) FROM tasks")?,
        "CANCELLED"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_043_04_pause_and_cancel_take_effect_with_the_commander_down() -> R {
    // AC-043-04: the Commander fails at its start; a pause and then a cancel are accepted and reach PAUSED and
    // CANCELLED, and the Commander is never called.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let agents = Arc::new(Agents::new(&[("T1", "sleep 3")]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let (status, _) = command(&host, &control(&project, "PauseRun", "pause")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "PAUSED",
        Duration::from_secs(15),
    )
    .await?;
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(15),
    )
    .await?;
    assert_eq!(agents.commander.load(Ordering::SeqCst), 0);
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_also_ends_a_descendant_that_left_the_agents_process_group() -> R {
    // REQ-043 decision 3 (docs/adr/ADR-021.md decision 8d), no TEST id: a group signal misses a descendant that took a
    // session of its own (setsid); found under the leader and pinned by a pidfd, it is signalled too, and the run is
    // CANCELLED only once it is gone.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let escaped = dir.path().join("escaped.pid");
    let script = format!(
        "setsid sleep 60 & echo $! > {}; sleep 60",
        escaped.display()
    );
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let end = Instant::now() + Duration::from_secs(10);
    let pid: u32 = loop {
        if let Ok(text) = std::fs::read_to_string(&escaped)
            && let Ok(pid) = text.trim().parse()
        {
            break pid;
        }
        if Instant::now() > end {
            return Err("the descendant did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // It left the agent's group.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let leader: u32 = query(&project, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    let pgrp: u32 = stat
        .rsplit_once(") ")
        .ok_or("stat")?
        .1
        .split(' ')
        .nth(2)
        .ok_or("pgrp")?
        .parse()?;
    assert_ne!(pgrp, leader);
    // It is stopped however the test ends (M2T06R-62).
    let _marked = Marked(query(
        &project,
        "SELECT p.project_id || '/' || a.attempt_id FROM projects p, attempts a WHERE a.task_id = 'T1'",
    )?);
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(30),
    )
    .await?;
    // Gone, or at most a zombie its new parent has not reaped yet.
    let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(") ")
                .map(|(_, rest)| rest.chars().next().unwrap_or('?'))
        });
    assert!(
        matches!(state, None | Some('Z')),
        "the escaped descendant lives: {state:?}"
    );
    host.shutdown().await;
    Ok(())
}

/// The live processes (not zombies) whose environment carries the marker `attempt` (`<project id>/<attempt id>`).
fn marked(attempt: &str) -> Vec<u32> {
    let marker = format!("{}={attempt}", hoh_app::host::ATTEMPT_ENV).into_bytes();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|e| e == marker.as_slice()))
                && std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
                    s.rsplit_once(") ")
                        .is_some_and(|(_, r)| !r.starts_with('Z'))
                })
        })
        .collect()
}

/// SIGKILLs every live process of an attempt's marker when dropped, however the test ends.
struct Marked(String);

impl Drop for Marked {
    fn drop(&mut self) {
        for pid in marked(&self.0) {
            if let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
    }
}

/// A writer that appends to `file` every 50 ms for 120 s by the clock (longer than any wait of these tests, so only a
/// signal ends it in time) and starts no process of its own (a signal to a child must not hasten its end).
fn writer(file: &Path) -> R<String> {
    // The file exists before the writer: one stopped before its first write leaves it empty (M2T06R-150).
    if !file.exists() {
        std::fs::write(file, b"")?;
    }
    let script = file.with_extension("py");
    std::fs::write(
        &script,
        "import sys, time\nend = time.monotonic() + 120\nwhile time.monotonic() < end:\n    \
         with open(sys.argv[1], 'a') as f:\n        f.write('x\\n')\n    time.sleep(0.05)\n",
    )?;
    Ok(format!(
        "/usr/bin/python3 {} {}",
        script.display(),
        file.display()
    ))
}

/// The writer in a session of its own, orphaned at once (its parent subshell exits): nothing but its environment ties
/// it to the attempt.
fn orphan_writer(file: &Path) -> R<String> {
    Ok(format!("(setsid {} &)", writer(file)?))
}

/// The writer ignoring SIGTERM (only the SIGKILL step ends it, a grace later), orphaned in a session of its own: an
/// attempt that ended before it was gone is seen while it still writes (M2T06R-250).
fn stubborn_orphan_writer(file: &Path) -> R<String> {
    Ok(format!("(trap '' TERM; setsid {} &)", writer(file)?))
}

/// The runner's /proc for the termination proof replaced by a tree with a process it cannot read (CR-006: a test's
/// fault hook): no proof can be had.
#[derive(Debug)]
struct UnreadableProc(PathBuf);

impl hoh_app::host::HostHooks for UnreadableProc {
    fn proc_root(&self) -> Option<PathBuf> {
        Some(self.0.clone())
    }

    fn cgroups_allowed(&self) -> bool {
        false
    }
}

/// Cancel R with T1 running `script` and check that the run is CANCELLED only once nothing of T1's attempt lives
/// and that `file` no longer grows then; the project's directory.
async fn cancel_with_a_writer_outside(dir: &Path, script: &str, file: &Path) -> R<PathBuf> {
    let agents = Arc::new(Agents::new(&[("T1", script)]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir, &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    // This attempt's marker, and another project's attempt of the same id (that project's id unique to this test),
    // both as the product makes them: the bystander is not this cancel's.
    let marker = |project_id: String| {
        hoh_app::host::Work {
            project_id,
            run_id: "R".to_owned(),
            task_id: "T1".to_owned(),
            attempt_id: "T1/attempt-1".to_owned(),
            role: "developer".to_owned(),
            effect_key: String::new(),
            assignment: Default::default(),
        }
        .marker()
    };
    assert_eq!(
        query(
            &project,
            "SELECT attempt_id FROM attempts WHERE task_id = 'T1'"
        )?,
        "T1/attempt-1"
    );
    let project_id = query(&project, "SELECT project_id FROM projects")?;
    let attempt = marker(project_id.clone());
    let other = marker(format!("{project_id}-other"));
    let _marked = Marked(attempt.clone());
    let _bystander = Marked(other.clone());
    let mut bystander = Command::new("sleep")
        .arg("30")
        .env(hoh_app::host::ATTEMPT_ENV, &other)
        .spawn()?;
    let asked = Instant::now();
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(40),
    )
    .await?;
    // Signalled with the group's steps (SIGTERM at 10 s, SIGKILL at 15 s), not left to end by itself.
    let took = asked.elapsed();
    assert!(took <= Duration::from_secs(20), "{took:?}");
    assert_eq!(
        marked(&attempt),
        Vec::<u32>::new(),
        "a process of the attempt lives on"
    );
    let size = || std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let before = size();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(size(), before, "the workspace is still written");
    assert!(
        bystander.try_wait()?.is_none(),
        "another project's process was signalled"
    );
    let _ = bystander.kill();
    let _ = bystander.wait();
    host.shutdown().await;
    Ok(project)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_ends_a_writer_the_leader_started_and_orphaned_after_the_cancel_began() -> R {
    // M2T06R-01 (REQ-043 behaviours 4, 5, decision 3; docs/adr/ADR-021.md decision 8d): on SIGTERM the leader starts a
    // writer in a session of its own whose parent exits at once, then waits for SIGKILL. The writer is found by the
    // attempt's marker, signalled, and the run is CANCELLED only after it is gone.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let script = format!(
        "trap \"{}\" TERM; while :; do sleep 0.1; done",
        orphan_writer(&file)?.replace('"', "\\\"")
    );
    let project = cancel_with_a_writer_outside(dir.path(), &script, &file).await?;
    // Found after the SIGTERM step, it was sent that signal at once, not left for the SIGKILL step 5 s later: its last
    // write is within 2 s of the step.
    let term: u64 = query(
        &project,
        "SELECT json_extract(payload_json, '$.at_ms') FROM events WHERE kind = 'cancel.sigterm'",
    )?
    .parse()?;
    let last = std::fs::metadata(&file)?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let after = u64::try_from(last)?.saturating_sub(term);
    assert!(after < 2000, "the writer wrote {after} ms after SIGTERM");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_ends_a_writer_orphaned_before_the_cancel() -> R {
    // M2T06R-01 (docs/adr/ADR-021.md decision 8d): the writer was orphaned in a session of its own long before the
    // cancel, so no parent chain leads to it; its marker does.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let script = format!("{}; while :; do sleep 0.1; done", orphan_writer(&file)?);
    cancel_with_a_writer_outside(dir.path(), &script, &file)
        .await
        .map(drop)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_ends_a_descendant_without_the_marker_that_left_the_group_while_the_leader_lived()
-> R {
    // M2T06R-01 (docs/adr/ADR-021.md decision 8d): 2 s into the cancel the leader starts a writer with an emptied
    // environment in a session of its own, then dies at SIGTERM. Only the parent chain, rescanned while the group
    // lives, ties the writer to the attempt before it is orphaned.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let pid_file = dir.path().join("writer.pid");
    let script = format!(
        "sleep 2; env -i /usr/bin/setsid {} & echo $! > {}; while :; do sleep 0.1; done",
        writer(&file)?,
        pid_file.display()
    );
    let guard = KillPidFile(pid_file.clone());
    cancel_with_a_writer_outside(dir.path(), &script, &file).await?;
    let pid: u32 = std::fs::read_to_string(&pid_file)?.trim().parse()?;
    let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(") ")
                .map(|(_, rest)| rest.chars().next().unwrap_or('?'))
        });
    assert!(
        matches!(state, None | Some('Z')),
        "the writer lives: {state:?}"
    );
    drop(guard);
    Ok(())
}

/// SIGKILLs the process whose pid a file names when dropped, however the test ends.
struct KillPidFile(PathBuf);

impl Drop for KillPidFile {
    fn drop(&mut self) {
        if let Some(pid) = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|t| t.trim().parse::<i32>().ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

/// A clock the test moves ahead.
#[derive(Debug, Default)]
struct FakeClock(Mutex<Duration>);

impl FakeClock {
    fn advance(&self, by: Duration) {
        if let Ok(mut ahead) = self.0.lock() {
            *ahead += by;
        }
    }
}

impl hoh_core::delivery::Clock for FakeClock {
    fn now(&self) -> std::time::SystemTime {
        std::time::SystemTime::now() + self.0.lock().map(|d| *d).unwrap_or_default()
    }
}

/// The marker of T1's attempt (its first) in `project`, as the product makes it.
fn t1_marker(project: &Path) -> R<String> {
    Ok(hoh_app::host::Work {
        project_id: query(project, "SELECT project_id FROM projects")?,
        run_id: "R".to_owned(),
        task_id: "T1".to_owned(),
        attempt_id: "T1/attempt-1".to_owned(),
        role: "developer".to_owned(),
        effect_key: String::new(),
        assignment: Default::default(),
    }
    .marker())
}

/// None of `marker`'s processes lives and `file` no longer grows.
async fn nothing_of_it_left(marker: &str, file: &Path) -> R {
    assert_eq!(
        marked(marker),
        Vec::<u32>::new(),
        "a process of the attempt lives on"
    );
    let size = || std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let before = size();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(size(), before, "the workspace is still written");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attempt_whose_agent_ended_settles_only_once_its_processes_outside_the_group_are_gone()
-> R {
    // M2T06R-100 (docs/adr/ADR-021.md decision 8d v3, path 2): T1's agent orphans a writer and exits 0; the executor
    // empties its group. The runner stops the writer before it ends the attempt and gives up its slot.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let script = format!("{}; sleep 0.3; exit 0", stubborn_orphan_writer(&file)?);
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    let marker = t1_marker(&project)?;
    let _marked = Marked(marker.clone());
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "SETTLED",
        Duration::from_secs(20),
    )
    .await?;
    nothing_of_it_left(&marker, &file).await?;
    // The writer ignored SIGTERM: it was gone only at the SIGKILL step, and the attempt settled after that.
    assert_eq!(
        query(
            &project,
            "SELECT json_extract(payload_json, '$.steps') FROM events WHERE kind = 'runner.outside'"
        )?,
        r#"["outside_sigterm","outside_sigkill","outside_reaped"]"#
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_lease_is_given_up_only_once_its_holders_processes_outside_the_group_are_gone()
-> R {
    // M2T06R-100 (decision 8d v3, path 3): T1 holds the writer lease and its agent orphaned a writer; the clock moves
    // past the lease. The settle stops the group and then the writer; T2 gets the lease only after.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let script = format!(
        "{}; while :; do sleep 0.1; done",
        stubborn_orphan_writer(&file)?
    );
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let clock = Arc::new(FakeClock::default());
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(clock.clone());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: agents.clone(),
        context: context(),
    });
    let host = start_when_free(config).await?;
    let marker = t1_marker(&project)?;
    let _marked = Marked(marker.clone());
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    clock.advance(Duration::from_secs(300));
    until(
        &project,
        "SELECT count(*) FROM attempts WHERE task_id = 'T2'",
        "1",
        Duration::from_secs(40),
    )
    .await?;
    nothing_of_it_left(&marker, &file).await?;
    assert_eq!(
        seq(
            &project,
            "SELECT count(*) FROM events WHERE kind IN ('lease.outside_sigterm', 'lease.outside_sigkill')"
        )?,
        2
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosts_start_reconcile_ends_an_attempt_only_once_its_processes_outside_the_group_are_gone()
-> R {
    // M2T06R-100 (decision 8d v3, path 4): the host stops with T1 running and a writer its agent orphaned; the next
    // host's start reconcile stops the group and the writer before it ends T1's attempt.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let script = format!(
        "{}; while :; do sleep 0.1; done",
        stubborn_orphan_writer(&file)?
    );
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, first) = host(dir.path(), &agents).await?;
    let marker = t1_marker(&project)?;
    let _marked = Marked(marker.clone());
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let end = Instant::now() + Duration::from_secs(10);
    while marked(&marker).len() < 2 {
        if Instant::now() > end {
            return Err("the writer did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    first.shutdown().await;
    assert!(
        marked(&marker).len() >= 2,
        "the leader and the writer outlive their host"
    );
    // The next host, with no executor: its start reconcile alone.
    let reconciled = seq(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
    )?;
    let second = start_when_free(config(&project)).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
        &(reconciled + 1).to_string(),
        Duration::from_secs(40),
    )
    .await?;
    assert_ne!(
        query(&project, "SELECT state FROM attempts WHERE task_id = 'T1'")?,
        "RUNNING"
    );
    nothing_of_it_left(&marker, &file).await?;
    assert_eq!(
        query(
            &project,
            "SELECT json_extract(payload_json, '$.process.steps') FROM events WHERE kind = 'reconcile.attempt' \
             AND json_extract(payload_json, '$.attempt_id') = 'T1/attempt-1' ORDER BY seq DESC LIMIT 1"
        )?,
        r#"["sigterm","reaped","outside_sigterm","outside_sigkill","outside_reaped"]"#
    );
    second.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosts_start_reconcile_pins_through_the_leader_a_descendant_without_the_marker_before_it_stops_the_group()
-> R {
    // M2T06R-251 (decision 8d, path 4): T1's leader starts a writer with an emptied environment in a session of its own,
    // ignoring SIGTERM. The next host's start reconcile pins it through the living leader before it stops the group;
    // after the group's stop it is an unmarked orphan that only that pin ties to the attempt.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let pid_file = dir.path().join("w.pid");
    let script = format!(
        "(trap '' TERM; exec env -i /usr/bin/setsid {}) & echo $! > {}; while :; do sleep 0.1; done",
        writer(&file)?,
        pid_file.display()
    );
    let guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, first) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let end = Instant::now() + Duration::from_secs(10);
    while std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0) == 0 {
        if Instant::now() > end {
            return Err("the writer did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    first.shutdown().await;
    let reconciled = seq(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
    )?;
    let second = start_when_free(config(&project)).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
        &(reconciled + 1).to_string(),
        Duration::from_secs(40),
    )
    .await?;
    let size = || std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    let before = size();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(size(), before, "the workspace is still written");
    let pid: u32 = std::fs::read_to_string(&pid_file)?.trim().parse()?;
    let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(") ")
                .map(|(_, rest)| rest.chars().next().unwrap_or('?'))
        });
    assert!(
        matches!(state, None | Some('Z')),
        "the writer lives: {state:?}"
    );
    drop(guard);
    second.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attempt_whose_agent_ended_stays_with_its_slot_when_its_termination_cannot_be_proved()
-> R {
    // M2T06R-251 (decision 8d, path 2): the agent exits 0 and its group is emptied, but the runner's /proc (a test's
    // tree) has a process it cannot read: no proof, so the attempt stays RUNNING with its slot and says why.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let fake = dir.path().join("proc");
    std::fs::create_dir_all(fake.join("sys/kernel"))?;
    std::fs::write(fake.join("sys/kernel/ns_last_pid"), b"10\n")?;
    std::fs::create_dir_all(fake.join("7"))?;
    std::fs::write(fake.join("7/stat"), b"garbage")?;
    let agents = Arc::new(Agents::new(&[("T1", "exit 0")]));
    let _leaders = Leaders(agents.leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let mut config = config(&project);
    config.hooks = Arc::new(UnreadableProc(fake));
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: agents.clone(),
        context: context(),
    });
    let host = start_when_free(config).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'runner.unreaped'",
        "1",
        Duration::from_secs(30),
    )
    .await?;
    assert_eq!(
        query(&project, "SELECT state FROM attempts WHERE task_id = 'T1'")?,
        "RUNNING"
    );
    assert_eq!(
        query(
            &project,
            "SELECT json_extract(payload_json, '$.steps') FROM events WHERE kind = 'runner.unreaped'"
        )?,
        r#"["outside_unreadable"]"#
    );
    assert_eq!(
        query(
            &project,
            "SELECT count(*) FROM attempts WHERE task_id = 'T2'"
        )?,
        "0",
        "the slot was not given up"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_ends_a_writer_the_leader_released_on_sigterm_just_before_it_exited() -> R {
    // M2T06R-152 (docs/adr/ADR-021.md decision 8d v3): on SIGTERM the leader orphans a writer and exits 20 ms later,
    // so its group is empty at once; the writer is found by the scans made at every look once the group is empty.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let script = format!(
        "trap \"{}; sleep 0.02; exit 0\" TERM; while :; do sleep 0.1; done",
        orphan_writer(&file)?.replace('"', "\\\"")
    );
    cancel_with_a_writer_outside(dir.path(), &script, &file)
        .await
        .map(drop)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_ends_what_a_pinned_process_without_the_marker_starts_after_the_leader_died() -> R
{
    // M2T06R-152 (decision 8d v3): the leader starts W with an emptied environment in a session of its own; W ignores
    // SIGTERM. The leader dies at SIGTERM (10 s); 1.5 s later W starts the writer C, which ignores SIGTERM too. Only
    // the parent chain from W, pinned while the leader lived, finds C; both end at SIGKILL (15 s).
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let file = dir.path().join("written.txt");
    let pid_file = dir.path().join("w.pid");
    let script = format!(
        "env -i /usr/bin/setsid /bin/sh -c 'trap \"\" TERM; /bin/sleep 11.5; {} & wait' & echo $! > {}; \
         while :; do sleep 0.1; done",
        writer(&file)?,
        pid_file.display()
    );
    let guard = KillPidFile(pid_file.clone());
    cancel_with_a_writer_outside(dir.path(), &script, &file).await?;
    drop(guard);
    Ok(())
}

/// Set in a test process started inside a delegated scope.
const IN_SCOPE: &str = "HOH_TEST_IN_DELEGATED_SCOPE";

/// Run this binary's ignored test `name` inside a delegated scope of the user's systemd manager
/// (`systemd-run --user --scope -p Delegate=yes`), where an in-process host tracks its attempts by cgroups of their own
/// (docs/adr/ADR-021.md decision 8d v5, CR-011). It must have run and passed.
fn in_delegated_scope(name: &str) -> R {
    let exe = std::env::current_exe()?;
    let out = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "-p",
            "Delegate=yes",
            "--quiet",
            "--collect",
            "--expand-environment=no",
            "--",
        ])
        .arg(&exe)
        .args([
            name,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(IN_SCOPE, "1")
        .output()?;
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    if !out.status.success() || !stdout.contains("1 passed") {
        return Err(format!(
            "{name} in a delegated scope: {}\n{stdout}\n{stderr}",
            out.status
        )
        .into());
    }
    Ok(())
}

/// An orphaned writer without the marker (an emptied environment) that ignores SIGTERM, its pid in `pid_file`: the
/// /proc scan cannot find it once its parent subshell is gone; only its cgroup holds it.
fn hidden_writer(file: &Path, pid_file: &Path) -> R<String> {
    Ok(format!(
        "(trap '' TERM; env -i /usr/bin/setsid {} & echo $! > {})",
        writer(file)?,
        pid_file.display()
    ))
}

/// Whether the process `pid_file` names is gone (or a zombie not reaped yet).
fn gone(pid_file: &Path) -> R<bool> {
    let pid: u32 = std::fs::read_to_string(pid_file)?.trim().parse()?;
    let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(") ")
                .map(|(_, rest)| rest.chars().next().unwrap_or('?'))
        });
    Ok(matches!(state, None | Some('Z')))
}

/// Its attempt's tracking as its start recorded it, and the cgroup's path.
fn tracking(project: &Path) -> R<(String, String)> {
    Ok((
        query(
            project,
            "SELECT json_extract(payload_json, '$.tracking') FROM events WHERE kind = 'attempt.tracking' \
             AND json_extract(payload_json, '$.attempt_id') = 'T1/attempt-1'",
        )?,
        query(
            project,
            "SELECT json_extract(payload_json, '$.cgroup') FROM events WHERE kind = 'attempt.tracking' \
             AND json_extract(payload_json, '$.attempt_id') = 'T1/attempt-1'",
        )?,
    ))
}

#[test]
fn in_a_delegated_cgroup_a_cancel_ends_a_hidden_writer_by_the_attempts_cgroup() -> R {
    in_delegated_scope("cgroup_cancel_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_cancel_ends_a_hidden_writer_by_the_attempts_cgroup"]
async fn cgroup_cancel_inside() -> R {
    // CR-011 (decision 8d v5, path 1): T1's agent leaves an orphaned writer with no marker that ignores SIGTERM. The
    // attempt's cgroup holds it: SIGTERM goes to its members, SIGKILL to all at once (cgroup.kill), and the run is
    // CANCELLED only once the cgroup is empty.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (file, pid_file) = (dir.path().join("written.txt"), dir.path().join("w.pid"));
    let script = format!(
        "{}; while :; do sleep 0.1; done",
        hidden_writer(&file, &pid_file)?
    );
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let (mode, cgroup) = tracking(&project)?;
    assert_eq!(mode, "cgroup", "{cgroup}");
    let end = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() {
        if Instant::now() > end {
            return Err("the writer did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let asked = Instant::now();
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(40),
    )
    .await?;
    assert!(
        asked.elapsed() <= Duration::from_secs(20),
        "{:?}",
        asked.elapsed()
    );
    assert!(gone(&pid_file)?, "the hidden writer lives on");
    assert!(
        !Path::new(&cgroup).exists(),
        "the attempt's cgroup was removed once empty"
    );
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_an_attempt_whose_agent_ended_settles_once_its_cgroup_is_empty() -> R {
    in_delegated_scope("cgroup_runner_end_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_an_attempt_whose_agent_ended_settles_once_its_cgroup_is_empty"]
async fn cgroup_runner_end_inside() -> R {
    // CR-011 (decision 8d v5, path 2): T1's agent leaves the hidden writer and exits 0; the runner stops what is left in
    // the attempt's cgroup (SIGTERM, ignored; cgroup.kill) before the attempt settles.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (file, pid_file) = (dir.path().join("written.txt"), dir.path().join("w.pid"));
    let script = format!("{}; sleep 0.3; exit 0", hidden_writer(&file, &pid_file)?);
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "SETTLED",
        Duration::from_secs(20),
    )
    .await?;
    assert!(
        gone(&pid_file)?,
        "the attempt settled while its hidden writer lived"
    );
    assert_eq!(tracking(&project)?.0, "cgroup");
    assert_eq!(
        query(
            &project,
            "SELECT json_extract(payload_json, '$.steps') FROM events WHERE kind = 'runner.outside'"
        )?,
        r#"["cgroup_sigterm","cgroup_kill","cgroup_reaped"]"#
    );
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_an_expired_lease_is_given_up_once_its_holders_cgroup_is_empty() -> R {
    in_delegated_scope("cgroup_settle_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_an_expired_lease_is_given_up_once_its_holders_cgroup_is_empty"]
async fn cgroup_settle_inside() -> R {
    // CR-011 (decision 8d v5, path 3): T1 holds the writer lease and its agent left the hidden writer; the clock moves
    // past the lease. The settle stops the group and then the attempt's cgroup; T2 gets the lease only after.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (file, pid_file) = (dir.path().join("written.txt"), dir.path().join("w.pid"));
    let script = format!(
        "{}; while :; do sleep 0.1; done",
        hidden_writer(&file, &pid_file)?
    );
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let clock = Arc::new(FakeClock::default());
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(clock.clone());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: agents.clone(),
        context: context(),
    });
    let host = start_when_free(config).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let end = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() {
        if Instant::now() > end {
            return Err("the writer did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    clock.advance(Duration::from_secs(300));
    until(
        &project,
        "SELECT count(*) FROM attempts WHERE task_id = 'T2'",
        "1",
        Duration::from_secs(40),
    )
    .await?;
    assert!(
        gone(&pid_file)?,
        "T2 got the lease while T1's hidden writer lived"
    );
    assert_eq!(tracking(&project)?.0, "cgroup");
    assert_eq!(
        seq(
            &project,
            "SELECT count(*) FROM events WHERE kind IN ('lease.cgroup_sigterm', 'lease.cgroup_kill')"
        )?,
        2
    );
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_hosts_start_reconcile_ends_an_attempt_once_its_cgroup_is_empty() -> R {
    in_delegated_scope("cgroup_start_reconcile_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_hosts_start_reconcile_ends_an_attempt_once_its_cgroup_is_empty"]
async fn cgroup_start_reconcile_inside() -> R {
    // CR-011 (decision 8d v5, path 4): the host stops with T1 running and its hidden writer; the next host finds T1's
    // cgroup by the path its start recorded and stops it before it ends the attempt.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (file, pid_file) = (dir.path().join("written.txt"), dir.path().join("w.pid"));
    let script = format!(
        "{}; while :; do sleep 0.1; done",
        hidden_writer(&file, &pid_file)?
    );
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, first) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let end = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() {
        if Instant::now() > end {
            return Err("the writer did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    first.shutdown().await;
    let reconciled = seq(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
    )?;
    let second = start_when_free(config(&project)).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
        &(reconciled + 1).to_string(),
        Duration::from_secs(40),
    )
    .await?;
    assert_ne!(
        query(&project, "SELECT state FROM attempts WHERE task_id = 'T1'")?,
        "RUNNING"
    );
    assert!(
        gone(&pid_file)?,
        "the attempt ended while its hidden writer lived"
    );
    assert_eq!(
        query(
            &project,
            "SELECT json_extract(payload_json, '$.process.steps') FROM events WHERE kind = 'reconcile.attempt' \
             AND json_extract(payload_json, '$.attempt_id') = 'T1/attempt-1' ORDER BY seq DESC LIMIT 1"
        )?,
        r#"["sigterm","reaped","cgroup_sigterm","cgroup_kill","cgroup_reaped"]"#
    );
    second.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_attempt_records_how_its_processes_are_tracked_and_why_it_is_degraded() -> R {
    // CR-011 (decision 8d v5): an attempt records its tracking before its gate opens; outside a delegated cgroup (this
    // test run from a login session or CI) it is the degraded proc scan, with why; inside one, its cgroup.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let agents = Arc::new(Agents::new(&[("T1", "sleep 60")]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    // Outside a delegated scope, or kept out of its cgroup (M2T06R-452): the degraded proc scan, with why.
    let (mode, _) = tracking(&project)?;
    assert_eq!(mode, "proc");
    assert_eq!(
        query(
            &project,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'attempt.tracking'"
        )?,
        "the host has no delegated cgroup"
    );
    host.shutdown().await;
    Ok(())
}

/// This process's own cgroup directory (the delegated scope's, inside one).
fn own_cgroup() -> R<PathBuf> {
    let text = std::fs::read_to_string("/proc/self/cgroup")?;
    let path = text
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .ok_or("no cgroup v2 line")?;
    Ok(Path::new("/sys/fs/cgroup").join(path.trim().trim_start_matches('/')))
}

/// A writer that ignores SIGTERM, orphaned in a session of its own, which first moves itself out of the attempt's cgroup
/// into `base` (the delegated scope's own) by writing its pid there: a same-user process can; it keeps its marker.
fn migrating_writer(file: &Path, pid_file: &Path, base: &Path) -> R<String> {
    Ok(format!(
        "(trap '' TERM; setsid sh -c 'echo $$ > {}/cgroup.procs && exec {}' & echo $! > {})",
        base.display(),
        writer(file)?,
        pid_file.display()
    ))
}

/// Wait until the writer `pid_file` names runs in `base`, out of the attempt's cgroup.
async fn moved_out(pid_file: &Path, base: &Path) -> R {
    let wanted = format!("0::/{}", base.strip_prefix("/sys/fs/cgroup")?.display());
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(pid) = std::fs::read_to_string(pid_file).map(|t| t.trim().to_owned())
            && std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                .is_ok_and(|c| c.trim() == wanted)
        {
            return Ok(());
        }
        if Instant::now() > end {
            return Err("the writer did not move out of the attempt's cgroup".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn in_a_delegated_cgroup_a_cancel_ends_a_process_that_moved_out_of_the_attempts_cgroup() -> R {
    in_delegated_scope("cgroup_moved_out_cancel_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_cancel_ends_a_process_that_moved_out_of_the_attempts_cgroup"]
async fn cgroup_moved_out_cancel_inside() -> R {
    // M2T06R-400 (decision 8d v6): T1's agent leaves a writer that moved itself out of the attempt's cgroup, keeping its
    // marker. The cgroup is empty soon; the marker scan, part of the proof, finds the writer: it gets the ladder's
    // signals (SIGTERM ignored, SIGKILL) and the run is CANCELLED only once it is gone.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let base = own_cgroup()?;
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (file, pid_file) = (dir.path().join("written.txt"), dir.path().join("w.pid"));
    let script = format!(
        "{}; while :; do sleep 0.1; done",
        migrating_writer(&file, &pid_file, &base)?
    );
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(tracking(&project)?.0, "cgroup");
    moved_out(&pid_file, &base).await?;
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(40),
    )
    .await?;
    assert!(gone(&pid_file)?, "the writer that left the cgroup lives on");
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_an_attempt_whose_agent_ended_settles_only_once_a_process_that_moved_out_is_gone()
-> R {
    in_delegated_scope("cgroup_moved_out_runner_end_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_an_attempt_whose_agent_ended_settles_only_once_a_process_that_moved_out_is_gone"]
async fn cgroup_moved_out_runner_end_inside() -> R {
    // M2T06R-400 (decision 8d v6, path 2): the agent leaves the writer that moved out of its cgroup and exits 0; the
    // runner's proof includes the marker scan, which stops the writer before the attempt settles.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let base = own_cgroup()?;
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (file, pid_file) = (dir.path().join("written.txt"), dir.path().join("w.pid"));
    let script = format!(
        "{}; sleep 1; exit 0",
        migrating_writer(&file, &pid_file, &base)?
    );
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "SETTLED",
        Duration::from_secs(30),
    )
    .await?;
    assert!(
        gone(&pid_file)?,
        "the attempt settled while the writer that left its cgroup lived"
    );
    assert_eq!(
        query(
            &project,
            "SELECT json_extract(payload_json, '$.steps') FROM events WHERE kind = 'runner.outside'"
        )?,
        r#"["outside_sigterm","outside_sigkill","outside_reaped"]"#
    );
    host.shutdown().await;
    Ok(())
}

/// Starts each task's script as the leader of its own process group and, when the leader ends, reaps it without
/// emptying its group: a member it left behind keeps the group without its leader.
#[derive(Debug)]
struct LeavesItsGroup(Arc<Mutex<Vec<u32>>>, &'static str);

impl hoh_app::host::Executor for LeavesItsGroup {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        let _ = answer.send(Ok(()));
        ack
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let mut child = Command::new("sh")
            .args([
                "-c",
                &format!(
                    "read -r go; [ \"$go\" = go ] || exit 0; {} & exit 0",
                    self.1
                ),
            ])
            .process_group(0)
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        if let Ok(mut leaders) = self.0.lock() {
            leaders.push(pid);
        }
        let stdin = child.stdin.take();
        let gate = hoh_app::host::Gate::new(move || {
            use std::io::Write as _;
            stdin
                .ok_or("no stdin")?
                .write_all(b"go\n")
                .map_err(|e| e.to_string())
        });
        let (end, ended) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = end.send(hoh_app::host::Outcome {
                success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: format!("{status:?}"),
                group_gone: false,
                class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                observed: Default::default(),
                unknown_effect: false,
            });
        });
        Ok(hoh_app::host::Started { pid, gate, ended })
    }
}

#[test]
fn in_a_delegated_cgroup_a_cancel_stops_a_group_left_without_its_leader_through_the_cgroup() -> R {
    in_delegated_scope("cgroup_leaderless_cancel_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_cancel_stops_a_group_left_without_its_leader_through_the_cgroup"]
async fn cgroup_leaderless_cancel_inside() -> R {
    // M2T06R-401 (decision 8d v6): T1's leader ends and is reaped, its member lives on in its group (no signal may go to
    // a group without its leader, ADR-019 decision 8). The attempt's cgroup holds the member by membership: the cancel
    // stops it through the cgroup, sees the group gone, and the run is CANCELLED.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let leaders = Arc::new(Mutex::new(Vec::new()));
    let _leaders = Leaders(leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let mut config = config(&project);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: Arc::new(LeavesItsGroup(leaders.clone(), "sleep 60")),
        context: context(),
    });
    let host = start_when_free(config).await?;
    // The leader ended with its group not emptied: the attempt and its slot stay.
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'runner.unreaped'",
        "1",
        Duration::from_secs(20),
    )
    .await?;
    assert_eq!(tracking(&project)?.0, "cgroup");
    let leader: u32 = query(&project, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    let group_lives = || {
        i32::try_from(leader)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|g| rustix::process::test_kill_process_group(g).is_ok())
    };
    assert!(
        group_lives(),
        "the precondition: a group without its leader"
    );
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(30),
    )
    .await?;
    assert!(!group_lives(), "the group's member lives on");
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_host_told_not_to_use_it_tracks_by_the_proc_scan() -> R {
    in_delegated_scope("cgroup_refused_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_host_told_not_to_use_it_tracks_by_the_proc_scan"]
async fn cgroup_refused_inside() -> R {
    // M2T06R-452: in a delegated scope, a host whose hooks refuse cgroups (the proc mode's tests) records the proc scan.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let agents = Arc::new(Agents::new(&[("T1", "sleep 60")]));
    let _leaders = Leaders(agents.leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let mut config = config(&project);
    config.hooks = Arc::new(ProcOnly);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: agents.clone(),
        context: context(),
    });
    let host = start_when_free(config).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(tracking(&project)?.0, "proc");
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_group_left_without_its_leader_and_out_of_the_cgroup_keeps_the_run_cancelling()
-> R {
    in_delegated_scope("cgroup_leaderless_outside_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_group_left_without_its_leader_and_out_of_the_cgroup_keeps_the_run_cancelling"]
async fn cgroup_leaderless_outside_inside() -> R {
    // M2T06R-401 (decision 8d v6): the leader is reaped and its member moved itself out of the attempt's cgroup with an
    // emptied environment, staying in the leader's group. The cgroup is empty and the marker scan finds nothing, but the
    // group without its leader still lives and may not be signalled: the run stays cancelling.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let base = own_cgroup()?;
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let leaders = Arc::new(Mutex::new(Vec::new()));
    let _leaders = Leaders(leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let member: &'static str = Box::leak(
        format!(
            "sh -c 'echo $$ > {}/cgroup.procs && exec env -i /bin/sleep 60'",
            base.display()
        )
        .into_boxed_str(),
    );
    let mut config = config(&project);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: Arc::new(LeavesItsGroup(leaders.clone(), member)),
        context: context(),
    });
    let host = start_when_free(config).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'runner.unreaped'",
        "1",
        Duration::from_secs(20),
    )
    .await?;
    let leader: u32 = query(&project, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    let group = i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or("no group")?;
    assert!(
        rustix::process::test_kill_process_group(group).is_ok(),
        "the precondition: a group without its leader"
    );
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    tokio::time::sleep(Duration::from_secs(6)).await;
    let state = query(&project, "SELECT state FROM runs WHERE run_id = 'R'")?;
    // Stop the member by hand (the test's own), then let the cancel finish.
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    assert_eq!(
        state, "CANCEL_REQUESTED",
        "cancelled while its leaderless group lived"
    );
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(30),
    )
    .await?;
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_cancel_ends_a_leader_whose_command_name_is_not_utf8() -> R {
    in_delegated_scope("cgroup_non_utf8_leader_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_cancel_ends_a_leader_whose_command_name_is_not_utf8"]
async fn cgroup_non_utf8_leader_inside() -> R {
    // M2T06R-500 (decision 8d v7): T1's leader moves itself out of the attempt's cgroup, empties its environment and
    // runs under a command name that is not UTF-8, staying in its own group. Its identity is read all the same, so the
    // cancel stops its group: the run is CANCELLED only once the leader is gone (it was CANCELLED, and the writer lease
    // released, with the leader alive).
    use std::os::unix::ffi::OsStrExt as _;
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let base = own_cgroup()?;
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let link = dir.path().join(std::ffi::OsStr::from_bytes(b"w\xffr"));
    // dash, not sleep: this platform's sleep is a multi-call program that picks its tool by its name.
    std::os::unix::fs::symlink("/usr/bin/dash", &link)?;
    let script = format!(
        "echo $$ > {}/cgroup.procs && exec env -i \"$(printf '{}/w\\377r')\" -c 'while :; do /bin/sleep 1; done'",
        base.display(),
        dir.path().display()
    );
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(tracking(&project)?.0, "cgroup");
    let leader: u32 = query(&project, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    // The precondition: the leader runs under that name, its group alive.
    let end = Instant::now() + Duration::from_secs(10);
    while std::fs::read(format!("/proc/{leader}/comm"))? != b"w\xffr\n" {
        if Instant::now() > end {
            return Err("the leader did not take its name".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let group = i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or("no group")?;
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(30),
    )
    .await?;
    assert!(
        rustix::process::test_kill_process_group(group).is_err(),
        "CANCELLED while the leader's group lived"
    );
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_leader_whose_record_cannot_confirm_it_keeps_the_run_cancelling_while_its_group_lives()
-> R {
    in_delegated_scope("cgroup_unverifiable_leader_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_leader_whose_record_cannot_confirm_it_keeps_the_run_cancelling_while_its_group_lives"]
async fn cgroup_unverifiable_leader_inside() -> R {
    // M2T06R-500 (decision 8d v7): T1's leader moved itself out of the attempt's cgroup with an emptied environment and
    // lives on in its own group; its record lacks the start time (an incomplete record: nothing can confirm the process
    // is the attempt's, so it gets no signal, ADR-019 decision 8). The cgroup is empty and the marker scan finds
    // nothing, but the group, checked by the recorded pid, still lives: the run stays cancelling (an inspection that
    // could not confirm the leader counted as an empty group, and the run was CANCELLED).
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let base = own_cgroup()?;
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let script = format!(
        "echo $$ > {}/cgroup.procs && exec env -i /bin/sleep 60",
        base.display()
    );
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, first) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(tracking(&project)?.0, "cgroup");
    let leader: u32 = query(&project, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    first.shutdown().await;
    seed(
        &project,
        "UPDATE attempts SET process_start_id = NULL WHERE task_id = 'T1';",
    )?;
    let host = start_when_free(config(&project)).await?;
    let group = i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or("no group")?;
    assert!(
        rustix::process::test_kill_process_group(group).is_ok(),
        "the precondition: the leader's group lives"
    );
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    tokio::time::sleep(Duration::from_secs(6)).await;
    let state = query(&project, "SELECT state FROM runs WHERE run_id = 'R'")?;
    // Stop the leader by hand (the test's own), then let the cancel finish.
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    assert_eq!(
        state, "CANCEL_REQUESTED",
        "cancelled while the leader's group lived"
    );
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(30),
    )
    .await?;
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_cancels_sigterm_reaches_a_member_of_a_nested_cgroup() -> R {
    in_delegated_scope("cgroup_nested_member_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_cancels_sigterm_reaches_a_member_of_a_nested_cgroup"]
async fn cgroup_nested_member_inside() -> R {
    // M2T06R-501 (decision 8d v7): an orphaned member of T1, in a session of its own with an emptied environment, moves
    // itself into a cgroup nested in the attempt's and ends at SIGTERM, recording it. The cancel's SIGTERM step
    // reaches it (it went to the attempt cgroup's direct members only, so the member met cgroup.kill), and the nested
    // cgroup is removed with the attempt's once both are proved empty.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (term, pid_file) = (dir.path().join("term.txt"), dir.path().join("m.pid"));
    let member = dir.path().join("member.sh");
    // Read by sh, not executed: nothing is written and then executed (M2T06R-354).
    std::fs::write(
        &member,
        format!(
            "trap 'echo term > {}; exit 0' TERM\n\
             cg=/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup)\n\
             mkdir \"$cg/nested\" && echo $$ > \"$cg/nested/cgroup.procs\" && echo $$ > {}\n\
             while :; do sleep 0.1; done\n",
            term.display(),
            pid_file.display()
        ),
    )?;
    let script = format!(
        "(env -i PATH=/usr/bin:/bin /usr/bin/setsid /bin/sh {} &); while :; do sleep 0.1; done",
        member.display()
    );
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, host) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    let (mode, path) = tracking(&project)?;
    assert_eq!(mode, "cgroup");
    let end = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() {
        if Instant::now() > end {
            return Err("the member did not move into its nested cgroup".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (status, _) = command(&host, &control(&project, "CancelRun", "cancel")?).await?;
    assert_eq!(status, 202);
    until(
        &project,
        "SELECT state FROM runs WHERE run_id = 'R'",
        "CANCELLED",
        Duration::from_secs(30),
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(&term).unwrap_or_default().trim(),
        "term",
        "the nested member got no SIGTERM"
    );
    assert!(
        !Path::new(&path).exists(),
        "the attempt's cgroup was left with its nested one"
    );
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_an_expired_lease_is_given_up_only_once_a_process_that_moved_out_is_gone()
-> R {
    in_delegated_scope("cgroup_moved_out_settle_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_an_expired_lease_is_given_up_only_once_a_process_that_moved_out_is_gone"]
async fn cgroup_moved_out_settle_inside() -> R {
    // M2T06R-550 (decision 8d v6, path 3): T1 holds the writer lease and its agent left a writer that moved itself out
    // of the attempt's cgroup, keeping its marker; the clock moves past the lease. The settle's proof includes the
    // marker scan: the writer gets SIGTERM (ignored) and SIGKILL, and T2 gets the lease only after it is gone.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let base = own_cgroup()?;
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (file, pid_file) = (dir.path().join("written.txt"), dir.path().join("w.pid"));
    let script = format!(
        "{}; while :; do sleep 0.1; done",
        migrating_writer(&file, &pid_file, &base)?
    );
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let clock = Arc::new(FakeClock::default());
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(clock.clone());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: agents.clone(),
        context: context(),
    });
    let host = start_when_free(config).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(tracking(&project)?.0, "cgroup");
    moved_out(&pid_file, &base).await?;
    clock.advance(Duration::from_secs(300));
    until(
        &project,
        "SELECT count(*) FROM attempts WHERE task_id = 'T2'",
        "1",
        Duration::from_secs(40),
    )
    .await?;
    assert!(
        gone(&pid_file)?,
        "T2 got the lease while the writer that left T1's cgroup lived"
    );
    assert_eq!(
        seq(
            &project,
            "SELECT count(*) FROM events WHERE kind IN ('lease.outside_sigterm', 'lease.outside_sigkill')"
        )?,
        2
    );
    host.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_hosts_start_reconcile_ends_an_attempt_only_once_a_process_that_moved_out_is_gone()
-> R {
    in_delegated_scope("cgroup_moved_out_start_reconcile_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_hosts_start_reconcile_ends_an_attempt_only_once_a_process_that_moved_out_is_gone"]
async fn cgroup_moved_out_start_reconcile_inside() -> R {
    // M2T06R-550 (decision 8d v6, path 4): the host stops with T1 running and its writer that moved itself out of the
    // attempt's cgroup, keeping its marker. The next host's reconcile empties the cgroup and, by the marker scan, stops
    // the writer before it ends the attempt.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let base = own_cgroup()?;
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let (file, pid_file) = (dir.path().join("written.txt"), dir.path().join("w.pid"));
    let script = format!(
        "{}; while :; do sleep 0.1; done",
        migrating_writer(&file, &pid_file, &base)?
    );
    let _guard = KillPidFile(pid_file.clone());
    let agents = Arc::new(Agents::new(&[("T1", script.as_str())]));
    let _leaders = Leaders(agents.leaders.clone());
    let (project, first) = host(dir.path(), &agents).await?;
    until(
        &project,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(tracking(&project)?.0, "cgroup");
    moved_out(&pid_file, &base).await?;
    first.shutdown().await;
    let reconciled = seq(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
    )?;
    let second = start_when_free(config(&project)).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
        &(reconciled + 1).to_string(),
        Duration::from_secs(40),
    )
    .await?;
    assert_ne!(
        query(&project, "SELECT state FROM attempts WHERE task_id = 'T1'")?,
        "RUNNING"
    );
    assert!(
        gone(&pid_file)?,
        "the attempt ended while the writer that left its cgroup lived"
    );
    let steps = query(
        &project,
        "SELECT json_extract(payload_json, '$.process.steps') FROM events WHERE kind = 'reconcile.attempt' \
         AND json_extract(payload_json, '$.attempt_id') = 'T1/attempt-1' ORDER BY seq DESC LIMIT 1",
    )?;
    assert!(
        steps.contains("outside_sigterm") && steps.contains("outside_sigkill"),
        "{steps}"
    );
    second.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_hosts_start_reconcile_stops_a_group_left_without_its_leader_through_the_cgroup()
-> R {
    in_delegated_scope("cgroup_leaderless_start_reconcile_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_hosts_start_reconcile_stops_a_group_left_without_its_leader_through_the_cgroup"]
async fn cgroup_leaderless_start_reconcile_inside() -> R {
    // M2T06R-551 (decision 8d v6, M2T06R-401, path 4): T1's leader ended and was reaped, its member lives on in its group
    // and in the attempt's cgroup; the host stops. The next host's reconcile cannot tell the group apart (no signal
    // to it) but stops the cgroup by membership, looks at the group again, sees it gone and ends the attempt.
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let leaders = Arc::new(Mutex::new(Vec::new()));
    let _leaders = Leaders(leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let mut with_runner = config(&project);
    with_runner.runner = Some(hoh_app::host::RunnerConfig {
        executor: Arc::new(LeavesItsGroup(leaders.clone(), "sleep 60")),
        context: context(),
    });
    let first = start_when_free(with_runner).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'runner.unreaped'",
        "1",
        Duration::from_secs(20),
    )
    .await?;
    assert_eq!(tracking(&project)?.0, "cgroup");
    let leader: u32 = query(&project, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    first.shutdown().await;
    let group = i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or("no group")?;
    assert!(
        rustix::process::test_kill_process_group(group).is_ok(),
        "the precondition: a group without its leader"
    );
    let reconciled = seq(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
    )?;
    let second = start_when_free(config(&project)).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
        &(reconciled + 1).to_string(),
        Duration::from_secs(40),
    )
    .await?;
    assert_ne!(
        query(&project, "SELECT state FROM attempts WHERE task_id = 'T1'")?,
        "RUNNING",
        "the attempt was left though its cgroup and group were stopped"
    );
    assert!(
        rustix::process::test_kill_process_group(group).is_err(),
        "the group's member lives on"
    );
    second.shutdown().await;
    Ok(())
}

#[test]
fn in_a_delegated_cgroup_a_hosts_start_reconcile_keeps_an_attempt_whose_leaderless_group_lives_out_of_the_cgroup()
-> R {
    in_delegated_scope("cgroup_leaderless_outside_start_reconcile_inside")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs inside a delegated scope, started by in_a_delegated_cgroup_a_hosts_start_reconcile_keeps_an_attempt_whose_leaderless_group_lives_out_of_the_cgroup"]
async fn cgroup_leaderless_outside_start_reconcile_inside() -> R {
    // M2T06R-551 (decision 8d v6, path 4): T1's leader was reaped and its member moved itself out of the attempt's
    // cgroup with an emptied environment, staying in the leader's group; the host stops. The next host's reconcile
    // finds the cgroup empty and the marker scan quiet, but the group without its leader still lives: the attempt and
    // its slot stay (it ended, and the lease would go, when the group was not looked at again).
    if std::env::var_os(IN_SCOPE).is_none() {
        return Ok(());
    }
    let base = own_cgroup()?;
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let leaders = Arc::new(Mutex::new(Vec::new()));
    let _leaders = Leaders(leaders.clone());
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let member: &'static str = Box::leak(
        format!(
            "sh -c 'echo $$ > {}/cgroup.procs && exec env -i /bin/sleep 60'",
            base.display()
        )
        .into_boxed_str(),
    );
    let mut with_runner = config(&project);
    with_runner.runner = Some(hoh_app::host::RunnerConfig {
        executor: Arc::new(LeavesItsGroup(leaders.clone(), member)),
        context: context(),
    });
    let first = start_when_free(with_runner).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'runner.unreaped'",
        "1",
        Duration::from_secs(20),
    )
    .await?;
    let leader: u32 = query(&project, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    first.shutdown().await;
    let group = i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or("no group")?;
    assert!(
        rustix::process::test_kill_process_group(group).is_ok(),
        "the precondition: a group without its leader"
    );
    let reconciled = seq(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
    )?;
    let second = start_when_free(config(&project)).await?;
    until(
        &project,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.completed'",
        &(reconciled + 1).to_string(),
        Duration::from_secs(40),
    )
    .await?;
    let state = query(&project, "SELECT state FROM attempts WHERE task_id = 'T1'")?;
    // Stop the member by hand (the test's own).
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    assert_eq!(
        state, "RUNNING",
        "the attempt ended while its leaderless group lived"
    );
    second.shutdown().await;
    Ok(())
}
