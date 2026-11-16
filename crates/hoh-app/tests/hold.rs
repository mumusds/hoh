//! docs/adr/ADR-031.md design correction D through a host with its runner: the property that a run is Held only in the
//! tick of its stop commit, never after it nor after a restart (test plan "停住之后的写入口" (2)). The store's fault
//! hook records every Held answer with the runner tick it fell in; the world has runs stopped in G, each with the work
//! a query would pick were stopped runs not left out (a PENDING supervisor trigger, an open verification round of a
//! cancelled run, an unrecovered candidate with a live obligation), and a run whose cancel ends while the host runs.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig, HostHooks, RunnerPoint};
use hoh_core::persistence::FaultHooks;
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
fn rows(project: &Path, sql: &str) -> R<Value> {
    Ok(serde_json::from_str(&python(
        &db(project),
        "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1], timeout=10); c.row_factory=sqlite3.Row; \
         print(json.dumps([dict(r) for r in c.execute(sys.stdin.read())]))",
        sql,
    )?)?)
}

/// The runner's ticks, counted where every tick passes (after its staffing).
#[derive(Debug, Default)]
struct Ticks(Arc<AtomicU64>);

impl HostHooks for Ticks {
    fn runner(&self, point: RunnerPoint) {
        if point == RunnerPoint::AfterStaffing {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Every Held answer of the writer: (the runner tick it fell in, the job, the run).
#[derive(Debug, Default)]
struct HeldLog {
    tick: Arc<AtomicU64>,
    seen: Mutex<Vec<(u64, &'static str, String)>>,
}

impl FaultHooks for HeldLog {
    fn held(&self, job: &'static str, run: &str) {
        let tick = self.tick.load(Ordering::SeqCst);
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((tick, job, run.to_owned()));
        }
    }
}

impl HeldLog {
    fn take(&self) -> Vec<(u64, &'static str, String)> {
        self.seen
            .lock()
            .map(|mut seen| std::mem::take(&mut *seen))
            .unwrap_or_default()
    }
}

/// Takes no work and opens no session: nothing is dispatched in this world anyway.
#[derive(Debug)]
struct NoWork;

impl hoh_app::host::Executor for NoWork {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        let _ = answer.send(Err("the fixture takes no work".to_owned()));
        ack
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        Err(hoh_app::host::Refusal {
            reason: "NO_WORK".to_owned(),
            class: hoh_core::retry::FailureClass::Capability,
            detail: None,
            details: None,
        })
    }
}

async fn host(project: &Path, log: &Arc<HeldLog>) -> R<Host> {
    let mut config = HostConfig::new(project);
    config.port = 0;
    config.store.wal_probe = Some(probe());
    config.store.faults = log.clone();
    config.hooks = Arc::new(Ticks(log.tick.clone()));
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: Arc::new(NoWork),
        context: hoh_core::decision::Context::default(),
    });
    Ok(Host::start(config).await?)
}

/// Wait until the host has ticked `n` more times (at most 60 s).
fn ticks(log: &HeldLog, n: u64) -> R {
    let until = log.tick.load(Ordering::SeqCst) + n;
    let end = Instant::now() + Duration::from_secs(60);
    while log.tick.load(Ordering::SeqCst) < until {
        if Instant::now() > end {
            return Err(format!("the runner did not tick {n} times").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// The world: R1 COMPLETED with a PENDING trigger and its Commander bound (the supervised runs), R2 CANCELLED with an open round (the cancelled
/// runs with open rounds), R3 INCOMPLETE likewise, R4 FAILED with candidate C4 VERIFYING under an open
/// round whose last recovery failed (the recoveries due); each stopped, its run.result its latest event. R5 cancelling
/// with an open round and no attempt: its cancel ends at the host's first look.
const WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, resume_state, created_at) VALUES
  ('R1', :p, 's1', 'COMPLETED', 'AUTO', 3, NULL, '2026-11-15T00:00:01Z'),
  ('R2', :p, 's1', 'CANCELLED', 'AUTO', 3, NULL, '2026-11-15T00:00:02Z'),
  ('R3', :p, 's1', 'INCOMPLETE', 'AUTO', 3, NULL, '2026-11-15T00:00:03Z'),
  ('R4', :p, 's1', 'FAILED', 'AUTO', 3, NULL, '2026-11-15T00:00:04Z'),
  ('R5', :p, 's1', 'CANCEL_REQUESTED', 'AUTO', 3, 'RUNNING', '2026-11-15T00:00:05Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R1', 'commander', 'codex', 'sc', 1, 1, '2026-11-15T00:00:00Z'),
  ('R3', 'commander', 'codex', 'sc', 1, 1, '2026-11-15T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq) VALUES
  ('Q2', 'R2', 's1', 'LEASED', 1, 'qa', 1), ('V2', 'R2', 's1', 'LEASED', 1, 'reviewer', 2),
  ('Q4', 'R4', 's1', 'LEASED', 1, 'qa', 1), ('V4', 'R4', 's1', 'LEASED', 1, 'reviewer', 2),
  ('Q5', 'R5', 's1', 'LEASED', 1, 'qa', 1), ('V5', 'R5', 's1', 'LEASED', 1, 'reviewer', 2);
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at) VALUES
  ('C2', 'R2', printf('%040d', 2), printf('%040d', 2), printf('%064d', 1), printf('%064d', 2), printf('%064d', 3),
   '/repo', 'VERIFYING', '2026-11-15T00:00:00Z'),
  ('C4', 'R4', printf('%040d', 4), printf('%040d', 4), printf('%064d', 1), printf('%064d', 2), printf('%064d', 3),
   '/repo', 'VERIFYING', '2026-11-15T00:00:00Z'),
  ('C5', 'R5', printf('%040d', 5), printf('%040d', 5), printf('%064d', 1), printf('%064d', 2), printf('%064d', 3),
   '/repo', 'VERIFYING', '2026-11-15T00:00:00Z');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  (:p, 'R1/nd', 'R1', 'need_decision', '{\"task_id\": \"T\", \"fingerprint\": \"fp\"}', '2026-11-15T00:00:00Z'),
  (:p, 'R3/nd', 'R3', 'need_decision', '{\"task_id\": \"T\", \"fingerprint\": \"fp\"}', '2026-11-15T00:00:00Z'),
  (:p, 'R2/round', 'R2', 'verification.round_opened',
   '{\"round_id\": \"rd2\", \"candidate_id\": \"C2\", \"number\": 1, \"tasks\": {\"qa\": \"Q2\", \"reviewer\": \"V2\"}, \"sources\": []}',
   '2026-11-15T00:00:00Z'),
  (:p, 'R4/round', 'R4', 'verification.round_opened',
   '{\"round_id\": \"rd4\", \"candidate_id\": \"C4\", \"number\": 1, \"tasks\": {\"qa\": \"Q4\", \"reviewer\": \"V4\"}, \"sources\": []}',
   '2026-11-15T00:00:00Z'),
  (:p, 'C4/reconcile_failed/1', 'R4', 'candidate.reconcile_failed',
   '{\"candidate_id\": \"C4\", \"attempt\": 1, \"kind\": \"transient\", \"error\": \"x\", \"at_ms\": \"0\"}',
   '2026-11-15T00:00:00Z'),
  (:p, 'R5/round', 'R5', 'verification.round_opened',
   '{\"round_id\": \"rd5\", \"candidate_id\": \"C5\", \"number\": 1, \"tasks\": {\"qa\": \"Q5\", \"reviewer\": \"V5\"}, \"sources\": []}',
   '2026-11-15T00:00:00Z');
INSERT INTO supervisor_triggers(trigger_id, run_id, fingerprint, spec_id, severity, event_seqs_json, first_seq, last_seq,
  due_at, state, created_at)
  SELECT run_id || '/trigger', run_id, 'fp', 's1', 'normal', json_array(seq), seq, seq, '2000-01-01T00:00:00Z',
    'PENDING', '2026-11-15T00:00:00Z' FROM events WHERE kind = 'need_decision';
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
  SELECT :p, run_id || '/result/3', run_id, 'run.result', json_object('run_id', run_id, 'state', state),
    '2026-11-15T00:00:00Z' FROM runs WHERE run_id IN ('R1', 'R2', 'R3', 'R4') ORDER BY run_id;
";

/// The runs' own events: (run, seq, kind).
fn run_events(project: &Path) -> R<Value> {
    rows(
        project,
        "SELECT run_id, seq, kind FROM events WHERE run_id IS NOT NULL ORDER BY seq",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_is_held_only_in_the_tick_of_its_stop_and_never_after_a_restart() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let log = Arc::new(HeldLog::default());
    // The database and its project, then the world.
    let first = host(&project, &log).await?;
    first.shutdown().await;
    seed(&project, WORLD)?;
    let seeded = run_events(&project)?;
    let host1 = host(&project, &log).await?;
    let outcome = async {
        ticks(&log, 10)?;
        let end = Instant::now() + Duration::from_secs(30);
        while rows(&project, "SELECT state FROM runs WHERE run_id = 'R5'")?[0]["state"]
            != "CANCELLED"
        {
            if Instant::now() > end {
                return Err("R5's cancel did not end".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        ticks(&log, 10)?;
        R::Ok(())
    }
    .await;
    host1.shutdown().await;
    outcome?;
    let held = log.take();
    // R1 to R4 stopped before the host began: never Held; R5 only in one tick, its stop's.
    assert!(held.iter().all(|(_, _, run)| run == "R5"), "held: {held:?}");
    let r5_ticks: std::collections::BTreeSet<u64> = held.iter().map(|(t, ..)| *t).collect();
    assert!(r5_ticks.len() <= 1, "held: {held:?}");
    // No new event of R1 to R4; none of R5 after its run.result.
    let after = run_events(&project)?;
    let of = |events: &Value, runs: &[&str]| -> Vec<Value> {
        events
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| runs.iter().any(|r| e["run_id"] == *r))
            .cloned()
            .collect()
    };
    assert_eq!(
        of(&after, &["R1", "R2", "R3", "R4"]),
        of(&seeded, &["R1", "R2", "R3", "R4"])
    );
    let r5 = of(&after, &["R5"]);
    assert_eq!(
        r5.last().map(|e| e["kind"].clone()),
        Some(Value::from("run.result")),
        "{r5:?}"
    );
    // A restart: nothing Held, nothing new.
    let host2 = host(&project, &log).await?;
    let outcome = ticks(&log, 10);
    host2.shutdown().await;
    outcome?;
    assert_eq!(log.take(), Vec::new());
    assert_eq!(run_events(&project)?, after);
    Ok(())
}

/// Flips run X to a published COMPLETED (and makes Y's trigger due) right after the staffing of tick `flip`, as a stop
/// committed by another writer between the tick's look and its steps would; at the same point of the next tick, notes
/// whether Y's trigger was still PENDING.
#[derive(Debug)]
struct FlipAt {
    project: PathBuf,
    flip: u64,
    tick: Arc<AtomicU64>,
    y_pending_next_tick: Mutex<Option<bool>>,
    /// What went wrong in the hook, for the test to see.
    errors: Mutex<Vec<String>>,
}

impl FlipAt {
    fn failed(&self, e: &dyn std::fmt::Display) {
        if let Ok(mut errors) = self.errors.lock() {
            errors.push(e.to_string());
        }
    }
}

impl HostHooks for FlipAt {
    fn runner(&self, point: RunnerPoint) {
        if point != RunnerPoint::AfterStaffing {
            return;
        }
        let tick = self.tick.fetch_add(1, Ordering::SeqCst) + 1;
        if tick == self.flip {
            if let Err(e) = seed(
                &self.project,
                "UPDATE runs SET state = 'COMPLETED', revision = revision + 1 WHERE run_id = 'X';
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
                   (:p, 'X/result/4', 'X', 'run.result', '{\"run_id\": \"X\", \"state\": \"COMPLETED\"}',
                    '2026-11-15T00:00:00Z');
                 UPDATE supervisor_triggers SET due_at = '2000-01-01T00:00:00Z' WHERE run_id = 'Y';",
            ) {
                self.failed(&e);
            }
        } else if tick == self.flip + 1 {
            match rows(
                &self.project,
                "SELECT state FROM supervisor_triggers WHERE run_id = 'Y'",
            ) {
                Ok(state) => {
                    if let Ok(mut pending) = self.y_pending_next_tick.lock() {
                        *pending = Some(state[0]["state"] == "PENDING");
                    }
                }
                Err(e) => self.failed(&e),
            }
        }
    }
}

/// X and Y RUNNING, supervised (spec, Commander bound), X first; each with a PENDING trigger due far ahead.
const FLIP_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('X', :p, 's1', 'RUNNING', 'AUTO', 3, '2026-11-15T00:00:01Z'),
  ('Y', :p, 's1', 'RUNNING', 'AUTO', 3, '2026-11-15T00:00:02Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('X', 'commander', 'codex', 'sc', 1, 1, '2026-11-15T00:00:00Z'),
  ('Y', 'commander', 'codex', 'sc', 1, 1, '2026-11-15T00:00:00Z');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  (:p, 'X/nd', 'X', 'need_decision', '{\"task_id\": \"T\", \"fingerprint\": \"fp\"}', '2026-11-15T00:00:00Z'),
  (:p, 'Y/nd', 'Y', 'need_decision', '{\"task_id\": \"T\", \"fingerprint\": \"fp\"}', '2026-11-15T00:00:00Z');
INSERT INTO supervisor_triggers(trigger_id, run_id, fingerprint, spec_id, severity, event_seqs_json, first_seq, last_seq,
  due_at, state, created_at)
  SELECT run_id || '/trigger', run_id, 'fp', 's1', 'normal', json_array(seq), seq, seq, '2100-01-01T00:00:00Z',
    'PENDING', '2026-11-15T00:00:00Z' FROM events WHERE kind = 'need_decision';
INSERT INTO supervisor_cursor(run_id, last_handled_seq) SELECT run_id, seq FROM events WHERE kind = 'need_decision';
";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_step_skips_its_run_and_the_tick_goes_on_with_nothing_counted_as_failed() -> R {
    // Correction D rule 2 (b) and (c): X stops between the tick's look and its steps. Its steps that tick are Held:
    // skipped, nothing said, no supervisor.step_failed, no failure counted; Y's steps go on in the same tick (its
    // trigger, due from that tick, is fired before the next tick begins). X is never Held again.
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let log = Arc::new(HeldLog::default());
    let first = host(&project, &log).await?;
    first.shutdown().await;
    seed(&project, FLIP_WORLD)?;
    let flip = Arc::new(FlipAt {
        project: project.clone(),
        flip: 3,
        tick: log.tick.clone(),
        y_pending_next_tick: Mutex::new(None),
        errors: Mutex::new(Vec::new()),
    });
    let mut config = HostConfig::new(&project);
    config.port = 0;
    config.store.wal_probe = Some(probe());
    config.store.faults = log.clone();
    config.hooks = flip.clone();
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: Arc::new(NoWork),
        context: hoh_core::decision::Context::default(),
    });
    let host = Host::start(config).await?;
    let outcome = ticks(&log, 10);
    host.shutdown().await;
    outcome?;
    assert_eq!(
        flip.errors.lock().map(|e| e.clone()).unwrap_or_default(),
        Vec::<String>::new()
    );
    let held = log.take();
    assert!(!held.is_empty(), "X's step that tick is Held");
    assert!(
        held.iter().all(|(tick, _, run)| run == "X" && *tick == 3),
        "held: {held:?}"
    );
    assert_eq!(
        flip.y_pending_next_tick.lock().map(|p| *p).unwrap_or(None),
        Some(false),
        "Y's trigger fired in the tick X was Held in"
    );
    let failed = rows(
        &project,
        "SELECT count(*) AS n FROM events WHERE kind IN ('supervisor.step_failed', 'supervisor.output_failed') \
         OR (run_id = 'X' AND seq > (SELECT seq FROM events WHERE event_id = 'X/result/4'))",
    )?;
    assert_eq!(failed[0]["n"], 0);
    Ok(())
}

/// What the writer and the runner did, in order: each request's name before it begins, each Held answer with its run,
/// and each tick's point after its staffing. Before the first `Staff` request, `stop` is committed from another
/// connection: a run's cancel that lands between the tick's read of the staffing list and the staffing's write.
#[derive(Debug, Default)]
struct StaffingOrder {
    stop: Mutex<Option<(PathBuf, String)>>,
    seen: Mutex<Vec<String>>,
    errors: Mutex<Vec<String>>,
}

impl StaffingOrder {
    fn push(&self, entry: String) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(entry);
        }
    }
}

impl FaultHooks for StaffingOrder {
    fn before_job(&self, job: &'static str) {
        self.push(format!("job {job}"));
        if job != "Staff" {
            return;
        }
        let Some((project, sql)) = self.stop.lock().ok().and_then(|mut s| s.take()) else {
            return;
        };
        if let Err(e) = seed(&project, &sql)
            && let Ok(mut errors) = self.errors.lock()
        {
            errors.push(e.to_string());
        }
    }

    fn held(&self, job: &'static str, run: &str) {
        self.push(format!("held {job} {run}"));
    }
}

impl HostHooks for StaffingOrder {
    fn runner(&self, point: RunnerPoint) {
        if point == RunnerPoint::AfterStaffing {
            self.push("after staffing".to_owned());
        }
    }
}

/// R1 and R2 RUNNING, R1 first, each with a developer binding to codex and no session: the registry, whose PATH has no
/// agent, finds codex not installed, so staffing writes for both.
const STAFFING_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R1', :p, 's1', 'RUNNING', 'AUTO', 3, '2026-11-15T00:00:01Z'),
  ('R2', :p, 's1', 'RUNNING', 'AUTO', 3, '2026-11-15T00:00:02Z');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R1', 'developer', 'codex', NULL, 1, 1, '2026-11-15T00:00:00Z'),
  ('R2', 'developer', 'codex', NULL, 1, 1, '2026-11-15T00:00:00Z');
";

/// R1's cancel ended and published, as another writer's commit leaves it (its run.result named by R1's new
/// revision).
const STOP_R1: &str =
    "UPDATE runs SET state = 'CANCELLED', revision = revision + 1 WHERE run_id = 'R1';
    INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
      (:p, 'R1/result/4', 'R1', 'run.result', json_object('run_id', 'R1', 'state', 'CANCELLED'),
       '2026-11-15T00:00:00Z');";

/// The runs' events of staffing: (run, kind).
fn staffing_events(project: &Path) -> R<Value> {
    rows(
        project,
        "SELECT run_id, kind FROM events WHERE run_id IS NOT NULL AND kind LIKE 'team.%' ORDER BY seq",
    )
}

/// The host half of [`a_held_staffing_write_skips_its_binding_and_the_tick_goes_on`], run by it in a child process
/// whose stderr it reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by a_held_staffing_write_skips_its_binding_and_the_tick_goes_on in a child process"]
async fn a_held_staffing_write_in_a_host_with_a_registry() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    let empty = dir.path().join("bin");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&empty)?;
    let first = host(&project, &Arc::new(HeldLog::default())).await?;
    first.shutdown().await;
    seed(&project, STAFFING_WORLD)?;
    let order = Arc::new(StaffingOrder::default());
    if let Ok(mut stop) = order.stop.lock() {
        *stop = Some((project.clone(), STOP_R1.to_owned()));
    }
    let registry = hoh_app::host::agents::Registry::with_pi_dir(
        empty.clone().into_os_string(),
        Some(dir.path().join("pi-agent")),
    );
    let mut config = HostConfig::product_with(
        &project,
        hoh_core::config::Config::default(),
        env!("CARGO_BIN_EXE_hoh").into(),
        Arc::new(registry),
    );
    config.port = 0;
    config.store.wal_probe = Some(probe());
    config.store.faults = order.clone();
    config.hooks = order.clone();
    let host = Host::start(config).await?;
    let outcome = async {
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            let after = order.seen.lock().map(|s| s.clone()).unwrap_or_default();
            let ticked = after
                .iter()
                .skip_while(|e| !e.starts_with("held"))
                .filter(|e| *e == "after staffing")
                .count();
            if ticked >= 3 {
                break;
            }
            if Instant::now() > end {
                return Err(
                    format!("no Held staffing write and 3 ticks after it: {after:?}").into(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome?;
    assert_eq!(
        order.errors.lock().map(|e| e.clone()).unwrap_or_default(),
        Vec::<String>::new()
    );
    let seen = order.seen.lock().map(|s| s.clone()).unwrap_or_default();
    // R1's staffing write is Held; the same tick goes on: R2's staffing write, then the tick's point after staffing,
    // with no new tick (its first request, the reconcile scan) between them.
    let held = seen
        .iter()
        .position(|e| e.starts_with("held"))
        .ok_or("no Held")?;
    assert_eq!(seen[held], "held Staff R1", "{seen:?}");
    let rest: Vec<&String> = seen[held + 1..]
        .iter()
        .take_while(|e| *e != "after staffing")
        .collect();
    assert!(
        rest.iter().any(|e| *e == "job Staff") && !rest.iter().any(|e| *e == "job Reconcile"),
        "the tick of R1's Held: {rest:?} (all: {seen:?})"
    );
    assert!(
        seen.iter().filter(|e| e.starts_with("held")).count() == 1,
        "{seen:?}"
    );
    // R2 staffed; R1 nothing after its run.result.
    let staffing = staffing_events(&project)?;
    assert!(
        staffing
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e["run_id"] == "R2")),
        "{staffing}"
    );
    assert_eq!(
        rows(
            &project,
            "SELECT count(*) AS n FROM events WHERE run_id = 'R1' AND seq > \
             (SELECT seq FROM events WHERE event_id = 'R1/result/4')"
        )?[0]["n"],
        0
    );
    Ok(())
}

#[test]
fn a_held_staffing_write_skips_its_binding_and_the_tick_goes_on() -> R {
    // docs/adr/ADR-031.md correction D rule 2 (b), M2T13R-01: a host with an agent registry; R1's cancel lands between
    // the tick's read of the staffing list and R1's staffing write. That write is Held: R1's binding is skipped and
    // marked held, the tick goes on (R2's staffing, the steps after staffing), nothing is said on stderr, and R1 gets
    // no event after its run.result.
    let out = Command::new(std::env::current_exe()?)
        .args([
            "a_held_staffing_write_in_a_host_with_a_registry",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "{}\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!stderr.contains("hoh host:"), "a diagnostic: {stderr}");
    Ok(())
}
