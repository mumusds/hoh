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
    config
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

    fn start(&self, work: &hoh_app::host::Work) -> Result<hoh_app::host::Started, String> {
        use std::os::unix::process::CommandExt as _;
        if work.role == "commander" {
            self.commander.fetch_add(1, Ordering::SeqCst);
            return Err("the Commander is down".to_owned());
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
    Ok((project.clone(), Host::start(config).await?))
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
