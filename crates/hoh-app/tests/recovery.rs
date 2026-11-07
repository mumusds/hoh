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
    let mut guard = ChildGuard(child(project)?.spawn()?);
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
