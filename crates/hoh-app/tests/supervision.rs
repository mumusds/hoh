//! Commander supervision through a host with its runner (REQ-021, docs/adr/ADR-025.md): TEST-021-04. A FakeCommander
//! executor answers each supervision turn through its file bridge with one decision naming the values its turn was
//! given; work dispatches are never taken, so a leased task's attempt never starts.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig};
use hoh_core::decision::Context;
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
    config.jitter_seed = Some(0x0021_0004);
    config
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

/// Run `sql` (with `:p` the host's project id) against the project's database.
fn seed(project: &Path, sql: &str) -> R {
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1], timeout=10); \
                  p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
                  c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit(); c.close()";
    python(&db(project), script, sql).map(drop)
}

/// Every row of `sql` as an object of its columns.
fn rows(db: &Path, sql: &str) -> R<Value> {
    Ok(serde_json::from_str(&python(
        db,
        "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1], timeout=10); c.row_factory=sqlite3.Row; \
         print(json.dumps([dict(r) for r in c.execute(sys.stdin.read())]))",
        sql,
    )?)?)
}

/// The system time plus what the test adds.
#[derive(Debug, Default)]
struct FakeClock(std::sync::Mutex<Duration>);

impl FakeClock {
    fn advance(&self, by: Duration) {
        if let Ok(mut ahead) = self.0.lock() {
            *ahead += by;
        }
    }
}

impl hoh_core::delivery::Clock for FakeClock {
    fn now(&self) -> SystemTime {
        SystemTime::now() + self.0.lock().map(|d| *d).unwrap_or_default()
    }
}

/// Answers each supervision turn with one record_assumption decision naming the revision and epoch the turn was given;
/// its first turn only once `release` is set. Takes no work dispatch.
#[derive(Debug, Default)]
struct FakeCommander {
    release: Arc<AtomicBool>,
    turns: Arc<std::sync::Mutex<u32>>,
}

impl hoh_app::host::Executor for FakeCommander {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        if work.role == "commander" {
            let _ = answer.send(Ok(()));
        } else {
            let _ = answer.send(Err("the fixture takes no work".to_owned()));
        }
        ack
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let input = work
            .assignment
            .supervision
            .clone()
            .ok_or_else(|| hoh_app::host::Refusal {
                reason: "NOT_SUPERVISION".to_owned(),
                class: hoh_core::retry::FailureClass::Capability,
                detail: None,
                details: None,
            })?;
        let first = self.turns.lock().map(|mut n| {
            *n += 1;
            *n == 1
        });
        let hold = first.unwrap_or(false);
        let mut child = Command::new("sh")
            .args(["-c", "read -r go; exit 0"])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| hoh_app::host::Refusal {
                reason: "SPAWN".to_owned(),
                class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                detail: Some(e.to_string()),
                details: None,
            })?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let release = self.release.clone();
        let gate = hoh_app::host::Gate::new(move || {
            use std::io::Write as _;
            let mut stdin = stdin.ok_or("no stdin")?;
            std::thread::spawn(move || {
                while hold && !release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = stdin.write_all(b"go\n");
            });
            Ok(())
        });
        let (end, ended) = tokio::sync::oneshot::channel();
        let attempt = work.attempt_id.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let until = Instant::now() + Duration::from_secs(5);
            let mut group_gone = hoh_agents::identity::group_empty(pid);
            while !group_gone && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
                group_gone = hoh_agents::identity::group_empty(pid);
            }
            let message = json!({
                "schema_version": "1.0", "message_id": "answer", "run_id": "R", "task_id": "-",
                "attempt_id": attempt, "sender_role": "commander", "recipient_role": "runtime",
                "role_epoch": input.role_epoch, "correlation_id": "c", "causation_id": input.wake,
                "idempotency_key": "k", "expected_revision": input.run_revision,
                "spec_sha256": format!("{:064}", 1), "candidate_id": null, "kind": "decision",
                "payload": {"kind": "record_assumption", "statement": "T stays as it is", "basis": "its revision"}
            });
            let _ = end.send(hoh_app::host::Outcome {
                success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: "answered".to_owned(),
                group_gone,
                class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                observed: hoh_core::persistence::Observed {
                    messages: vec![json!({"nonce": input.nonce, "message": message}).to_string()],
                    ..Default::default()
                },
                unknown_effect: false,
            });
        });
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
        })
    }
}

/// Run R RUNNING at revision 7 with its task T READY at revision 7 (the developer's binding has no session yet, so no
/// round leases T), its Commander bound, and a need_decision about T.
const WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', :p, 's1', 'RUNNING', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{}'), ('sc', 'codex', 'commander', '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-07T00:00:00Z'),
  ('R', 'developer', 'claude', NULL, 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T', 'R', 's1', 'READY', 7, 'developer', 0, 1, 1);
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  (:p, 'nd-1', 'R', 'need_decision', '{\"task_id\": \"T\", \"fingerprint\": \"fp-T\"}', '2026-11-07T00:00:00Z');
";

/// Wait until `sql`'s rows satisfy `done`, moving `clock` on by a second each time; the rows.
fn wait_for(db: &Path, clock: &FakeClock, sql: &str, done: &dyn Fn(&Value) -> bool) -> R<Value> {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let found = rows(db, sql)?;
        if done(&found) {
            return Ok(found);
        }
        if Instant::now() > until {
            return Err(format!("not reached: {sql}: {found}").into());
        }
        clock.advance(Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// AC-021-04: the wake on T is given a turn when T is at revision 7; before the Commander answers, a round leases T
/// (run and T both to 8); the answer names revision 7 and is REJECTED STALE_REVISION, T still at 8 and nothing
/// dispatched by it; exactly one re-trigger, whose turn is shown T at 8; its answer, naming 8, is APPLIED.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_04_a_stale_answer_is_rejected_and_its_retrigger_answered_at_the_new_revision() -> R
{
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    seed(&project, WORLD)?;
    let clock = Arc::new(FakeClock::default());
    let commander = Arc::new(FakeCommander::default());
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(clock.clone());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: commander.clone(),
        context: Context {
            required_capabilities: [("developer".to_owned(), Default::default())].into(),
            ..Context::default()
        },
    });
    let host = Host::start(config).await?;
    let db = db(&project);
    // The first turn runs (held), shown T at 7.
    wait_for(
        &db,
        &clock,
        "SELECT a.state FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'commander'",
        &|r| r[0]["state"] == "RUNNING",
    )?;
    let first = rows(
        &db,
        "SELECT json_extract(payload_json, '$.snapshots') AS snapshots, json_extract(payload_json, '$.run_revision') AS run \
         FROM events WHERE kind = 'supervisor.allocated'",
    )?;
    assert_eq!(first[0]["run"], json!(7));
    assert_eq!(first[0]["snapshots"], json!("{\"T\":7}"));
    // The developer gets its session: a round leases T (both to 8); its attempt is never taken, so T stays at 8.
    seed(
        &project,
        "UPDATE role_bindings SET session_id = 'sd' WHERE run_id = 'R' AND role = 'developer';",
    )?;
    wait_for(
        &db,
        &clock,
        "SELECT t.state, t.revision AS task, r.revision AS run FROM tasks t, runs r WHERE t.task_id = 'T' AND r.run_id = 'R'",
        &|r| r[0]["state"] == "LEASED",
    )?;
    let dispatches = rows(
        &db,
        "SELECT count(*) AS n FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch'",
    )?;
    commander.release.store(true, Ordering::SeqCst);
    let rejected = wait_for(
        &db,
        &clock,
        "SELECT seq, json_extract(payload_json, '$.reason_code') AS reason FROM events WHERE kind = 'decision.rejected'",
        &|r| r.as_array().is_some_and(|a| !a.is_empty()),
    )?;
    assert_eq!(rejected[0]["reason"], json!("STALE_REVISION"));
    let at_rejection = rows(
        &db,
        &format!(
            "SELECT (SELECT revision FROM tasks WHERE task_id = 'T') AS task, (SELECT count(*) FROM outbox o JOIN events e \
             ON e.seq = o.event_seq WHERE e.seq >= {} AND json_extract(o.payload_json, '$.kind') = 'dispatch') AS new",
            rejected[0]["seq"]
        ),
    )?;
    assert_eq!(at_rejection[0]["task"], json!(8));
    assert_eq!(
        at_rejection[0]["new"],
        json!(0),
        "no dispatch by the stale decision"
    );
    // The re-trigger: one new trigger, its turn shown T at 8, its answer APPLIED.
    let applied = wait_for(
        &db,
        &clock,
        "SELECT json_extract(payload_json, '$.decision_id') AS id FROM events WHERE kind = 'decision.applied'",
        &|r| r.as_array().is_some_and(|a| !a.is_empty()),
    )?;
    assert_eq!(applied.as_array().map(Vec::len), Some(1));
    let retriggers = rows(
        &db,
        "SELECT count(*) AS n FROM supervisor_triggers WHERE fingerprint LIKE '%#retrigger%'",
    )?;
    assert_eq!(retriggers[0]["n"], json!(1));
    let second = rows(
        &db,
        "SELECT json_extract(payload_json, '$.snapshots') AS snapshots, json_extract(payload_json, '$.run_revision') AS run \
         FROM events WHERE kind = 'supervisor.allocated' ORDER BY seq DESC LIMIT 1",
    )?;
    assert_eq!(second[0]["snapshots"], json!("{\"T\":8}"));
    assert_eq!(second[0]["run"], json!(8));
    let _ = dispatches;
    host.shutdown().await;
    Ok(())
}
