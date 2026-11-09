//! Retries, blocking and budgets in a host with its runner (REQ-042; docs/adr/ADR-020.md): TEST-042-03 and
//! TEST-042-06. A scripted executor runs each attempt as a short process at its start gate and reports the failure
//! class its script gives the task; a FakeClock is moved past each persisted `next_retry_at` while nothing runs.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig};
use hoh_core::decision::Context;
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use hoh_core::retry::FailureClass;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
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
    config.jitter_seed = Some(0x0042_0003);
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

/// Seed `sql` (with `:p` the host's project id) into the project's database.
fn seed(project: &Path, sql: &str) -> R {
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); \
                  p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
                  c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit(); c.close()";
    python(&db(project), script, sql).map(drop)
}

/// The first column of the first row of `sql` ("" for none or NULL), read outside the product.
fn query(db: &Path, sql: &str) -> R<String> {
    Ok(python(
        db,
        "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); print('' if r is None or r[0] is None else r[0])",
        sql,
    )?
    .trim()
    .to_owned())
}

/// Every row of `sql` as an object of its columns.
fn rows(db: &Path, sql: &str) -> R<Value> {
    Ok(serde_json::from_str(&python(
        db,
        "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1]); c.row_factory=sqlite3.Row; \
         print(json.dumps([dict(r) for r in c.execute(sys.stdin.read())]))",
        sql,
    )?)?)
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

/// The system time plus what the test adds, so it never runs behind the time attempts are recorded at.
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

/// How each task's attempts end: `None` succeeds, `Some(class)` fails with that class.
#[derive(Debug)]
struct Scripted {
    script: BTreeMap<String, Option<FailureClass>>,
    /// The groups it started and has not seen empty (only those are killed when it is dropped, M2T19R-60).
    started: Arc<std::sync::Mutex<Vec<u32>>>,
}

impl hoh_app::host::Executor for Scripted {
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
        let failure = self.script.get(&work.task_id).copied().flatten();
        let code = if failure.is_some() { "3" } else { "0" };
        let mut child = Command::new("sh")
            .args(["-c", &format!("read -r go; exit {code}")])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        if let Ok(mut started) = self.started.lock() {
            started.push(pid);
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
        let started = self.started.clone();
        std::thread::spawn(move || {
            // The executor's contract: the group SIGKILLed while the leader is an unreaped zombie, the leader reaped,
            // then the group seen empty.
            if let Some(raw) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
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
            if group_gone && let Ok(mut started) = started.lock() {
                started.retain(|&p| p != pid);
            }
            let _ = end.send(hoh_app::host::Outcome {
                success: failure.is_none()
                    && status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: match failure {
                    Some(FailureClass::Auth) => "401: authentication required".to_owned(),
                    Some(_) => "HTTP 503".to_owned(),
                    None => format!("{status:?}"),
                },
                group_gone,
                class: failure.unwrap_or(FailureClass::Transient { retry_after: None }),
                observed: Default::default(),
                unknown_effect: false,
            });
        });
        Ok(hoh_app::host::Started { pid, gate, ended })
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        for pid in self.started.lock().map(|p| p.clone()).unwrap_or_default() {
            if let Some(pgid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
            }
        }
    }
}

/// The developer's and the QA's role contracts (REQ-020's initial values).
fn context() -> Context {
    let caps = ["structured_events", "command_exec"].map(str::to_owned);
    Context {
        required_capabilities: [
            ("developer".to_owned(), caps.iter().cloned().collect()),
            ("qa".to_owned(), caps.iter().cloned().collect()),
        ]
        .into(),
        ..Context::default()
    }
}

/// A host of `project` (its database made by a first start) with `world` seeded, running `executor` on `clock`.
async fn host_with(
    project: &Path,
    world: &str,
    executor: Arc<Scripted>,
    clock: Arc<FakeClock>,
) -> R<Host> {
    std::fs::create_dir_all(project)?;
    Host::start(config(project)).await?.shutdown().await;
    seed(project, world)?;
    let mut config = config(project);
    config.clock = hoh_app::host::HostClock(clock);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor,
        context: context(),
    });
    Ok(Host::start(config).await?)
}

/// Move `clock` past the next retry of `tasks` whenever none of their attempts is unfinished, until `done` holds or
/// `limit` passes; the clock's moves.
fn drive(
    db: &Path,
    clock: &FakeClock,
    tasks: &[&str],
    done: &dyn Fn() -> R<bool>,
    limit: Duration,
) -> R<Vec<Value>> {
    let list = tasks
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut moves = Vec::new();
    let end = Instant::now() + limit;
    while !done()? {
        if Instant::now() > end {
            return Err(format!("not done in {limit:?}; moves {moves:?}").into());
        }
        let busy = query(
            db,
            &format!(
                "SELECT count(*) FROM attempts WHERE task_id IN ({list}) AND state IN ('PREPARED', 'STARTING', 'RUNNING')"
            ),
        )?;
        let next = query(
            db,
            &format!(
                "SELECT min(next_retry_at) FROM tasks WHERE task_id IN ({list}) AND state = 'READY' AND next_retry_at IS NOT NULL"
            ),
        )?;
        if busy == "0"
            && let Some(next) = hoh_core::time::parse_rfc3339(&next)
        {
            let now = hoh_core::delivery::Clock::now(clock);
            if let Ok(ahead) = next.duration_since(now) {
                clock.advance(ahead + Duration::from_secs(1));
                moves.push(
                    json!({"to": hoh_core::time::rfc3339_utc(next), "by_s": ahead.as_secs() + 1}),
                );
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(moves)
}

/// AC-042-03's world: runs R1, R2 and R3 RUNNING, each with one reader task. R1's T1 and R3's T3 go to the developer
/// (bound to a session with the developer's capabilities); R2's T2 to the QA, whose session lacks command_exec.
const BLOCKING_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R1', :p, 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z'),
  ('R2', :p, 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:01Z'),
  ('R3', :p, 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:02Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}'),
  ('sq', 'pi', 'qa', '1', '{\"structured_events\": true, \"command_exec\": false}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R1', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z'),
  ('R2', 'qa', 'pi', 'sq', 1, 1, '2026-11-07T00:00:00Z'),
  ('R3', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R1', 's1', 'READY', 1, 'developer', 0, 1, 1),
  ('T2', 'R2', 's1', 'READY', 1, 'qa', 0, 2, 1),
  ('T3', 'R3', 's1', 'READY', 1, 'developer', 0, 3, 1);
";

/// AC-042-03: R1's agent fails authentication, R2's session lacks a required capability, R3's agent answers 503 nine
/// times. R1 is BLOCKED_AUTH after one attempt with no retry scheduled; R2 BLOCKED_CAPABILITY with no attempt; R3's
/// ninth failure ends its retries: no dispatch after it, T3 BLOCKED, a need_decision; none COMPLETED.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_042_03_auth_and_capability_block_at_once_and_nine_503s_end_in_a_decision() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    let executor = Arc::new(Scripted {
        script: [
            ("T1".to_owned(), Some(FailureClass::Auth)),
            (
                "T3".to_owned(),
                Some(FailureClass::Transient { retry_after: None }),
            ),
        ]
        .into(),
        started: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let clock = Arc::new(FakeClock::default());
    let host = host_with(&project, BLOCKING_WORLD, executor.clone(), clock.clone()).await?;
    let db = db(&project);
    let done = || -> R<bool> {
        Ok(query(
            &db,
            "SELECT group_concat(state || ':' || COALESCE(blocked_reason, '-'), ',') FROM \
             (SELECT state, blocked_reason FROM runs ORDER BY run_id)",
        )? == "BLOCKED:BLOCKED_AUTH,BLOCKED:BLOCKED_CAPABILITY,BLOCKED:NEED_DECISION")
    };
    let moves = drive(
        &db,
        &clock,
        &["T1", "T2", "T3"],
        &done,
        Duration::from_secs(90),
    )?;
    // R3 dispatches nothing more: a while later, with the clock moved far on, still nine attempts.
    clock.advance(Duration::from_secs(3_600));
    tokio::time::sleep(Duration::from_millis(800)).await;
    let attempts = |task: &str| {
        query(
            &db,
            &format!("SELECT count(*) FROM attempts WHERE task_id = '{task}'"),
        )
    };
    assert_eq!(attempts("T3")?, "9");
    assert_eq!(attempts("T2")?, "0");
    assert_eq!(attempts("T1")?, "1");
    assert_eq!(
        query(
            &db,
            "SELECT attempt_count || ':' || COALESCE(next_retry_at, 'NULL') || ':' || state FROM tasks WHERE task_id = 'T1'"
        )?,
        "1:NULL:BLOCKED"
    );
    assert_eq!(
        query(
            &db,
            "SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = 'T3'"
        )?,
        "BLOCKED:NEED_DECISION"
    );
    assert_eq!(
        query(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'need_decision' AND json_extract(payload_json, '$.task_id') = 'T3'"
        )?,
        "1"
    );
    assert_eq!(
        query(&db, "SELECT count(*) FROM runs WHERE state = 'COMPLETED'")?,
        "0"
    );
    keep(
        "test-042-03.json",
        &json!({
            "runs": rows(&db, "SELECT run_id, state, blocked_reason, blocked_reasons_json FROM runs ORDER BY run_id")?,
            "tasks": rows(&db, "SELECT task_id, run_id, state, attempt_count, next_retry_at, blocked_reason FROM tasks ORDER BY task_id")?,
            "attempts": rows(&db, "SELECT attempt_id, task_id, state, ended_at, failure_reason FROM attempts ORDER BY rowid")?,
            "events": rows(&db, "SELECT seq, kind, json_extract(payload_json, '$.task_id') AS task_id, run_id FROM events WHERE kind IN ('task.retry_scheduled', 'task.blocked', 'need_decision', 'run.waiting', 'run.resumed', 'run.blocked') ORDER BY seq")?,
            "clock_moves": moves,
        }),
    )?;
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// AC-042-06's world: runs Rc and Rd RUNNING, each with one developer task; Rc has a cost limit of 5 USD and the
/// price of its model is unknown (no price table), Rd has no cost limit.
const COST_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('Rc', :p, 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z'),
  ('Rd', :p, 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:01Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('Rc', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z'),
  ('Rd', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('Tc', 'Rc', 's1', 'READY', 1, 'developer', 0, 1, 1),
  ('Td', 'Rd', 's1', 'READY', 1, 'developer', 0, 2, 1);
INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, price_known) VALUES
  ('run', 'Rc', 'usd', 5, 0),
  ('run', 'Rd', 'usd', NULL, 0);
";

/// AC-042-06: Rc's cost limit cannot be enforced with an unknown price: BLOCKED (BUDGET_UNENFORCEABLE) before any
/// dispatch; Rd, with no cost limit, runs one round normally, its cost null and unknown; neither COMPLETED.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_042_06_a_cost_limit_with_an_unknown_price_blocks_before_any_dispatch() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    let executor = Arc::new(Scripted {
        script: [("Td".to_owned(), None)].into(),
        started: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let clock = Arc::new(FakeClock::default());
    let host = host_with(&project, COST_WORLD, executor.clone(), clock).await?;
    let db = db(&project);
    let end = Instant::now() + Duration::from_secs(30);
    while query(&db, "SELECT state FROM tasks WHERE task_id = 'Td'")? != "IMPLEMENTED"
        || query(&db, "SELECT state FROM runs WHERE run_id = 'Rc'")? != "BLOCKED"
    {
        if Instant::now() > end {
            return Err("Rc did not block or Rd did not finish its round".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        query(&db, "SELECT blocked_reason FROM runs WHERE run_id = 'Rc'")?,
        "BUDGET_UNENFORCEABLE"
    );
    assert_eq!(
        query(&db, "SELECT count(*) FROM attempts WHERE task_id = 'Tc'")?,
        "0"
    );
    assert_eq!(
        query(&db, "SELECT count(*) FROM attempts WHERE task_id = 'Td'")?,
        "1"
    );
    assert_eq!(
        query(
            &db,
            "SELECT count(*) FROM attempts WHERE task_id = 'Td' AND cost_usd IS NULL"
        )?,
        "1"
    );
    // The run's cost as its final JSON result carries it (CR-009): null and unknown.
    let reader = host.store().reader()?;
    let cost = |run: &str| -> R<Value> {
        let (usd, status) = reader.run_cost(run)?;
        Ok(json!({"cost_usd": usd, "cost_status": status}))
    };
    assert_eq!(
        cost("Rd")?,
        json!({"cost_usd": null, "cost_status": "unknown"})
    );
    assert_eq!(
        cost("Rc")?,
        json!({"cost_usd": null, "cost_status": "unknown"})
    );
    // A schema check of Rc's final JSON result (M2-T13 outputs it): built here from its runs row and its cost
    // projection, encoded against the schema, the cost null and unknown (M2T19R-62, -158).
    let (cost_usd, cost_status) = reader.run_cost("Rc")?;
    let result = hoh_core::protocol::encode(&hoh_core::protocol::RunResult {
        run_id: "Rc".to_owned(),
        state: match query(&db, "SELECT state FROM runs WHERE run_id = 'Rc'")?.as_str() {
            "BLOCKED" => hoh_core::protocol::RunState::Blocked,
            other => return Err(format!("Rc is {other}").into()),
        },
        exit_code: 3,
        blocked_reason: hoh_core::protocol::BlockedReason::parse(&query(
            &db,
            "SELECT blocked_reason FROM runs WHERE run_id = 'Rc'",
        )?),
        spec_revision: Some(1),
        accepted: 0,
        total: 0,
        last_event_seq: query(&db, "SELECT max(seq) FROM events")?.parse()?,
        cost_usd,
        cost_status,
        remaining_requirements: Vec::new(),
    })?;
    assert_eq!(
        (&result["cost_usd"], &result["cost_status"]),
        (&json!(null), &json!("unknown"))
    );
    assert_eq!(
        query(&db, "SELECT count(*) FROM runs WHERE state = 'COMPLETED'")?,
        "0"
    );
    keep(
        "test-042-06.json",
        &json!({
            "runs": rows(&db, "SELECT run_id, state, blocked_reason FROM runs ORDER BY run_id")?,
            "attempts": rows(&db, "SELECT attempt_id, task_id, state, cost_usd FROM attempts ORDER BY rowid")?,
            "cost": {"Rc": cost("Rc")?, "Rd": cost("Rd")?},
            "rc_run_result_schema_check": result,
        }),
    )?;
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// Moves its clock two minutes on when the first pace begins, once.
#[derive(Debug)]
struct AdvanceAtPace(Arc<FakeClock>, std::sync::atomic::AtomicBool);

impl hoh_core::persistence::FaultHooks for AdvanceAtPace {
    fn at(&self, job: &'static str, point: hoh_core::persistence::WritePoint) {
        if job == "Pace"
            && point == hoh_core::persistence::WritePoint::Begun
            && !self.1.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.0.advance(Duration::from_secs(120));
        }
    }
}

/// M2T19R-04: the run's deadline is a minute ahead when its tick begins; the clock passes it while the pace runs.
/// The round after it takes the time afresh: nothing is dispatched, and the run ends INCOMPLETE.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_round_after_the_deadline_dispatches_nothing_however_its_tick_began() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    let deadline = hoh_core::time::rfc3339_utc(SystemTime::now() + Duration::from_secs(60));
    seed(
        &project,
        &format!(
            "{COST_WORLD}
             DELETE FROM budgets; DELETE FROM tasks WHERE task_id = 'Tc'; DELETE FROM role_bindings WHERE run_id = 'Rc';
             DELETE FROM runs WHERE run_id = 'Rc';
             UPDATE runs SET deadline_at = '{deadline}' WHERE run_id = 'Rd';"
        ),
    )?;
    let clock = Arc::new(FakeClock::default());
    let executor = Arc::new(Scripted {
        script: [("Td".to_owned(), None)].into(),
        started: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(clock.clone());
    config.store.faults = Arc::new(AdvanceAtPace(
        clock,
        std::sync::atomic::AtomicBool::new(false),
    ));
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: executor.clone(),
        context: context(),
    });
    let host = Host::start(config).await?;
    let db = db(&project);
    let end = Instant::now() + Duration::from_secs(20);
    while query(&db, "SELECT state FROM runs WHERE run_id = 'Rd'")? != "INCOMPLETE" {
        if Instant::now() > end {
            return Err("Rd did not end INCOMPLETE".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(query(&db, "SELECT count(*) FROM attempts")?, "0");
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// Timers at the system time, the wall clock two hours ahead of it.
#[derive(Debug, Default)]
struct WallAhead;

impl hoh_core::delivery::Clock for WallAhead {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn wall(&self) -> SystemTime {
        SystemTime::now() + Duration::from_secs(7_200)
    }
}

/// M2T19R-151: the runner judges an absolute deadline by its clock's wall time: an hour ahead by its timers, already
/// past by the wall, so nothing is dispatched and the run ends INCOMPLETE.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_runner_judges_a_deadline_by_the_wall_clock() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    let deadline = hoh_core::time::rfc3339_utc(SystemTime::now() + Duration::from_secs(3_600));
    seed(
        &project,
        &format!(
            "{COST_WORLD}
             DELETE FROM budgets; DELETE FROM tasks WHERE task_id = 'Tc'; DELETE FROM role_bindings WHERE run_id = 'Rc';
             DELETE FROM runs WHERE run_id = 'Rc';
             UPDATE runs SET deadline_at = '{deadline}' WHERE run_id = 'Rd';"
        ),
    )?;
    let executor = Arc::new(Scripted {
        script: [("Td".to_owned(), None)].into(),
        started: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(Arc::new(WallAhead));
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: executor.clone(),
        context: context(),
    });
    let host = Host::start(config).await?;
    let db = db(&project);
    let end = Instant::now() + Duration::from_secs(20);
    while query(&db, "SELECT state FROM runs WHERE run_id = 'Rd'")? != "INCOMPLETE" {
        if Instant::now() > end {
            return Err("Rd did not end INCOMPLETE".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(query(&db, "SELECT count(*) FROM attempts")?, "0");
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// M2T19R-252: the runner's timers read the host clock's `now`, never its wall clock: Td waits for a retry an hour
/// ahead of the timers and an hour behind the wall clock, so Rd parks (WAITING_EXTERNAL) with no attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_runner_waits_for_a_retry_by_its_timers_and_not_the_wall_clock() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    Host::start(config(&project)).await?.shutdown().await;
    let retry = hoh_core::time::rfc3339_utc(SystemTime::now() + Duration::from_secs(3_600));
    seed(
        &project,
        &format!(
            "{COST_WORLD}
             DELETE FROM budgets; DELETE FROM tasks WHERE task_id = 'Tc'; DELETE FROM role_bindings WHERE run_id = 'Rc';
             DELETE FROM runs WHERE run_id = 'Rc';
             UPDATE tasks SET attempt_count = 1, next_retry_at = '{retry}' WHERE task_id = 'Td';"
        ),
    )?;
    let executor = Arc::new(Scripted {
        script: [("Td".to_owned(), None)].into(),
        started: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(Arc::new(WallAhead));
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: executor.clone(),
        context: context(),
    });
    let host = Host::start(config).await?;
    let db = db(&project);
    let end = Instant::now() + Duration::from_secs(20);
    while query(&db, "SELECT state FROM runs WHERE run_id = 'Rd'")? != "WAITING_EXTERNAL" {
        if Instant::now() > end {
            return Err("Rd did not park".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(query(&db, "SELECT count(*) FROM attempts")?, "0");
    host.shutdown().await;
    drop(executor);
    Ok(())
}
