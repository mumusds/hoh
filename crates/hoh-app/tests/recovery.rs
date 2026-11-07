//! The recovery check (REQ-036, REQ-041; docs/adr/ADR-019.md): kill -9 matrices and the reconcile at a host's start.
//! Each host runs in a process of its own (this test binary's `recovery_host` entry); the faults are made by the
//! test's hook implementations, passed in process (CR-006 F2/F3): the product's hooks do nothing.

use hoh_app::host::{Host, HostConfig};
use hoh_core::decision::{Context, Proposal};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use hoh_core::persistence::{FaultHooks, WritePoint};
use hoh_core::protocol::Message;
use serde_json::{Value, json};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CHILD_PROJECT: &str = "HOH_TEST_RECOVERY_PROJECT";
/// `<job>:<point>`: the child kills itself when its writer reaches that point of such a request.
const CHILD_KILL: &str = "HOH_TEST_RECOVERY_KILL";
/// A decision Message (JSON) the child submits once its host has started.
const CHILD_DECIDE: &str = "HOH_TEST_RECOVERY_DECIDE";
/// The reconcile's stop graces in milliseconds (SIGTERM to SIGKILL, SIGKILL to gone).
const CHILD_GRACE_MS: &str = "HOH_TEST_RECOVERY_GRACE_MS";
/// A JSON file `{"available": {marker: ref}, "unavailable": [marker]}`: the FakeRemote the reconcile looks in.
const CHILD_REMOTE: &str = "HOH_TEST_RECOVERY_REMOTE";
/// A directory with the FakeAgent's counter file and markers: the child runs a runner with the FakeExecutor and looks
/// for work effects by their markers.
const CHILD_WORK: &str = "HOH_TEST_RECOVERY_WORK";
/// A runner point: the child kills itself when an attempt reaches it.
const CHILD_RUNNER_KILL: &str = "HOH_TEST_RECOVERY_RUNNER_KILL";
/// The child's exit code when it submitted its decision and was not killed.
const NOT_KILLED: i32 = 77;

/// The product binary's WAL self-test probe.
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
    config
}

/// Ends its own process with SIGKILL when the writer reaches `point` of a `job` request (CR-006 F2: a SIGKILL the
/// process sends itself, made by test code).
#[derive(Debug)]
struct KillAt {
    job: String,
    point: WritePoint,
}

impl FaultHooks for KillAt {
    fn at(&self, job: &'static str, point: WritePoint) {
        if job == self.job && point == self.point {
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            );
            // SIGKILL cannot be caught; nothing after it runs.
            loop {
                std::thread::park();
            }
        }
    }
}

/// A local fake forge (AC-041-05): pull requests by run marker (the effect key), some markers unreachable. Every call
/// is appended to `<file>.calls` as `query <marker>` or `create <marker>`; the reconcile only ever queries.
#[derive(Debug)]
struct FakeRemote(PathBuf);

impl FakeRemote {
    fn log(&self, line: &str) {
        use std::io::Write as _;
        let mut calls = self.0.clone().into_os_string();
        calls.push(".calls");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(calls)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

impl hoh_core::reconcile::Observer for FakeRemote {
    fn observe(&self, _attempt_id: &str, marker: &str) -> hoh_core::reconcile::Observation {
        use hoh_core::reconcile::Observation;
        self.log(&format!("query {marker}"));
        let remote: Value = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if remote["unavailable"]
            .as_array()
            .is_some_and(|m| m.iter().any(|x| x == marker))
        {
            return Observation::Unavailable {
                detail: "the forge did not answer".to_owned(),
            };
        }
        match remote["available"][marker].as_str() {
            Some(pr) => Observation::Found {
                external_ref: pr.to_owned(),
            },
            None => Observation::Absent,
        }
    }
}

/// The FakeAgent (AC-041-01): appends its effect key to the counter file when it starts executing, works a moment,
/// then leaves its marker (the key, `/` as `_`) when it is done.
const FAKE_AGENT: &str =
    "printf '%s\\n' \"$1\" >> \"$2/counter\"; sleep 0.2; : > \"$2/markers/$3\"";

fn marker_name(key: &str) -> String {
    key.replace('/', "_")
}

/// Runs each attempt's work as a FakeAgent, the leader of its own process group (as the substrate starts agents).
#[derive(Debug)]
struct FakeExecutor(PathBuf);

impl hoh_app::host::Executor for FakeExecutor {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        let _ = answer.send(Ok(()));
        ack
    }

    fn start(&self, work: &hoh_app::host::Work) -> Result<hoh_app::host::Started, String> {
        use std::os::unix::process::CommandExt as _;
        let mut child = Command::new("sh")
            .args(["-c", FAKE_AGENT, "fake-agent", &work.effect_key])
            .arg(&self.0)
            .arg(marker_name(&work.effect_key))
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        let (end, ended) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = end.send(hoh_app::host::Outcome {
                success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: format!("{status:?}"),
            });
        });
        Ok(hoh_app::host::Started { pid, ended })
    }
}

/// Looks for an attempt's work by the FakeAgent's marker.
#[derive(Debug)]
struct Markers(PathBuf);

impl hoh_core::reconcile::Observer for Markers {
    fn observe(&self, _attempt_id: &str, key: &str) -> hoh_core::reconcile::Observation {
        let marker = self.0.join("markers").join(marker_name(key));
        if marker.exists() {
            hoh_core::reconcile::Observation::Found {
                external_ref: marker.to_string_lossy().into_owned(),
            }
        } else {
            hoh_core::reconcile::Observation::Absent
        }
    }
}

/// Ends its own process with SIGKILL when an attempt reaches `0` (CR-006 F2).
#[derive(Debug)]
struct KillAtRunner(hoh_app::host::RunnerPoint);

impl hoh_app::host::HostHooks for KillAtRunner {
    fn runner(&self, point: hoh_app::host::RunnerPoint) {
        if point == self.0 {
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            );
            loop {
                std::thread::park();
            }
        }
    }
}

fn runner_point(name: &str) -> Option<hoh_app::host::RunnerPoint> {
    use hoh_app::host::RunnerPoint;
    Some(match name {
        "BeforeSend" => RunnerPoint::BeforeSend,
        "AfterSend" => RunnerPoint::AfterSend,
        "AfterAck" => RunnerPoint::AfterAck,
        "AfterEffectStarted" => RunnerPoint::AfterEffectStarted,
        "AfterEffectCommitted" => RunnerPoint::AfterEffectCommitted,
        _ => return None,
    })
}

fn point_named(name: &str) -> Option<WritePoint> {
    Some(match name {
        "Begun" => WritePoint::Begun,
        "StateWritten" => WritePoint::StateWritten,
        "EventsWritten" => WritePoint::EventsWritten,
        "OutboxWritten" => WritePoint::OutboxWritten,
        "Committed" => WritePoint::Committed,
        _ => return None,
    })
}

/// The child entry: a host built in the test target. With [`CHILD_DECIDE`] it starts, submits the decision and is
/// expected to die at its kill point; without, it runs as `hoh_app::host::run` until SIGTERM.
#[test]
#[ignore = "run by the recovery tests in a process of their own"]
fn recovery_host() {
    let Some(project) = std::env::var_os(CHILD_PROJECT) else {
        return;
    };
    let mut config = config(Path::new(&project));
    if let Ok(kill) = std::env::var(CHILD_KILL) {
        let (job, point) = kill.split_once(':').expect("<job>:<point>");
        config.store.faults = Arc::new(KillAt {
            job: job.to_owned(),
            point: point_named(point).expect("a write point"),
        });
    }
    if let Some(ms) = std::env::var(CHILD_GRACE_MS)
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
    {
        config.reconcile.term_grace = Duration::from_millis(ms);
        config.reconcile.kill_grace = Duration::from_millis(ms.max(2000));
    }
    if let Some(remote) = std::env::var_os(CHILD_REMOTE) {
        config.reconcile.observer = Arc::new(FakeRemote(PathBuf::from(remote)));
    }
    if let Some(work) = std::env::var_os(CHILD_WORK) {
        let developer = ["structured_events", "command_exec"].map(str::to_owned);
        config.runner = Some(hoh_app::host::RunnerConfig {
            executor: Arc::new(FakeExecutor(PathBuf::from(&work))),
            context: Context {
                required_capabilities: [("developer".to_owned(), developer.into_iter().collect())]
                    .into(),
                ..Context::default()
            },
        });
        config.reconcile.observer = Arc::new(Markers(PathBuf::from(work)));
    }
    let runner_kill = std::env::var(CHILD_RUNNER_KILL).ok();
    if let Some(point) = &runner_kill {
        config.hooks = Arc::new(KillAtRunner(runner_point(point).expect("a runner point")));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    if let Ok(decision) = std::env::var(CHILD_DECIDE) {
        let code = runtime.block_on(async {
            let host = Host::start(config).await.expect("the host starts");
            let message: Message = serde_json::from_str(&decision).expect("a decision message");
            let proposal = Proposal::from_message(&message).expect("a proposal");
            let decided = host.store().decide(proposal, Context::default()).await;
            if runner_kill.is_some() {
                // The runner reaches its kill point while this waits.
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            eprintln!("decided without being killed: {decided:?}");
            NOT_KILLED
        });
        std::process::exit(code);
    }
    let code = runtime.block_on(hoh_app::host::run(config));
    std::process::exit(i32::from(code));
}

/// A child process that is always ended and reaped, whatever the test does.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn child(project: &Path) -> R<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "recovery_host",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_PROJECT, project)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    Ok(command)
}

/// Wait for `child` to end, at most `limit`; its status and stderr.
fn wait(mut child: ChildGuard, limit: Duration) -> R<(std::process::ExitStatus, String)> {
    let end = Instant::now() + limit;
    loop {
        if let Some(status) = child.0.try_wait()? {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.0.stderr.take() {
                use std::io::Read as _;
                let _ = pipe.read_to_string(&mut stderr);
            }
            return Ok((status, stderr));
        }
        if Instant::now() > end {
            return Err("the host process did not end in time".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The pid in the project's host.json, if there is one (a host killed by SIGKILL leaves its file behind).
fn announced(project: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(project.join(".hoh/host.json")).ok()?;
    serde_json::from_str::<Value>(&text).ok()?["pid"].as_u64()
}

/// Start a host (no faults, no command), wait until it serves (its own host.json, not one a killed host left), stop
/// it with SIGTERM: its exit code and stderr.
fn serve_and_stop(project: &Path) -> R<(i32, String)> {
    serve_and_stop_with(project, &[])
}

/// [`serve_and_stop`] with these child variables.
fn serve_and_stop_with(project: &Path, env: &[(&str, &std::ffi::OsStr)]) -> R<(i32, String)> {
    let mut command = child(project)?;
    for (name, value) in env {
        command.env(name, value);
    }
    let mut guard = ChildGuard(command.spawn()?);
    let own = u64::from(guard.0.id());
    let end = Instant::now() + Duration::from_secs(30);
    while announced(project) != Some(own) {
        if let Some(status) = guard.0.try_wait()? {
            return Err(format!("the host ended before it served: {status:?}").into());
        }
        if Instant::now() > end {
            return Err("the host did not start".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = rustix::process::Pid::from_raw(i32::try_from(guard.0.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let (status, stderr) = wait(guard, Duration::from_secs(30))?;
    Ok((
        status
            .code()
            .ok_or_else(|| format!("ended by a signal ({status:?}): {stderr}"))?,
        stderr,
    ))
}

/// Run `script` (Python, sqlite3 module) against `db`; its stdout. Test code: the product never writes outside
/// persistence.
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

/// Write the evidence file `name` when the check gives a directory for it.
fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

fn sha(n: u32) -> String {
    format!("{n:064}")
}

/// AC-036-02's world in the host's project: run R READY at revision 7 (spec s2, the Commander bound at epoch 3), its
/// task T1 READY for the developer.
const READY_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', :p, 's2', 'READY', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{\"structured_events\": true, \"resume\": true}'),
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 3, 1, '2026-11-07T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's2', 'READY', 1, 'developer', 1, 1, 1);
";

/// Seed `sql` (with `:p` the host's project id) into the project's database.
fn seed(project: &Path, sql: &str) -> R {
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); \
                  p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
                  c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit(); c.close()";
    python(&db(project), script, sql).map(drop)
}

/// The Commander's decision to start run R: READY -> RUNNING at revision 7.
fn start_run() -> Value {
    json!({
        "schema_version": "1.0", "message_id": "d-start", "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 3, "correlation_id": "c",
        "causation_id": null, "idempotency_key": "k-start", "expected_revision": 7, "spec_sha256": sha(2),
        "candidate_id": null, "kind": "decision",
        "payload": {"kind": "transition_run", "to_state": "RUNNING"}
    })
}

/// What TEST-036-02 reads after the restart: the run, the transition's events and outbox rows, the integrity check
/// and the outbox rows whose event is missing.
const READ_TRANSITION: &str = r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
run = c.execute("SELECT state, revision FROM runs WHERE run_id = 'R'").fetchone()
events = [r[0] for r in c.execute(
    "SELECT seq FROM events WHERE kind = 'decision.applied' AND json_extract(payload_json, '$.decision_id') = 'd-start'")]
outbox = [r[0] for r in c.execute(
    "SELECT event_seq FROM outbox WHERE json_extract(payload_json, '$.decision_id') = 'd-start'")]
decisions = c.execute("SELECT count(*) FROM decisions WHERE decision_id = 'd-start'").fetchone()[0]
orphans = c.execute("SELECT count(*) FROM outbox WHERE event_seq NOT IN (SELECT seq FROM events)").fetchone()[0]
check = [r[0] for r in c.execute("PRAGMA integrity_check")]
print(json.dumps({"run": list(run), "events": events, "outbox": outbox, "decisions": decisions,
                  "orphans": orphans, "integrity": check}))
"#;

/// Copy the database files of `from` to `to` (the files that exist).
fn copy_db(from: &Path, to: &Path) -> R {
    std::fs::create_dir_all(to)?;
    for suffix in ["", "-wal", "-shm"] {
        let name = format!("state.db{suffix}");
        let _ = std::fs::remove_file(to.join(&name));
        if from.join(&name).exists() {
            std::fs::copy(from.join(&name), to.join(&name))?;
        }
    }
    Ok(())
}

/// AC-036-02: a host killed (SIGKILL by itself) at each of the five points of the transaction that moves run R
/// READY -> RUNNING and queues its notice, 20 times each, then restarted without any command: before the commit the
/// run is still READY at its revision with none of the transition's events or outbox rows; after it, RUNNING with
/// exactly one of each, the outbox row pointing at the event; every integrity check is ok and no outbox row points
/// at a missing event.
#[test]
fn test_036_02_a_transition_killed_at_any_point_is_all_or_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    // The fixture: the host makes the database (its project), the world goes in, the host's files are kept.
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, READY_WORLD)?;
    let template = dir.path().join("template");
    copy_db(&project.join(".hoh"), &template)?;
    let decision = start_run().to_string();
    let points = [
        "Begun",
        "StateWritten",
        "EventsWritten",
        "OutboxWritten",
        "Committed",
    ];
    let mut runs = Vec::new();
    for point in points {
        for round in 0..20 {
            copy_db(&template, &project.join(".hoh"))?;
            let killed = ChildGuard(
                child(&project)?
                    .env(CHILD_KILL, format!("Decide:{point}"))
                    .env(CHILD_DECIDE, &decision)
                    .spawn()?,
            );
            let (status, stderr) = wait(killed, Duration::from_secs(60))?;
            assert_eq!(
                status.signal(),
                Some(9),
                "{point} #{round}: not killed at its point ({status:?}): {stderr}"
            );
            let (code, stderr) = serve_and_stop(&project)?;
            assert_eq!(code, 0, "{point} #{round}: the restarted host: {stderr}");
            let read: Value = serde_json::from_str(&python(&db(&project), READ_TRANSITION, "")?)?;
            let committed = point == "Committed";
            let expected = if committed {
                (json!(["RUNNING", 8]), 1, 1)
            } else {
                (json!(["READY", 7]), 0, 0)
            };
            assert_eq!(
                (
                    &read["run"],
                    read["events"].as_array().map_or(usize::MAX, Vec::len),
                    read["outbox"].as_array().map_or(usize::MAX, Vec::len)
                ),
                (&expected.0, expected.1, expected.2),
                "{point} #{round}: {read}"
            );
            if committed {
                assert_eq!(read["outbox"][0], read["events"][0], "{point} #{round}");
            }
            assert_eq!(
                read["decisions"],
                json!(i32::from(committed)),
                "{point} #{round}"
            );
            assert_eq!(read["orphans"], json!(0), "{point} #{round}");
            assert_eq!(read["integrity"], json!(["ok"]), "{point} #{round}");
            runs.push(json!({"point": point, "round": round, "signal": 9, "read": read}));
        }
    }
    keep(
        "test-036-02.json",
        &json!({"runs": runs, "count": runs.len()}),
    )?;
    Ok(())
}

/// A helper process unrelated to any attempt that records every catchable signal it gets, one line each, in `log`.
fn signal_counter(log: &Path) -> R<ChildGuard> {
    let script = format!(
        "for s in TERM INT HUP USR1 USR2 QUIT; do trap \"echo $s >> '{}'\" $s; done; while :; do sleep 0.05; done",
        log.display()
    );
    Ok(ChildGuard(
        Command::new("sh")
            .args(["-c", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    ))
}

/// What reconcile events and attempts AC-041-03 and AC-041-05 read.
const READ_RECONCILE: &str = r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
attempts = {r[0]: r[1] for r in c.execute("SELECT attempt_id, state FROM attempts")}
events = [{"seq": r[0], "kind": r[1], "payload": json.loads(r[2])} for r in c.execute(
    "SELECT seq, kind, payload_json FROM events WHERE kind LIKE 'reconcile.%' ORDER BY seq")]
effects = {r[0]: {"state": r[1], "external_ref": r[2]} for r in c.execute(
    "SELECT effect_key, state, external_ref FROM effects")}
tasks = {r[0]: r[1] for r in c.execute("SELECT task_id, state FROM tasks")}
print(json.dumps({"attempts": attempts, "events": events, "effects": effects, "tasks": tasks}))
"#;

/// AC-036-02's world with run R RUNNING and two running tasks.
const RUNNING_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', :p, 's2', 'RUNNING', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T2', 'R', 's2', 'RUNNING', 3, 'developer', 0, 1, 1), ('T3', 'R', 's2', 'RUNNING', 3, 'developer', 0, 2, 1);
";

/// AC-041-03: attempt A2 records the pid of a live, unrelated process with this boot but another start time, A3 the
/// same pid with its start time but another boot. The restarted host's reconcile finds neither is ours, sends no
/// signal (the helper lives and logged none), records the basis in the reconcile events, and ends both (not RUNNING).
#[test]
fn test_041_03_a_reused_pid_or_another_boot_is_not_signalled() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let log = dir.path().join("signals.log");
    let helper = signal_counter(&log)?;
    let pid = helper.0.id();
    let me = hoh_agents::identity::of(pid)?.ok_or("the helper is not running")?;
    seed(
        &project,
        &format!(
            "{RUNNING_WORLD}
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, boot_id,
               process_start_id, started_at) VALUES
               ('A2', 'T2', 'RUNNING', 1, 1, 1, {pid}, '{boot}', '{wrong_start}', '2026-11-07T00:00:00Z'),
               ('A3', 'T3', 'RUNNING', 1, 1, 1, {pid}, '{other_boot}', '{start}', '2026-11-07T00:00:00Z');",
            boot = me.boot_id,
            wrong_start = me.start_id.clone() + "1",
            other_boot = "00000000-0000-0000-0000-000000000000",
            start = me.start_id,
        ),
    )?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "the restarted host: {stderr}");
    let read: Value = serde_json::from_str(&python(&db(&project), READ_RECONCILE, "")?)?;
    // The helper lives on, the same process, and got no signal.
    assert_eq!(
        hoh_agents::identity::start_id(pid)?.as_deref(),
        Some(me.start_id.as_str())
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap_or_default(), "");
    for (attempt, basis) in [("A2", "start_time_differs"), ("A3", "boot_id_differs")] {
        let state = read["attempts"][attempt].as_str().unwrap_or_default();
        assert!(
            ["SETTLED", "FAILED", "UNKNOWN_EFFECT"].contains(&state),
            "{attempt}: {state}"
        );
        let event = read["events"]
            .as_array()
            .and_then(|events| {
                events.iter().find(|e| {
                    e["kind"] == "reconcile.attempt" && e["payload"]["attempt_id"] == attempt
                })
            })
            .ok_or("no reconcile event")?;
        assert_eq!(
            (
                &event["payload"]["process"]["found"],
                &event["payload"]["process"]["basis"],
                &event["payload"]["process"]["signals"]
            ),
            (&json!("gone"), &json!(basis), &json!(0)),
            "{attempt}: {event}"
        );
    }
    let kinds: Vec<&str> = read["events"]
        .as_array()
        .ok_or("no events")?
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert_eq!(kinds.first(), Some(&"reconcile.started"));
    assert_eq!(kinds.last(), Some(&"reconcile.completed"));
    keep(
        "test-041-03.json",
        &json!({"helper": {"pid": pid, "boot_id": me.boot_id, "start_id": me.start_id, "signals": 0}, "read": read}),
    )?;
    drop(helper);
    Ok(())
}

/// An old host's agent still running when the new host starts (its record names it: pid, boot and start time): the
/// reconcile stops it (SIGTERM to its group, which it ignores, then SIGKILL) before it disposes of the attempt.
#[test]
fn an_old_hosts_live_agent_is_stopped_before_its_attempt_is_disposed_of() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let log = dir.path().join("signals.log");
    // An orphan leading its own group, as an old host's agent is after a kill -9 (the substrate starts each agent as
    // a group leader; its parent is gone, so another process reaps it).
    let script = format!(
        "setsid sh -c 'trap \"echo TERM >> {}\" TERM; while :; do sleep 0.05; done' </dev/null >/dev/null 2>&1 & echo $!",
        log.display()
    );
    let out = Command::new("sh").args(["-c", &script]).output()?;
    let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse()?;
    let end = Instant::now() + Duration::from_secs(5);
    while hoh_agents::process::group_members(pid).is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let me = hoh_agents::identity::of(pid)?.ok_or("the agent is not running")?;
    seed(
        &project,
        &format!(
            "{RUNNING_WORLD}
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, boot_id,
               process_start_id, started_at) VALUES
               ('A2', 'T2', 'RUNNING', 1, 1, 1, {pid}, '{boot}', '{start}', '2026-11-07T00:00:00Z');",
            boot = me.boot_id,
            start = me.start_id,
        ),
    )?;
    let (code, stderr) = serve_and_stop_with(&project, &[(CHILD_GRACE_MS, "300".as_ref())])?;
    assert_eq!(code, 0, "the restarted host: {stderr}");
    let read: Value = serde_json::from_str(&python(&db(&project), READ_RECONCILE, "")?)?;
    assert_eq!(std::fs::read_to_string(&log)?.trim(), "TERM");
    let event = read["events"]
        .as_array()
        .and_then(|events| events.iter().find(|e| e["kind"] == "reconcile.attempt"))
        .ok_or("no reconcile event")?;
    assert_eq!(
        event["payload"]["process"],
        json!({"found": "ours", "steps": ["sigterm", "sigkill", "reaped"], "reaped": true})
    );
    assert_eq!(read["attempts"]["A2"], json!("FAILED"));
    assert_eq!(read["tasks"]["T2"], json!("READY"));
    assert!(hoh_agents::process::group_members(pid).is_empty());
    Ok(())
}

/// AC-041-05: effect E (a pull request) STARTED and the FakeRemote already has one with its marker; E2 STARTED and
/// the FakeRemote cannot be asked. The reconcile commits E with the existing PR as its external ref, makes E2
/// UNKNOWN and its attempt UNKNOWN_EFFECT with a diagnostic naming E2, and never creates anything.
#[test]
fn test_041_05_a_started_effect_is_found_or_unknown_never_done_again() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(
        &project,
        &format!(
            "{RUNNING_WORLD}
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
               ('AE', 'T2', 'RUNNING', 1, 1, 1, '2026-11-07T00:00:00Z'),
               ('AE2', 'T3', 'RUNNING', 1, 1, 1, '2026-11-07T00:00:00Z');
             INSERT INTO effects(effect_key, attempt_id, state) VALUES ('pr:R/T2', 'AE', 'STARTED'), ('pr:R/T3', 'AE2', 'STARTED');"
        ),
    )?;
    let remote = dir.path().join("forge.json");
    std::fs::write(
        &remote,
        json!({"available": {"pr:R/T2": "https://forge.invalid/pr/17"}, "unavailable": ["pr:R/T3"]}).to_string(),
    )?;
    let (code, stderr) = serve_and_stop_with(&project, &[(CHILD_REMOTE, remote.as_os_str())])?;
    assert_eq!(code, 0, "the restarted host: {stderr}");
    let read: Value = serde_json::from_str(&python(&db(&project), READ_RECONCILE, "")?)?;
    let e = &read["effects"]["pr:R/T2"];
    assert!(
        ["OBSERVED", "COMMITTED"].contains(&e["state"].as_str().unwrap_or_default()),
        "{e}"
    );
    assert_eq!(e["external_ref"], json!("https://forge.invalid/pr/17"));
    assert_eq!(read["effects"]["pr:R/T3"]["state"], json!("UNKNOWN"));
    assert_eq!(read["attempts"]["AE2"], json!("UNKNOWN_EFFECT"));
    // No new attempt for its task, which stays where it was.
    assert_eq!(read["tasks"]["T3"], json!("RUNNING"));
    let diagnostic = read["events"]
        .as_array()
        .and_then(|events| {
            events.iter().find(|e| {
                e["kind"] == "reconcile.diagnostic"
                    && e["payload"]["effect_keys"]
                        .as_array()
                        .is_some_and(|k| k.iter().any(|x| x == "pr:R/T3"))
            })
        })
        .ok_or("no diagnostic naming E2")?;
    assert_eq!(diagnostic["payload"]["code"], json!("UNKNOWN_EFFECT"));
    let calls = std::fs::read_to_string(dir.path().join("forge.json.calls"))?;
    let mut lines: Vec<&str> = calls.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["query pr:R/T2", "query pr:R/T3"]);
    assert_eq!(lines.iter().filter(|l| l.starts_with("create")).count(), 0);
    keep(
        "test-041-05.json",
        &json!({"read": read, "forge_calls": lines}),
    )?;
    Ok(())
}

/// Start a host with these child variables, wait until it serves, then until `done` holds (at most `limit`), stop it
/// with SIGTERM: its exit code, its stderr, and whether `done` held.
fn serve_until(
    project: &Path,
    env: &[(&str, &std::ffi::OsStr)],
    limit: Duration,
    done: impl Fn() -> bool,
) -> R<(i32, String, bool)> {
    let mut command = child(project)?;
    for (name, value) in env {
        command.env(name, value);
    }
    let mut guard = ChildGuard(command.spawn()?);
    let own = u64::from(guard.0.id());
    let end = Instant::now() + Duration::from_secs(30);
    while announced(project) != Some(own) {
        if let Some(status) = guard.0.try_wait()? {
            return Err(format!("the host ended before it served: {status:?}").into());
        }
        if Instant::now() > end {
            return Err("the host did not start".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let end = Instant::now() + limit;
    let mut held = done();
    while !held && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
        held = done();
    }
    let pid = rustix::process::Pid::from_raw(i32::try_from(guard.0.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let (status, stderr) = wait(guard, Duration::from_secs(30))?;
    Ok((
        status
            .code()
            .ok_or_else(|| format!("ended by a signal ({status:?}): {stderr}"))?,
        stderr,
        held,
    ))
}

/// AC-041-01's world: run R READY (revision 7, the Commander at epoch 3) with five tasks for the developer, none a
/// writer (two run at a time, `execution.reader_slots`).
const FIVE_TASKS: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', :p, 's2', 'READY', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{\"structured_events\": true, \"resume\": true}'),
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 3, 1, '2026-11-07T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's2', 'READY', 1, 'developer', 0, 1, 1), ('T2', 'R', 's2', 'READY', 1, 'developer', 0, 2, 1),
  ('T3', 'R', 's2', 'READY', 1, 'developer', 0, 3, 1), ('T4', 'R', 's2', 'READY', 1, 'developer', 0, 4, 1),
  ('T5', 'R', 's2', 'READY', 1, 'developer', 0, 5, 1);
";

/// Every task IMPLEMENTED, or left with its attempt UNKNOWN_EFFECT: the run's next stable point.
const STABLE: &str = "SELECT count(*) FROM tasks t WHERE t.state != 'IMPLEMENTED' AND NOT EXISTS \
                      (SELECT 1 FROM attempts a WHERE a.task_id = t.task_id AND a.state = 'UNKNOWN_EFFECT')";

/// What TEST-041-01 reads after a restart.
const READ_RUN: &str = r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
attempts = [{"id": r[0], "task": r[1], "state": r[2], "host_epoch": r[3]} for r in c.execute(
    "SELECT attempt_id, task_id, state, host_epoch FROM attempts ORDER BY rowid")]
reconcile = [{"seq": r[0], "kind": r[1], "at": r[2], "payload": json.loads(r[3])} for r in c.execute(
    "SELECT seq, kind, created_at, payload_json FROM events WHERE kind LIKE 'reconcile.%' ORDER BY seq")]
dispatches = [r[0] for r in c.execute(
    "SELECT event_seq FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' ORDER BY event_seq")]
tasks = {r[0]: r[1] for r in c.execute("SELECT task_id, state FROM tasks")}
print(json.dumps({"attempts": attempts, "reconcile": reconcile, "dispatches": dispatches, "tasks": tasks,
                  "host_epoch": c.execute("SELECT max(host_epoch) FROM projects").fetchone()[0]}))
"#;

/// AC-041-01: a host killed (SIGKILL by itself) at each of five points of an attempt the runner runs (before the
/// dispatch is sent; sent, not answered; answered, its receipt not written; its work effect STARTED, not OBSERVED; its
/// work effect COMMITTED, its task not moved), ten times each, and restarted at once. Each run ends at its next
/// stable point with no effect key counted twice, every attempt of the killed host SETTLED, CANCELLED or
/// UNKNOWN_EFFECT, a task given another attempt only after its earlier one was judged never started, no new attempt
/// for a task whose attempt is UNKNOWN_EFFECT, the reconcile completed before the restarted host's first dispatch,
/// and the reconcile started within 30 s of the kill.
#[test]
fn test_041_01_kill_at_each_point_of_a_dispatch_never_runs_an_effect_twice() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, FIVE_TASKS)?;
    let template = dir.path().join("template");
    copy_db(&project.join(".hoh"), &template)?;
    let decision = start_run().to_string();
    let points = [
        "BeforeSend",
        "AfterSend",
        "AfterAck",
        "AfterEffectStarted",
        "AfterEffectCommitted",
    ];
    let mut runs = Vec::new();
    for point in points {
        for round in 0..10 {
            copy_db(&template, &project.join(".hoh"))?;
            let work = dir.path().join(format!("work-{point}-{round}"));
            std::fs::create_dir_all(work.join("markers"))?;
            let killed = ChildGuard(
                child(&project)?
                    .env(CHILD_RUNNER_KILL, point)
                    .env(CHILD_WORK, &work)
                    .env(CHILD_DECIDE, &decision)
                    .spawn()?,
            );
            let (status, stderr) = wait(killed, Duration::from_secs(90))?;
            let t0 = std::time::SystemTime::now();
            assert_eq!(
                status.signal(),
                Some(9),
                "{point} #{round}: not killed at its point ({status:?}): {stderr}"
            );
            let db = db(&project);
            let (code, stderr, stable) = serve_until(
                &project,
                &[(CHILD_WORK, work.as_os_str())],
                Duration::from_secs(60),
                || {
                    python(&db, &format!("import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute({STABLE:?}).fetchone()[0])"), "")
                    .is_ok_and(|n| n.trim() == "0")
                },
            )?;
            assert_eq!(code, 0, "{point} #{round}: the restarted host: {stderr}");
            assert!(
                stable,
                "{point} #{round}: the run did not reach a stable point: {stderr}"
            );
            let read: Value = serde_json::from_str(&python(&db, READ_RUN, "")?)?;
            let counted = std::fs::read_to_string(work.join("counter")).unwrap_or_default();
            let mut keys: Vec<&str> = counted.lines().collect();
            keys.sort_unstable();
            let before = keys.len();
            keys.dedup();
            assert_eq!(
                keys.len(),
                before,
                "{point} #{round}: an effect ran twice: {counted}"
            );
            // The killed host's epoch is the one before the restarted host's.
            let killed_epoch = read["host_epoch"].as_i64().ok_or("no epoch")? - 1;
            let attempts = read["attempts"].as_array().ok_or("no attempts")?;
            let restart_reconcile: Vec<&Value> = read["reconcile"]
                .as_array()
                .ok_or("no reconcile")?
                .iter()
                .collect();
            let started = restart_reconcile
                .iter()
                .rev()
                .find(|e| e["kind"] == "reconcile.started")
                .ok_or("no reconcile.started")?;
            let completed = restart_reconcile
                .iter()
                .rev()
                .find(|e| e["kind"] == "reconcile.completed")
                .ok_or("no reconcile.completed")?;
            let dispositions: std::collections::BTreeMap<&str, &str> = restart_reconcile
                .iter()
                .filter(|e| {
                    e["kind"] == "reconcile.attempt" && e["seq"].as_i64() > started["seq"].as_i64()
                })
                .filter_map(|e| {
                    Some((
                        e["payload"]["attempt_id"].as_str()?,
                        e["payload"]["disposition"].as_str()?,
                    ))
                })
                .collect();
            for a in attempts
                .iter()
                .filter(|a| a["host_epoch"].as_i64() == Some(killed_epoch))
            {
                let state = a["state"].as_str().unwrap_or_default();
                assert!(
                    ["SETTLED", "CANCELLED", "UNKNOWN_EFFECT"].contains(&state),
                    "{point} #{round}: old attempt {a}"
                );
            }
            for task in ["T1", "T2", "T3", "T4", "T5"] {
                let of: Vec<&Value> = attempts.iter().filter(|a| a["task"] == task).collect();
                for pair in of.windows(2) {
                    let earlier = pair[0]["id"].as_str().unwrap_or_default();
                    assert_ne!(
                        pair[0]["state"], "UNKNOWN_EFFECT",
                        "{point} #{round}: {task} got another attempt"
                    );
                    assert_eq!(
                        (
                            pair[0]["state"].as_str(),
                            dispositions.get(earlier).copied()
                        ),
                        (Some("CANCELLED"), Some("ended")),
                        "{point} #{round}: {task} got another attempt without a never-started judgement"
                    );
                }
            }
            let after: Vec<i64> = read["dispatches"]
                .as_array()
                .ok_or("no dispatches")?
                .iter()
                .filter_map(Value::as_i64)
                .filter(|seq| Some(*seq) > started["seq"].as_i64())
                .collect();
            if let Some(first) = after.first() {
                assert!(
                    completed["seq"].as_i64() < Some(*first),
                    "{point} #{round}: a dispatch before the reconcile completed"
                );
            }
            let reconcile_at =
                hoh_core::time::parse_rfc3339(started["at"].as_str().unwrap_or_default())
                    .ok_or("a bad reconcile time")?;
            let delay = reconcile_at.duration_since(t0).unwrap_or_default();
            assert!(
                delay <= Duration::from_secs(30),
                "{point} #{round}: reconcile after {delay:?}"
            );
            runs.push(json!({
                "point": point, "round": round, "signal": 9, "counted": counted.lines().count(),
                "keys": keys.len(), "reconcile_after_ms": delay.as_millis(), "dispositions": dispositions,
                "attempts": attempts, "tasks": read["tasks"],
            }));
        }
    }
    keep(
        "test-041-01.json",
        &json!({"runs": runs, "count": runs.len()}),
    )?;
    Ok(())
}
