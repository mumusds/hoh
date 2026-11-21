//! A verification round's dispatch through a host with its runner (docs/adr/ADR-028.md decisions 3, 4, 9 and 10):
//! TEST-035-03, TEST-035-05, TEST-036-03, TEST-021-05. A Team executor plays every role: the Developer's FakeAgent writes a
//! COMPLETED TaskResult through its bridge (its first `fail` turns fail transiently), the verifiers' FakeAgents run
//! until released, and the Commander's turns are counted (answered with an assumption, or refused at their start).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use hoh_app::host::{Host, HostConfig};
use hoh_core::decision::Context;
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    config.jitter_seed = Some(0x0035_0003);
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
fn rows(project: &Path, sql: &str) -> R<Value> {
    Ok(serde_json::from_str(&python(
        &db(project),
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

/// Wait until `sql`'s rows satisfy `done`, moving `clock` on by `step` each time; the rows.
fn wait_for(
    project: &Path,
    clock: &FakeClock,
    step: Duration,
    sql: &str,
    done: &dyn Fn(&Value) -> bool,
) -> R<Value> {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let found = rows(project, sql)?;
        if done(&found) {
            return Ok(found);
        }
        if Instant::now() > until {
            return Err(format!("not reached: {sql}: {found}").into());
        }
        clock.advance(step);
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn git(dir: &Path, args: &[&str]) -> R {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(())
}

/// The project's frozen check plan: `unit` fails with exit code 1 and changes nothing. Each check has a timeout, so a
/// request helper's wait is minutes, not the hour a check naming none gets (M2T18R-54).
const PLAN: &str = r#"{"schema_version": "1.0", "plan_id": "fixture", "checks": {
  "unit": {"mode": "product", "timeout_seconds": 60, "argv": ["sh", "-c", "exit 1"]},
  "pass": {"mode": "product", "artifacts": ["test-results.jsonl"], "timeout_seconds": 60, "argv": ["sh", "-c",
    "printf '%s\\n' '{\"schema_version\": \"1.0\", \"test_id\": \"TEST-035-03\", \"name\": \"test_035_03_works\", \"status\": \"PASS\"}' > \"$HOH_EVIDENCE_DIR/test-results.jsonl\""]}}}
"#;

/// The project's specification as committed (its sha256 is the run's spec digest).
const SPEC: &str = "{\"requirements\": [{\"id\": \"REQ-035\", \"acceptance\": [{\"id\": \"AC-035-03\", \"test_id\": \"TEST-035-03\"}]}]}\n";

/// A committed project whose layout the freeze takes (docs/adr/ADR-028.md decision 4), its store made by one host's
/// start and stop; the project and the specification's digest.
async fn project(dir: &Path) -> R<(PathBuf, String)> {
    project_with(dir, SPEC, PLAN).await
}

/// [`project`] with specification `spec` and check plan `plan`.
async fn project_with(dir: &Path, spec: &str, plan: &str) -> R<(PathBuf, String)> {
    let project = dir.join("p");
    std::fs::create_dir_all(&project)?;
    git(&project, &["init", "-q"])?;
    let spec = common::spec::write(&project, &serde_json::from_str(spec)?, "integration")?;
    for (path, text) in [
        ("src/lib.rs", "pub fn a() {}\n"),
        ("docs/spec/index.json", spec.as_str()),
        ("tests/check-plan.json", plan),
    ] {
        let file = project.join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, text)?;
    }
    git(&project, &["add", "."])?;
    git(&project, &["commit", "-q", "-m", "work"])?;
    Host::start(config(&project)).await?.shutdown().await;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(spec.as_bytes());
    Ok((project, digest.iter().map(|b| format!("{b:02x}")).collect()))
}

/// Run R RUNNING on the committed specification with REQ-035 (criterion AC-035-03); every built-in role bound to its
/// own session at epoch 1; developer task D READY (a writer, REQ-035).
fn world(spec: &str) -> String {
    format!(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, '{spec}', 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', :p, 's1', 'RUNNING', 'AUTO', 7, '2026-11-12T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-035', 's1', 'r', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-035-03', 'REQ-035', 's1', 'handoffs without the Commander', 'integration');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{{}}'), ('sd', 'claude', 'developer', '1', '{{}}'),
  ('sq', 'pi', 'qa', '1', '{{}}'), ('sr', 'codex', 'reviewer', '1', '{{}}'), ('sx', 'pi', 'researcher', '1', '{{}}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'qa', 'pi', 'sq', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'reviewer', 'codex', 'sr', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'researcher', 'pi', 'sx', 1, 1, '2026-11-12T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('D', 'R', 's1', 'READY', 1, 'developer', 1, 1, 1);
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-035', 's1');"
    )
}

/// How the Team's Commander answers its turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Commander {
    /// One record_assumption decision naming what its turn was given.
    Answers,
    /// Refused at its start for authentication: the agent cannot run (docs/adr/ADR-025.md decision 5).
    FailsAtStart,
}

impl Commander {
    fn fails(self) -> bool {
        self == Commander::FailsAtStart
    }
}

/// A verifier work's role and the run directory its start was given.
type RunDir = (String, Option<PathBuf>);

/// Plays every role of run R (see the module's head).
#[derive(Debug)]
struct Team {
    /// The Commander's turns are refused at their start (else answered).
    refuse: AtomicBool,
    /// The project, whose objects keep a verifier's raw session log.
    project: PathBuf,
    /// The status a verifier's report block gives.
    report: std::sync::Mutex<String>,
    /// The reviewer's report status, when it differs from `report`.
    review_report: std::sync::Mutex<Option<String>>,
    /// What the reviewer FakeAgent runs once released, as `qa_script` for qa.
    review_script: std::sync::Mutex<Option<String>>,
    spec: String,
    /// The Commander's turns started (refused ones included).
    commander_turns: Arc<AtomicU32>,
    /// The Developer turns still to fail (transiently) before one completes.
    fail: AtomicU32,
    /// Lets the held FakeAgents (verifiers and other readers) end.
    release: Arc<AtomicBool>,
    /// The qa FakeAgent writes a file into its working directory before it ends.
    tamper: AtomicBool,
    /// What the qa FakeAgent runs once released (sh), with `$HOH_RUN_CHECK`, `$HOH_ASK`, its identity (`$RUN_ID`,
    /// `$TASK_ID`, `$ATTEMPT_ID`, `$ROLE_EPOCH`) and `$OUT`, a directory outside its run directory.
    qa_script: std::sync::Mutex<Option<String>>,
    /// The `$OUT` of the qa script.
    out: PathBuf,
    /// Each verifier work's role and run directory, as given to its start.
    run_dirs: Arc<std::sync::Mutex<Vec<RunDir>>>,
    /// Lets the Developer's FakeAgent end (set: it is not held).
    developer: Arc<AtomicBool>,
    /// What the Developer's FakeAgent runs (sh, in the project) before it reports COMPLETED, when set.
    developer_script: std::sync::Mutex<Option<String>>,
    /// The verifiers' starts are refused (transiently) while set: none of them runs.
    refuse_verifiers: AtomicBool,
    /// SQL run on the project's store when a verifier's work is delivered (before its run directory is prepared).
    on_verifier_delivery: std::sync::Mutex<Option<String>>,
    /// The scope a verifier's report block gives.
    scope: std::sync::Mutex<Vec<String>>,
    /// The findings the qa FakeAgent's report block gives.
    qa_findings: std::sync::Mutex<Vec<Value>>,
    /// The findings the reviewer FakeAgent's report block gives.
    review_findings: std::sync::Mutex<Vec<Value>>,
    /// The Developer's FakeAgent writes a self-test record of every TEST passing under its attempt's directory and names
    /// it in its TaskResult's evidence (docs/adr/ADR-028.md decision 14).
    self_test: AtomicBool,
    /// Each FakeAgent started: its process group (its leader's pid) and its leader's start time.
    agents: std::sync::Mutex<Vec<(u32, Option<String>)>>,
    /// The product's agent registry, when set: the runner then staffs the bindings by it (docs/adr/ADR-030.md decision
    /// 1), and the Team takes each new holder's first input (its session.start) as delivered. Each turn then runs as
    /// the registry's agent at its start says, as the product's prepare takes it (`Started::ran`, decision 2).
    registry: std::sync::Mutex<Option<Arc<hoh_app::host::agents::Registry>>>,
    /// Developer tasks whose FakeAgent is held until `task_release` (not `developer`).
    held_tasks: std::sync::Mutex<Vec<String>>,
    task_release: Arc<AtomicBool>,
    /// How a verifier's turn fails once released, when it does (docs/adr/ADR-030.md decision 6): no report, its end of
    /// this class (`BLOCKED_AUTH` for Auth).
    verifier_fails: std::sync::Mutex<Option<hoh_core::retry::FailureClass>>,
    /// Only a verifier's turn run as this agent fails (as `verifier_fails` says), when set; else every one does.
    verifier_fails_as: std::sync::Mutex<Option<String>>,
}

/// The Developer FakeAgent's self-test record: it claims every TEST of TEST-053-01's specification passed.
const SELF_TEST: &str = "{\"schema_version\": \"1.0\", \"test_id\": \"TEST-090-01\", \"name\": \"self_a\", \"status\": \"PASS\"}\n\
    {\"schema_version\": \"1.0\", \"test_id\": \"TEST-090-02\", \"name\": \"self_b\", \"status\": \"PASS\"}\n";

impl Team {
    fn new(commander: Commander, spec: &str, project: &Path) -> Team {
        Team {
            refuse: AtomicBool::new(commander.fails()),
            spec: spec.to_owned(),
            project: project.to_owned(),
            report: std::sync::Mutex::new("PASS".to_owned()),
            review_report: std::sync::Mutex::default(),
            review_script: std::sync::Mutex::default(),
            commander_turns: Arc::default(),
            fail: AtomicU32::new(0),
            release: Arc::default(),
            tamper: AtomicBool::new(false),
            qa_script: std::sync::Mutex::default(),
            out: std::env::temp_dir(),
            run_dirs: Arc::default(),
            developer: Arc::new(AtomicBool::new(true)),
            developer_script: std::sync::Mutex::default(),
            refuse_verifiers: AtomicBool::new(false),
            on_verifier_delivery: std::sync::Mutex::default(),
            scope: std::sync::Mutex::new(vec!["AC-035-03".to_owned()]),
            qa_findings: std::sync::Mutex::default(),
            review_findings: std::sync::Mutex::default(),
            self_test: AtomicBool::new(false),
            agents: std::sync::Mutex::default(),
            registry: std::sync::Mutex::default(),
            held_tasks: std::sync::Mutex::default(),
            task_release: Arc::default(),
            verifier_fails: std::sync::Mutex::default(),
            verifier_fails_as: std::sync::Mutex::default(),
        }
    }

    /// End every FakeAgent still running, its whole process group (its request helpers too), then let every held one
    /// go: a host's stop leaves its agents running (they outlive a host), so a test ends its own once its host is
    /// stopped, before its directory goes, and none runs a request helper against no host (M2T18R-54). Only a group
    /// whose leader is still the FakeAgent started is signalled, never a number taken again.
    fn end_agents(&self) {
        let agents = self
            .agents
            .lock()
            .map(|mut a| std::mem::take(&mut *a))
            .unwrap_or_default();
        for (pgid, start) in agents {
            let same =
                start.is_some() && hoh_agents::identity::start_id(pgid).ok().flatten() == start;
            if let (true, Ok(pgid)) = (same, i32::try_from(pgid)) {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(pgid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        self.developer.store(true, Ordering::SeqCst);
        self.release.store(true, Ordering::SeqCst);
        self.task_release.store(true, Ordering::SeqCst);
    }
}

/// A test's Team ends its FakeAgents with it, however the test ended.
impl Drop for Team {
    fn drop(&mut self) {
        self.end_agents();
    }
}

fn refusal(reason: &str, class: hoh_core::retry::FailureClass) -> hoh_app::host::Refusal {
    hoh_app::host::Refusal {
        reason: reason.to_owned(),
        class,
        detail: None,
        details: None,
    }
}

impl hoh_app::host::Executor for Team {
    fn registry(&self) -> Option<Arc<hoh_app::host::agents::Registry>> {
        self.registry.lock().ok().and_then(|r| r.clone())
    }

    fn open_session(
        &self,
        _start: &hoh_core::persistence::SessionStart,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (sent, answer) = tokio::sync::oneshot::channel();
        let taken = if self.registry().is_some() {
            Ok(())
        } else {
            Err("this executor opens no sessions".to_owned())
        };
        let _ = sent.send(taken);
        answer
    }

    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        let sql = self
            .on_verifier_delivery
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .filter(|_| work.assignment.handoff.is_some());
        let done = match sql {
            Some(sql) => seed(&self.project, &sql).map_err(|e| e.to_string()),
            None => Ok(()),
        };
        let _ = answer.send(done);
        ack
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let transient = hoh_core::retry::FailureClass::Transient { retry_after: None };
        let mut messages = Vec::new();
        let mut final_text = None;
        let mut artifacts = Vec::new();
        let mut failed = false;
        let mut verifier_class = None;
        let mut held = None;
        match work.role.as_str() {
            "commander" => {
                self.commander_turns.fetch_add(1, Ordering::SeqCst);
                if self.refuse.load(Ordering::SeqCst) {
                    return Err(refusal(
                        "LOGIN_MISSING",
                        hoh_core::retry::FailureClass::Auth,
                    ));
                }
                let input = work
                    .assignment
                    .supervision
                    .clone()
                    .ok_or_else(|| refusal("NOT_SUPERVISION", transient))?;
                let message = json!({
                    "schema_version": "1.0", "message_id": format!("{}/answer", work.attempt_id), "run_id": "R",
                    "task_id": "-", "attempt_id": work.attempt_id, "sender_role": "commander",
                    "recipient_role": "runtime", "role_epoch": input.role_epoch, "correlation_id": "c",
                    "causation_id": input.wake, "idempotency_key": format!("{}/k", work.attempt_id),
                    "expected_revision": input.run_revision, "spec_sha256": self.spec, "candidate_id": null,
                    "kind": "decision",
                    "payload": {"kind": "record_assumption", "statement": "nothing to change", "basis": "the wake"}
                });
                messages.push(json!({"nonce": input.nonce, "message": message}).to_string());
            }
            "developer" => {
                let own = self
                    .held_tasks
                    .lock()
                    .is_ok_and(|t| t.contains(&work.task_id));
                held = Some(if own {
                    self.task_release.clone()
                } else {
                    self.developer.clone()
                });
                failed = self
                    .fail
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                if !failed {
                    let mut evidence = Vec::new();
                    if self.self_test.load(Ordering::SeqCst) {
                        let dir = format!(
                            ".hoh/self-tests/{}",
                            work.attempt_id.replace('%', "%25").replace('/', "%2F")
                        );
                        let record = format!("{dir}/self-test.jsonl");
                        std::fs::create_dir_all(self.project.join(&dir))
                            .and_then(|()| std::fs::write(self.project.join(&record), SELF_TEST))
                            .map_err(|e| refusal(&format!("SELF_TEST: {e}"), transient))?;
                        let digest = <sha2::Sha256 as sha2::Digest>::digest(SELF_TEST.as_bytes());
                        let sha256: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                        evidence.push(json!({"path": record, "sha256": sha256}));
                    }
                    messages.push(
                        json!({
                            "schema_version": "1.0", "run_id": work.run_id, "task_id": work.task_id,
                            "attempt_id": work.attempt_id, "role": work.role,
                            "role_epoch": work.assignment.role_epoch, "status": "COMPLETED",
                            "summary": "implemented", "candidate_id": null, "evidence": evidence, "issues": [],
                            "next_action": "verify"
                        })
                        .to_string(),
                    );
                }
            }
            role => {
                if work.assignment.handoff.is_some() && self.refuse_verifiers.load(Ordering::SeqCst)
                {
                    return Err(refusal("NOT_YET", transient));
                }
                held = Some(self.release.clone());
                if let Ok(mut dirs) = self.run_dirs.lock() {
                    dirs.push((role.to_owned(), work.run_dir.clone()));
                }
                let fails = self
                    .verifier_fails
                    .lock()
                    .ok()
                    .and_then(|f| *f)
                    .filter(|_| {
                        self.verifier_fails_as
                            .lock()
                            .ok()
                            .and_then(|a| a.clone())
                            .is_none_or(|a| work.assignment.agent.as_deref() == Some(a.as_str()))
                    });
                if let (Some(class), true) = (fails, work.assignment.handoff.is_some()) {
                    failed = true;
                    verifier_class = Some(class);
                }
                // A verifier reports its work done through its bridge, and its verdict in a hoh-report block of its
                // final message, its raw session log kept as an object, as the product's agent and executor do.
                if work.assignment.handoff.is_some() && !failed {
                    let status = match self.review_report.lock().ok().and_then(|r| r.clone()) {
                        Some(review) if work.role == "reviewer" => review,
                        _ => self.report.lock().map(|s| s.clone()).unwrap_or_default(),
                    };
                    let scope = self.scope.lock().map(|s| s.clone()).unwrap_or_default();
                    let findings = if work.role == "qa" {
                        &self.qa_findings
                    } else {
                        &self.review_findings
                    }
                    .lock()
                    .map(|f| f.clone())
                    .unwrap_or_default();
                    final_text = Some(format!(
                        "done\n```hoh-report\n{}\n```\n",
                        json!({"status": status, "scope": scope, "findings": findings})
                    ));
                    let raw = format!("{}\n", json!({"type": "result", "result": final_text}));
                    let sha256 = hoh_core::objects::put(&self.project, raw.as_bytes())
                        .map_err(|e| refusal(&format!("RAW_LOG: {e}"), transient))?;
                    artifacts.push(hoh_core::persistence::ArtifactRow {
                        path: hoh_core::objects::relative_path(&sha256),
                        sha256,
                        bytes: raw.len() as u64,
                        mime: "application/x-ndjson".to_owned(),
                        kind: "raw_log".to_owned(),
                        name: "stdout.jsonl".to_owned(),
                    });
                    messages.push(
                        json!({
                            "schema_version": "1.0", "run_id": work.run_id, "task_id": work.task_id,
                            "attempt_id": work.attempt_id, "role": work.role,
                            "role_epoch": work.assignment.role_epoch, "status": "COMPLETED",
                            "summary": "verified", "candidate_id": null, "evidence": [], "issues": [],
                            "next_action": "report"
                        })
                        .to_string(),
                    );
                }
            }
        }
        let qa_script = if work.role == "reviewer" {
            self.review_script.lock().ok().and_then(|s| s.clone())
        } else {
            self.qa_script.lock().ok().and_then(|s| s.clone())
        };
        let developer_script = self
            .developer_script
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .filter(|_| work.role == "developer");
        let script = if work.role == "qa" && self.tamper.load(Ordering::SeqCst) {
            "read -r go; echo changed > tampered.txt; exit 0".to_owned()
        } else if let Some(body) =
            qa_script.filter(|_| matches!(work.role.as_str(), "qa" | "reviewer"))
        {
            format!("read -r go; {body}")
        } else if let Some(body) = &developer_script {
            format!("read -r go; {body}")
        } else {
            "read -r go; exit 0".to_owned()
        };
        let mut command = Command::new("sh");
        if let Some(dir) = &work.run_dir {
            command.current_dir(dir);
        }
        // A writer works in the project (never the test's own directory).
        if developer_script.is_some() {
            command.current_dir(&self.project);
        }
        if let Some(h) = &work.helpers {
            command
                .env("HOH_RUN_CHECK", &h.run_check)
                .env("HOH_ASK", &h.ask)
                .env("RUN_ID", &work.run_id)
                .env("TASK_ID", &work.task_id)
                .env("ATTEMPT_ID", &work.attempt_id)
                .env("ROLE_EPOCH", work.assignment.role_epoch.to_string())
                .env("OUT", &self.out);
        }
        let mut child = command
            .args(["-c", &script])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| refusal(&format!("SPAWN: {e}"), transient))?;
        let pid = child.id();
        if let Ok(mut agents) = self.agents.lock() {
            agents.push((pid, hoh_agents::identity::start_id(pid).ok().flatten()));
        }
        let stdin = child.stdin.take();
        let gate = hoh_app::host::Gate::new(move || {
            use std::io::Write as _;
            let mut stdin = stdin.ok_or("no stdin")?;
            std::thread::spawn(move || {
                while held.as_ref().is_some_and(|h| !h.load(Ordering::SeqCst)) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = stdin.write_all(b"go\n");
            });
            Ok(())
        });
        let (end, ended) = tokio::sync::oneshot::channel();
        let raw_log_path = format!(
            ".hoh/attempts/{}/raw.jsonl",
            work.attempt_id.replace('/', "%2F")
        );
        std::thread::spawn(move || {
            let status = child.wait();
            let until = Instant::now() + Duration::from_secs(5);
            let mut group_gone = hoh_agents::identity::group_empty(pid);
            while !group_gone && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
                group_gone = hoh_agents::identity::group_empty(pid);
            }
            let _ = end.send(hoh_app::host::Outcome {
                success: !failed && status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: match verifier_class {
                    Some(hoh_core::retry::FailureClass::Auth) => "BLOCKED_AUTH",
                    Some(_) => "FAILED",
                    None if failed => "TransientExternal",
                    None => "ended",
                }
                .to_owned(),
                group_gone,
                class: verifier_class.unwrap_or(transient),
                observed: hoh_core::persistence::Observed {
                    messages,
                    raw_log_path: Some(raw_log_path),
                    final_text,
                    artifacts,
                    ..Default::default()
                },
                unknown_effect: false,
                retry_at: None,
            });
        });
        // What the turn runs as, as the product's prepare takes it from the registry (docs/adr/ADR-030.md decision 2).
        let ran = self.registry().and_then(|registry| {
            let agent = work
                .assignment
                .agent
                .as_deref()
                .and_then(hoh_agents::compat::Agent::named)?;
            registry.prepared(agent)?.ok().map(|current| current.ran)
        });
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
            ran,
        })
    }
}

/// A host of `project` whose runner is `team`, on `clock`; every role needs no capability.
async fn host(project: &Path, team: Arc<Team>, clock: Arc<FakeClock>) -> R<Host> {
    let mut config = config(project);
    config.clock = hoh_app::host::HostClock(clock);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: team,
        context: Context {
            required_capabilities: ["commander", "developer", "qa", "reviewer", "researcher"]
                .map(|r| (r.to_owned(), Default::default()))
                .into(),
            // Commander directories, and the run directories beside them, in the test's own directory.
            commander_dir: project.parent().map(|d| d.join("state/hoh/commander")),
            ..Context::default()
        },
    });
    Ok(Host::start(config).await?)
}

/// The verifier attempts dispatched, with the origin of their dispatch.
const VERIFIERS: &str = "SELECT a.attempt_id, t.role, a.session_id, json_extract(o.payload_json, '$.origin') AS origin \
    FROM attempts a JOIN tasks t ON t.task_id = a.task_id JOIN outbox o ON o.message_id = a.attempt_id || '/dispatch' \
    WHERE t.role IN ('qa', 'reviewer') ORDER BY t.role";

/// The verification handoffs: their recipient, candidate and the event they hang on.
const HANDOFFS: &str = "SELECT recipient_role AS role, json_extract(payload_json, '$.candidate_id') AS candidate, \
    event_seq FROM outbox WHERE json_extract(payload_json, '$.payload.handoff_kind') = 'verification' \
    ORDER BY recipient_role";

/// TEST-035-03's run: D completes into C1 (with a critical issue opened at that moment when `critical`); the
/// Commander's turns counted from candidate_ready until both verifier tasks are dispatched and 12 s more (past every
/// supervisor bound: 2 s join, 10 s cap) have passed.
async fn candidate_ready_hands_off(critical: bool) -> R<(u32, Value, Value)> {
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(
            &project,
            &clock,
            step,
            "SELECT seq FROM events WHERE kind = 'candidate_ready'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let at_ready = team.commander_turns.load(Ordering::SeqCst);
        if critical {
            seed(
                &project,
                "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
                   (:p, 'issue-1', 'R', 'issue.opened',
                    '{\"issue_id\": \"I1\", \"severity\": \"critical\", \"fingerprint\": \"fp-I1\"}',
                    strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));",
            )?;
        }
        let verifiers = wait_for(&project, &clock, step, VERIFIERS, &|r| {
            r.as_array().is_some_and(|a| a.len() == 2)
        })?;
        for _ in 0..24 {
            clock.advance(Duration::from_millis(500));
            std::thread::sleep(Duration::from_millis(100));
        }
        let turns = team.commander_turns.load(Ordering::SeqCst) - at_ready;
        let handoffs = rows(&project, HANDOFFS)?;
        R::Ok((turns, handoffs, verifiers))
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_035_03_candidate_ready_hands_c1_to_qa_and_review_without_the_commander() -> R {
    // AC-035-03 (docs/adr/ADR-028.md decisions 9 and 10): D's FakeAgent completes; its completion freezes C1 and
    // opens its round: the qa and reviewer handoffs in one transaction (one event_seq), both naming C1, and their
    // verifier tasks dispatched by the Runtime (origin verification) with no Commander turn in the window. The
    // positive control: the same run with a critical issue opened as candidate_ready comes counts one turn.
    let (turns, handoffs, verifiers) = candidate_ready_hands_off(false).await?;
    let candidate = handoffs[0]["candidate"].clone();
    assert!(
        candidate.as_str().is_some_and(|c| c.starts_with("CAND-")),
        "{handoffs}"
    );
    assert_eq!(
        handoffs,
        json!([
            {"role": "qa", "candidate": candidate, "event_seq": handoffs[0]["event_seq"]},
            {"role": "reviewer", "candidate": candidate, "event_seq": handoffs[0]["event_seq"]},
        ])
    );
    assert_eq!(
        verifiers
            .as_array()
            .map(|v| v.iter().map(|r| r["origin"].clone()).collect::<Vec<_>>()),
        Some(vec![json!("verification"), json!("verification")])
    );
    assert_eq!(turns, 0, "the Commander was called for candidate_ready");
    let (control, _, _) = candidate_ready_hands_off(true).await?;
    assert_eq!(control, 1, "the counter sees a critical issue's wake");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_035_05_with_the_commander_unavailable_qa_and_review_are_still_dispatched() -> R {
    // AC-035-05: the Commander's every turn is refused at its start; a critical issue is opened while D's FakeAgent
    // runs, and the run goes BLOCKED (COMMANDER_UNAVAILABLE). D then completes (candidate_ready on the BLOCKED run):
    // qa and reviewer still get one handoff each and their verifier tasks are dispatched (origin verification, the
    // Runtime's class a BLOCKED run takes); no APPLIED decision from the Commander.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::FailsAtStart, &spec, &project));
    team.developer.store(false, Ordering::SeqCst);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(
            &project,
            &clock,
            step,
            "SELECT state FROM attempts WHERE task_id = 'D'",
            &|r| r[0]["state"] == "RUNNING",
        )?;
        seed(
            &project,
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               (:p, 'issue-1', 'R', 'issue.opened',
                '{\"issue_id\": \"I1\", \"severity\": \"critical\", \"fingerprint\": \"fp-I1\"}',
                strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));",
        )?;
        let run = wait_for(
            &project,
            &clock,
            step,
            "SELECT state, blocked_reason FROM runs WHERE run_id = 'R'",
            &|r| r[0]["state"] == "BLOCKED",
        )?;
        assert_eq!(run[0]["blocked_reason"], json!("COMMANDER_UNAVAILABLE"));
        team.developer.store(true, Ordering::SeqCst);
        let verifiers = wait_for(&project, &clock, step, VERIFIERS, &|r| {
            r.as_array().is_some_and(|a| a.len() == 2)
        })?;
        assert!(
            verifiers
                .as_array()
                .is_some_and(|v| v.iter().all(|r| r["origin"] == "verification")),
            "{verifiers}"
        );
        assert_eq!(
            rows(&project, "SELECT state FROM runs WHERE run_id = 'R'")?[0]["state"],
            json!("BLOCKED"),
            "dispatched on the BLOCKED run"
        );
        let handoffs = rows(&project, HANDOFFS)?;
        assert_eq!(
            handoffs
                .as_array()
                .map(|h| h.iter().map(|r| r["role"].clone()).collect::<Vec<_>>()),
            Some(vec![json!("qa"), json!("reviewer")])
        );
        assert!(team.commander_turns.load(Ordering::SeqCst) >= 1);
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM decisions WHERE state = 'APPLIED'"
            )?[0]["n"],
            json!(0)
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_036_03_three_attempts_then_qa_in_a_session_of_its_own() -> R {
    // AC-036-03: D's FakeAgent fails transiently twice, then completes into C1; QA is dispatched for C1. D has three
    // attempts with distinct ids, A1 and A2 FAILED with their own raw_log_path kept, each attempt's role_epoch,
    // host_epoch, configuration revision and session_id set; QA's attempt's session is none of D's.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    team.fail.store(2, Ordering::SeqCst);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        // The retries come due on the clock (ADR-020: backoff with jitter, at most a minute here).
        let verifiers = wait_for(&project, &clock, Duration::from_secs(5), VERIFIERS, &|r| {
            r.as_array().is_some_and(|a| a.len() == 2)
        })?;
        let attempts = rows(
            &project,
            "SELECT attempt_id, state, raw_log_path, role_epoch, host_epoch, config_revision, session_id \
             FROM attempts WHERE task_id = 'D' ORDER BY rowid",
        )?;
        let attempts = attempts.as_array().ok_or("no attempts")?;
        assert_eq!(attempts.len(), 3, "{attempts:?}");
        let ids: std::collections::BTreeSet<&str> = attempts
            .iter()
            .filter_map(|a| a["attempt_id"].as_str())
            .collect();
        assert_eq!(ids.len(), 3);
        for (n, a) in attempts.iter().enumerate() {
            for field in ["role_epoch", "host_epoch", "config_revision", "session_id"] {
                assert!(!a[field].is_null(), "{field} of {a}");
            }
            let raw = format!(
                ".hoh/attempts/{}/raw.jsonl",
                a["attempt_id"].as_str().unwrap_or_default().replace('/', "%2F")
            );
            if n < 2 {
                assert_eq!(
                    (a["state"].clone(), a["raw_log_path"].clone()),
                    (json!("FAILED"), json!(raw)),
                    "{a}"
                );
            }
        }
        assert_eq!(attempts[2]["state"], json!("SETTLED"));
        let developer: std::collections::BTreeSet<&str> = attempts
            .iter()
            .filter_map(|a| a["session_id"].as_str())
            .collect();
        let qa = verifiers
            .as_array()
            .and_then(|v| v.iter().find(|r| r["role"] == "qa"))
            .ok_or("no qa attempt")?;
        let session = qa["session_id"].as_str().ok_or("qa has no session")?;
        assert!(!developer.contains(session), "{session} in {developer:?}");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// The data of every event `GET /v1/events` streams for `query` within `limit`, by an SSE client of the host's port.
async fn streamed(host: &Host, query: &str, limit: Duration) -> R<Vec<Value>> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    let request = format!(
        "GET /v1/events?{query} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\nAccept: text/event-stream\r\n\r\n",
        host.port, host.token
    );
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    let mut buf = vec![0u8; 8192];
    let end = tokio::time::Instant::now() + limit;
    while let Ok(Ok(n)) = tokio::time::timeout_at(end, stream.read(&mut buf)).await {
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    // The chunked body, unchunked.
    let mut decoded = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || after.len() < size {
            break;
        }
        decoded.push_str(&after[..size]);
        rest = after[size..].trim_start_matches("\r\n");
    }
    decoded
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| Ok(serde_json::from_str(d.trim())?))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_05_a_critical_issue_wakes_the_reserved_slot_and_handoffs_survive_the_commanders_loss()
-> R {
    // AC-021-05 (docs/adr/ADR-028.md decision 10, the hand-over of ADR-025 decision 3): the writer slot holds D's
    // long FakeAgent, both reader slots two long non-verification readers, the supervisor's slot reserved, on a fake
    // clock. A critical issue at t=0 wakes the Commander at t=2.0 s while every slot is still held. Then the
    // Commander is lost (its turns refused at their start); a need_decision and D's completion (candidate_ready)
    // follow: the run is BLOCKED with COMMANDER_UNAVAILABLE and NEED_DECISION, the qa and reviewer handoffs exist one
    // each, and GET /v1/events gives each wake's dedup key, event seqs and latency.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(
        &project,
        &format!(
            "{}
             INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate)
               VALUES ('X1', 'R', 's1', 'READY', 1, 'researcher', 0, 2, 1),
                      ('X2', 'R', 's1', 'READY', 1, 'researcher', 0, 3, 1);",
            world(&spec)
        ),
    )?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    team.developer.store(false, Ordering::SeqCst);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let busy = "SELECT group_concat(t.task_id || ':' || a.state, ',') AS slots FROM attempts a \
                    JOIN tasks t ON t.task_id = a.task_id WHERE t.task_id IN ('D', 'X1', 'X2') ORDER BY t.task_id";
        wait_for(&project, &clock, step, busy, &|r| {
            r[0]["slots"] == "D:RUNNING,X1:RUNNING,X2:RUNNING"
        })?;
        seed(
            &project,
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               (:p, 'issue-1', 'R', 'issue.opened',
                '{\"issue_id\": \"I1\", \"severity\": \"critical\", \"fingerprint\": \"fp-I1\"}',
                strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));",
        )?;
        let woken = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.latency_ms') AS latency FROM events WHERE kind = 'supervisor.woken'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let latency = woken[0]["latency"].as_u64().ok_or("no latency")?;
        assert!((2_000..3_000).contains(&latency), "woken after {latency} ms");
        // Its turn runs while every work slot is still held: the reserved slot, not a place behind them.
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'commander' \
             AND a.state IN ('RUNNING', 'SETTLED')",
            &|r| r[0]["n"] == json!(1),
        )?;
        assert_eq!(
            rows(&project, busy)?[0]["slots"],
            json!("D:RUNNING,X1:RUNNING,X2:RUNNING")
        );
        // The Commander is lost; a need_decision, then D's completion.
        team.refuse.store(true, Ordering::SeqCst);
        seed(
            &project,
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               (:p, 'nd-D', 'R', 'need_decision', '{\"task_id\": \"D\", \"fingerprint\": \"fp-D\"}',
                strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));",
        )?;
        team.developer.store(true, Ordering::SeqCst);
        wait_for(
            &project,
            &clock,
            step,
            "SELECT seq FROM events WHERE kind = 'candidate_ready'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let run = wait_for(
            &project,
            &clock,
            step,
            "SELECT state, blocked_reason, blocked_reasons_json AS reasons FROM runs WHERE run_id = 'R'",
            &|r| r[0]["state"] == "BLOCKED" && r[0]["reasons"].as_str().is_some_and(|s| s.contains("NEED_DECISION")),
        )?;
        assert_eq!(run[0]["blocked_reason"], json!("COMMANDER_UNAVAILABLE"), "{run}");
        let handoffs = rows(&project, HANDOFFS)?;
        assert_eq!(
            handoffs
                .as_array()
                .map(|h| h.iter().map(|r| r["role"].clone()).collect::<Vec<_>>()),
            Some(vec![json!("qa"), json!("reviewer")])
        );
        let streamed = streamed(
            &host,
            "run_id=R&kinds=supervisor.woken",
            Duration::from_secs(2),
        )
        .await?;
        assert!(!streamed.is_empty());
        for event in &streamed {
            let p = &event["payload"];
            assert!(
                p["dedup_key"]["fingerprint"].is_string()
                    && p["event_seqs"].as_array().is_some_and(|s| !s.is_empty())
                    && p["latency_ms"].is_u64(),
                "{event}"
            );
        }
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// The candidate's commit and tree, and its id.
fn candidate(project: &Path) -> R<(String, String, String)> {
    let row = rows(
        project,
        "SELECT candidate_id, git_commit, git_tree FROM candidates",
    )?;
    let text = |k: &str| {
        row[0][k]
            .as_str()
            .map(str::to_owned)
            .ok_or(format!("no {k}"))
    };
    Ok((
        text("candidate_id")?,
        text("git_commit")?,
        text("git_tree")?,
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_verifier_works_in_a_checkout_of_the_candidate_outside_the_project_checked_at_both_ends()
-> R {
    // docs/adr/ADR-028.md decisions 6 and 8 (REQ-037 behaviours 3 and 5): each verifier attempt is started in its own
    // run directory outside the project tree, a checkout at the candidate's commit and tree, recorded with its
    // pre_tree in verification.attempt_started; at its end verification.tree_checked says it is unchanged, and the
    // directory is removed once the attempt is settled.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        // Both started (their start was given its run directory before RUNNING).
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') AND a.state = 'RUNNING'",
            &|r| r[0]["n"] == json!(2),
        )?;
        let (id, commit, tree) = candidate(&project)?;
        let dirs = team.run_dirs.lock().map(|d| d.clone()).unwrap_or_default();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        for (role, run_dir) in &dirs {
            let run_dir = run_dir.as_ref().ok_or(format!("{role} has no run directory"))?;
            assert!(!run_dir.starts_with(&project), "{role}: {}", run_dir.display());
            let head = Command::new("git").args(["rev-parse", "HEAD", "HEAD^{tree}"]).current_dir(run_dir).output()?;
            assert_eq!(String::from_utf8(head.stdout)?, format!("{commit}\n{tree}\n"), "{role}");
        }
        let started = rows(
            &project,
            "SELECT json_extract(payload_json, '$.run_dir') AS run_dir, json_extract(payload_json, '$.pre_tree.head') AS head, \
             json_extract(payload_json, '$.candidate_id') AS candidate FROM events WHERE kind = 'verification.attempt_started' \
             ORDER BY seq",
        )?;
        assert_eq!(started.as_array().map(Vec::len), Some(2), "{started}");
        for s in started.as_array().into_iter().flatten() {
            assert_eq!((&s["head"], &s["candidate"]), (&json!(commit), &json!(id)), "{s}");
        }
        team.release.store(true, Ordering::SeqCst);
        let checked = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.equal') AS equal FROM events WHERE kind = 'verification.tree_checked'",
            &|r| r.as_array().is_some_and(|a| a.len() == 2),
        )?;
        assert_eq!(checked, json!([{"equal": 1}, {"equal": 1}]));
        // Their COMPLETED results settle them, each recorded (docs/adr/ADR-028.md decision 11).
        wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.result_status') AS status FROM events \
             WHERE kind = 'verification.result' ORDER BY seq",
            &|r| r.as_array().is_some_and(|a| a.len() == 2),
        )?;
        assert_eq!(
            rows(
                &project,
                "SELECT DISTINCT json_extract(payload_json, '$.result_status') AS status FROM events \
                 WHERE kind = 'verification.result'"
            )?,
            json!([{"status": "COMPLETED"}])
        );
        // Each with its report: the block's status, the report kept as an object valid under its schema, naming the
        // attempt's HoH session and the candidate (decision 15).
        let reports = rows(
            &project,
            "SELECT json_extract(payload_json, '$.role') AS role, json_extract(payload_json, '$.status') AS status, \
             json_extract(payload_json, '$.report.path') AS path FROM events WHERE kind = 'verification.report' \
             ORDER BY json_extract(payload_json, '$.role')",
        )?;
        let reports = reports.as_array().ok_or("no reports")?;
        assert_eq!(reports.len(), 2);
        for r in reports {
            assert_eq!(r["status"], json!("PASS"), "{r}");
            let report: Value = serde_json::from_slice(&std::fs::read(
                project.join(r["path"].as_str().ok_or("no path")?),
            )?)?;
            assert_eq!(report["candidate_id"], json!(id), "{report}");
            assert!(
                hoh_core::schema::independent_report().validate(&report).is_ok(),
                "{report}"
            );
        }
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') AND a.state = 'SETTLED'",
            &|r| r[0]["n"] == json!(2),
        )?;
        let end = Instant::now() + Duration::from_secs(10);
        while dirs.iter().any(|(_, d)| d.as_ref().is_some_and(|d| d.exists())) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        for (role, run_dir) in &dirs {
            assert!(run_dir.as_ref().is_some_and(|d| !d.exists()), "{role}'s run directory was left");
        }
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verifier_that_changes_its_run_directory_makes_the_candidate_invalid() -> R {
    // docs/adr/ADR-028.md decision 8: the qa FakeAgent writes a file into its run directory; at its end the tree check
    // finds it changed (its status shows the file) and the attempt-level marker record makes the evidence INVALID, the
    // candidate INVALID, with integrity_violation.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    team.tamper.store(true, Ordering::SeqCst);
    team.release.store(true, Ordering::SeqCst);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let checked = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.equal') AS equal, json_extract(payload_json, '$.status') AS status \
             FROM events WHERE kind = 'verification.tree_checked' AND json_extract(payload_json, '$.attempt_id') LIKE '%task/qa%'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(checked[0]["equal"], json!(0), "{checked}");
        assert!(
            checked[0]["status"].as_str().is_some_and(|s| s.contains("tampered.txt")),
            "{checked}"
        );
        let evidence = wait_for(
            &project,
            &clock,
            step,
            "SELECT outcome, check_id FROM evidence",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(evidence, json!([{"outcome": "INVALID", "check_id": "independent-qa"}]));
        assert_eq!(
            rows(&project, "SELECT state FROM candidates")?,
            json!([{"state": "INVALID"}])
        );
        assert_eq!(
            rows(&project, "SELECT count(*) AS n FROM events WHERE kind = 'integrity_violation'")?[0]["n"],
            json!(1)
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// A message of the qa script's attempt, as its `$1` names: `<kind> <recipient> <payload JSON>`.
const MESSAGE: &str = "message() { printf '{\"schema_version\": \"1.0\", \"message_id\": \"%s\", \"run_id\": \"%s\", \
\"task_id\": \"%s\", \"attempt_id\": \"%s\", \"sender_role\": \"qa\", \"recipient_role\": \"%s\", \"role_epoch\": %s, \
\"correlation_id\": \"c\", \"causation_id\": null, \"idempotency_key\": \"%s\", \"expected_revision\": 1, \
\"spec_sha256\": \"%s\", \"candidate_id\": null, \"kind\": \"%s\", \"payload\": %s}' \
\"$1\" \"$RUN_ID\" \"$TASK_ID\" \"$ATTEMPT_ID\" \"$3\" \"$ROLE_EPOCH\" \"$1\" \"$SPEC\" \"$2\" \"$4\" > \"$OUT/$1.json\"; }; ";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verifier_runs_planned_checks_and_asks_through_its_request_helpers() -> R {
    // docs/adr/ADR-028.md decisions 7 and 13: the qa FakeAgent, in its run directory, runs `run-check unit` (the check
    // runs there, its producer the qa attempt, and the helper ends with its exit code 1), `run-check nope` (not a
    // check of the frozen plan: refused, exit 2, verification.check_refused), asks the developer to change the
    // candidate (rejected: request.rejected and an OPEN Issue on the candidate) and asks an information question
    // (deferred); no message is queued for the developer.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.path().join("out");
    std::fs::create_dir_all(&team.out)?;
    let script = format!(
        "SPEC={spec}; {MESSAGE}\
         \"$HOH_RUN_CHECK\" unit > \"$OUT/unit.txt\"; echo $? > \"$OUT/unit.code\"; \
         \"$HOH_RUN_CHECK\" nope > \"$OUT/nope.txt\"; echo $? > \"$OUT/nope.code\"; \
         message m-change steer developer '{{\"text\": \"rewrite the parser\", \"severity\": \"high\"}}'; \
         \"$HOH_ASK\" \"$OUT/m-change.json\" > \"$OUT/change.txt\"; \
         message m-info query developer '{{\"question\": \"what does AC-035-03 mean?\"}}'; \
         \"$HOH_ASK\" \"$OUT/m-info.json\" > \"$OUT/info.txt\"; exit 0"
    );
    *team.qa_script.lock().map_err(|e| e.to_string())? = Some(script);
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let out = team.out.clone();
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role = 'qa' AND a.state = 'SETTLED'",
            &|r| r[0]["n"] == json!(1),
        )?;
        let read = |name: &str| std::fs::read_to_string(out.join(name)).unwrap_or_default();
        assert_eq!(read("unit.code").trim(), "1", "{}", read("unit.txt"));
        assert!(read("unit.txt").contains("\"status\":\"ran\""), "{}", read("unit.txt"));
        assert_eq!(read("nope.code").trim(), "2", "{}", read("nope.txt"));
        assert!(read("change.txt").contains("\"status\":\"rejected\""), "{}", read("change.txt"));
        assert!(read("info.txt").contains("\"status\":\"deferred\""), "{}", read("info.txt"));
        let qa = rows(
            &project,
            "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'qa'",
        )?[0]["attempt_id"]
            .clone();
        assert_eq!(
            rows(&project, "SELECT check_id, attempt_id, outcome FROM evidence")?,
            json!([{"check_id": "unit", "attempt_id": qa, "outcome": "FAIL"}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT json_extract(payload_json, '$.check_id') AS check_id FROM events \
                 WHERE kind = 'verification.check_refused'"
            )?,
            json!([{"check_id": "nope"}])
        );
        assert_eq!(
            rows(&project, "SELECT severity, state, description FROM issues")?,
            json!([{"severity": "high", "state": "OPEN", "description": "rewrite the parser"}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT json_extract(payload_json, '$.message_id') AS m FROM events WHERE kind = 'request.deferred'"
            )?,
            json!([{"m": "m-info"}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM outbox WHERE recipient_role = 'developer' \
                 AND json_extract(payload_json, '$.kind') IN ('steer', 'query')"
            )?[0]["n"],
            json!(0)
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_037_02_qa_and_review_run_side_by_side_and_the_round_closes_only_after_both() -> R {
    // AC-037-02 (docs/adr/ADR-028.md decisions 6 to 11): QA runs `run-check unit` (fails) and reports FAIL and ends
    // first; the reviewer, still running then, runs `run-check pass` and reports PASS later. No round_closed between
    // the two ends; after both, the round closes (its seq after both ends), with two evidence rows (the FAIL the QA
    // attempt's, the PASS the reviewer's) whose identity is their own run directory's candidate, and C1 REJECTED.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.path().join("out");
    std::fs::create_dir_all(&team.out)?;
    *team.report.lock().map_err(|e| e.to_string())? = "FAIL".to_owned();
    *team.review_report.lock().map_err(|e| e.to_string())? = Some("PASS".to_owned());
    *team.qa_script.lock().map_err(|e| e.to_string())? =
        Some("\"$HOH_RUN_CHECK\" unit > \"$OUT/qa.txt\"; exit 0".to_owned());
    // The reviewer runs its check, then waits for the test's word before it ends.
    *team.review_script.lock().map_err(|e| e.to_string())? = Some(
        "\"$HOH_RUN_CHECK\" pass > \"$OUT/review.txt\"; while [ ! -e \"$OUT/end-review\" ] && [ -d \"$OUT\" ]; do sleep 0.1; done; exit 0"
            .to_owned(),
    );
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let out = team.out.clone();
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        // QA ended while the reviewer runs (its check recorded).
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role = 'qa' AND a.state = 'SETTLED'",
            &|r| r[0]["n"] == json!(1),
        )?;
        wait_for(&project, &clock, step, "SELECT count(*) AS n FROM evidence", &|r| r[0]["n"] == json!(2))?;
        for _ in 0..10 {
            clock.advance(step);
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            rows(&project, "SELECT count(*) AS n FROM events WHERE kind = 'verification.round_closed'")?[0]["n"],
            json!(0),
            "closed before the reviewer ended"
        );
        std::fs::write(out.join("end-review"), "")?;
        let closed = wait_for(
            &project,
            &clock,
            step,
            "SELECT seq, json_extract(payload_json, '$.verdict') AS verdict FROM events \
             WHERE kind = 'verification.round_closed'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(
            closed[0]["verdict"],
            json!("repair"),
            "{}",
            rows(
                &project,
                "SELECT ev.check_id, ev.outcome, ev.kind, json_extract(e.payload_json, '$.reasons') AS reasons \
                 FROM evidence ev LEFT JOIN events e ON e.event_id = ev.evidence_id || '/evidence'"
            )?
        );
        let ends = rows(
            &project,
            "SELECT max(e.seq) AS seq FROM events e JOIN attempts a ON a.attempt_id = json_extract(e.payload_json, \
             '$.attempt_id') JOIN tasks t ON t.task_id = a.task_id WHERE e.kind = 'attempt.moved' \
             AND json_extract(e.payload_json, '$.to') = 'SETTLED' AND t.role IN ('qa', 'reviewer')",
        )?;
        assert!(closed[0]["seq"].as_i64() > ends[0]["seq"].as_i64(), "{closed} {ends}");
        assert_eq!(
            rows(
                &project,
                "SELECT ev.check_id, ev.outcome, t.role FROM evidence ev JOIN attempts a ON a.attempt_id = ev.attempt_id \
                 JOIN tasks t ON t.task_id = a.task_id ORDER BY ev.check_id"
            )?,
            json!([{"check_id": "pass", "outcome": "PASS", "role": "reviewer"},
                   {"check_id": "unit", "outcome": "FAIL", "role": "qa"}])
        );
        assert_eq!(rows(&project, "SELECT state FROM candidates")?, json!([{"state": "REJECTED"}]));
        // Side by side (M2T16R-58): their runs overlap, and each worked on the same candidate, as its own run
        // directory's start and its evidence say.
        let runs = rows(
            &project,
            "SELECT a.started_at, a.ended_at FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') ORDER BY t.role",
        )?;
        let at = |i: usize, k: &str| runs[i][k].as_str().unwrap_or_default().to_owned();
        assert!(at(0, "started_at") < at(1, "ended_at") && at(1, "started_at") < at(0, "ended_at"), "{runs}");
        let (id, commit, tree) = candidate(&project)?;
        assert_eq!(
            rows(
                &project,
                "SELECT DISTINCT json_extract(payload_json, '$.candidate_id') AS c, json_extract(payload_json, '$.pre_tree.head') AS head, \
                 json_extract(payload_json, '$.pre_tree.head_tree') AS tree FROM events WHERE kind = 'verification.attempt_started'"
            )?,
            json!([{"c": id, "head": commit, "tree": tree}])
        );
        assert_eq!(
            rows(&project, "SELECT DISTINCT candidate_id FROM evidence")?,
            json!([{"candidate_id": id}])
        );
        // Both verifiers completed: their handoffs ACCEPTED at the close (decision 9).
        assert_eq!(
            rows(&project, "SELECT to_role, state FROM handoffs ORDER BY to_role")?,
            json!([{"to_role": "qa", "state": "ACCEPTED"}, {"to_role": "reviewer", "state": "ACCEPTED"}])
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_damaged_pending_record_makes_its_candidate_stale_and_its_round_superseded() -> R {
    // docs/adr/ADR-028.md decision 11's recovery (M2T16D-1250, -1251): a pending record of C1 whose facts do not parse
    // is a permanent fault: judged at once, C1 STALE with candidate.stale{evidence_unrecoverable} and
    // integrity_violation; its round, once its verifiers end, closes superseded, and D goes back to be frozen anew.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(&project, &clock, step, VERIFIERS, &|r| r.as_array().is_some_and(|a| a.len() == 2))?;
        let (id, _, _) = candidate(&project)?;
        let record = "00000000-0000-4000-8000-000000000001";
        std::fs::create_dir_all(project.join(format!(".hoh/evidence/{id}/unit/{record}")))?;
        std::fs::create_dir_all(project.join(".hoh/pending"))?;
        std::fs::write(
            project.join(format!(".hoh/pending/{record}.json")),
            json!({"record": {"candidate_id": id}, "damaged": true}).to_string(),
        )?;
        wait_for(&project, &clock, step, "SELECT state FROM candidates", &|r| {
            r[0]["state"] == "STALE"
        })?;
        assert_eq!(
            rows(
                &project,
                "SELECT json_extract(payload_json, '$.kind') AS kind FROM events WHERE kind = 'candidate.reconcile_failed'"
            )?,
            json!([{"kind": "permanent"}])
        );
        assert_eq!(
            rows(&project, "SELECT json_extract(payload_json, '$.reason') AS reason FROM events WHERE kind = 'candidate.stale'")?,
            json!([{"reason": "evidence_unrecoverable"}])
        );
        team.release.store(true, Ordering::SeqCst);
        let closed = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.verdict') AS verdict FROM events WHERE kind = 'verification.round_closed'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(closed[0]["verdict"], json!("superseded"));
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM events WHERE kind = 'task.superseded' AND json_extract(payload_json, '$.task_id') = 'D'",
            &|r| r[0]["n"] == json!(1),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_the_hosts_start_a_failed_recovery_wakes_the_commander_and_holds_its_verifiers_and_unused_dirs_go()
-> R {
    // docs/adr/ADR-028.md decision 16: a host stops while C1's verifiers run. Before the next starts, C1's record
    // directory gets a pending record its listing cannot read (a transient fault), and a run directory and a request
    // directory of no attempt appear. At its start, before its first tick: C1's recovery fails, recorded with
    // integrity_violation (the Commander woken); C1's verifier tasks are not dispatched while it is held; the unused
    // directories are gone. Once a recovery succeeds, C1's verifiers are dispatched again.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    let step = Duration::from_millis(100);
    // The first host's verifiers never run (their starts refused, retried later), so the next host's start finds
    // C1's round with its verifier tasks waiting, never an attempt whose effect is unknown.
    team.refuse_verifiers.store(true, Ordering::SeqCst);
    let first = host(&project, team.clone(), clock.clone()).await?;
    let started = async {
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(DISTINCT t.task_id) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') AND a.state = 'FAILED'",
            &|r| r[0]["n"] == json!(2),
        )?;
        R::Ok(candidate(&project)?.0)
    }
    .await;
    first.shutdown().await;
    team.refuse_verifiers.store(false, Ordering::SeqCst);
    let verifier_attempts = "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
        WHERE t.role IN ('qa', 'reviewer')";
    let before = rows(&project, verifier_attempts)?[0]["n"]
        .as_i64()
        .unwrap_or_default();
    let id = started?;
    let project_id = rows(&project, "SELECT project_id FROM projects")?[0]["project_id"]
        .as_str()
        .ok_or("no project")?
        .to_owned();
    let component = |id: &str| id.replace('%', "%25").replace('/', "%2F");
    let unused_run = dir
        .path()
        .join("state/hoh/runs")
        .join(component(&project_id))
        .join(component("gone/attempt-1"));
    std::fs::create_dir_all(unused_run.join("src"))?;
    let unused_requests = project.join(format!(
        ".hoh/attempts/{}/requests",
        component("gone/attempt-1")
    ));
    std::fs::create_dir_all(&unused_requests)?;
    std::fs::write(
        unused_requests.join("1.json"),
        "{\"kind\":\"check\",\"check_id\":\"unit\"}\n",
    )?;
    let record = "00000000-0000-4000-8000-000000000002";
    let check_dir = project.join(format!(".hoh/evidence/{id}/unit"));
    std::fs::create_dir_all(check_dir.join(record))?;
    std::fs::create_dir_all(project.join(".hoh/pending"))?;
    std::fs::write(
        project.join(format!(".hoh/pending/{record}.json")),
        json!({"record": {"candidate_id": id}}).to_string(),
    )?;
    std::fs::set_permissions(&check_dir, std::fs::Permissions::from_mode(0o000))?;
    let second = host(&project, team.clone(), clock.clone()).await;
    let outcome = async {
        let second = second.as_ref().map_err(|e| e.to_string())?;
        let woken = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.reason') AS reason FROM events WHERE kind = 'integrity_violation'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(woken, json!([{"reason": "reconcile_failed_at_start"}]));
        // Removed by the same start, right after the recoveries are recorded.
        let end = Instant::now() + Duration::from_secs(30);
        while unused_run.exists() || unused_requests.exists() {
            assert!(
                Instant::now() < end,
                "{} {}",
                unused_run.display(),
                unused_requests.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        // Held: a few ticks, and no verifier attempt beyond the first host's.
        for _ in 0..10 {
            clock.advance(step);
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            rows(&project, verifier_attempts)?[0]["n"].as_i64().unwrap_or_default(),
            before,
            "held: no verifier attempt since the first host's"
        );
        // Tried again with backoff (2 s, then 4 s), not every tick: at most the start's and one more in the 1 s held.
        let failed = rows(&project, "SELECT count(*) AS n FROM events WHERE kind = 'candidate.reconcile_failed'")?;
        assert!(failed[0]["n"].as_i64().is_some_and(|n| (1..=2).contains(&n)), "{failed}");
        // The fault goes (the record is put aside): the next recovery succeeds and the verifiers go on.
        std::fs::set_permissions(&check_dir, std::fs::Permissions::from_mode(0o755))?;
        std::fs::remove_file(project.join(format!(".hoh/pending/{record}.json")))?;
        std::fs::remove_dir_all(check_dir.join(record))?;
        wait_for(&project, &clock, step, "SELECT count(*) AS n FROM events WHERE kind = 'candidate.reconcile_recovered'", &|r| {
            r[0]["n"] == json!(1)
        })?;
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role IN ('qa', 'reviewer')",
            &|r| r[0]["n"].as_i64().is_some_and(|n| n > before),
        )
        .map_err(|e| {
            format!(
                "{e}; tasks {}; attempts {}",
                rows(&project, "SELECT task_id, state, blocked_reason, next_retry_at FROM tasks WHERE role IN ('qa', 'reviewer')")
                    .unwrap_or_default(),
                rows(&project, "SELECT a.attempt_id, a.state, a.failure_reason FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                     WHERE t.role IN ('qa', 'reviewer')")
                    .unwrap_or_default()
            )
        })?;
        let _ = second;
        R::Ok(())
    }
    .await;
    let _ = std::fs::set_permissions(&check_dir, std::fs::Permissions::from_mode(0o755));
    if let Ok(second) = second {
        second.shutdown().await;
    }
    team.end_agents();
    outcome
}

/// The sha256 of the file at `path`, as lowercase hex.
fn file_sha256(path: &Path) -> R<String> {
    let bytes = std::fs::read(path)?;
    Ok(<sha2::Sha256 as sha2::Digest>::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_054_03_qa_fails_first_and_review_still_completes_with_its_report_in_parallel() -> R {
    // AC-054-03 (docs/adr/ADR-028.md decisions 9, 10 and 15, M2T16D-158): reader_slots 2; QA's planned check fails
    // and QA ends first, reporting FAIL; the reviewer, still running then, completes normally afterwards. Each role has
    // exactly one attempt, in sessions of their own; both handoffs carry the same candidate field by field; their runs
    // overlap; the reviewer's report is kept (its object there, its raw session's digest checks, its native attempt
    // valid by the schema and naming the report's session); the candidate is not ACCEPTED.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.path().join("out");
    std::fs::create_dir_all(&team.out)?;
    *team.report.lock().map_err(|e| e.to_string())? = "FAIL".to_owned();
    *team.review_report.lock().map_err(|e| e.to_string())? = Some("PASS".to_owned());
    *team.qa_script.lock().map_err(|e| e.to_string())? =
        Some("\"$HOH_RUN_CHECK\" unit > \"$OUT/qa.txt\"; exit 0".to_owned());
    *team.review_script.lock().map_err(|e| e.to_string())? = Some(
        "while [ ! -e \"$OUT/end-review\" ] && [ -d \"$OUT\" ]; do sleep 0.1; done; exit 0"
            .to_owned(),
    );
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let out = team.out.clone();
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role = 'qa' AND a.state = 'SETTLED'",
            &|r| r[0]["n"] == json!(1),
        )?;
        assert_eq!(
            rows(
                &project,
                "SELECT a.state FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'reviewer'"
            )?,
            json!([{"state": "RUNNING"}]),
            "the reviewer still runs after QA's FAIL"
        );
        std::fs::write(out.join("end-review"), "")?;
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM events WHERE kind = 'verification.round_closed'",
            &|r| r[0]["n"] == json!(1),
        )?;
        let attempts = rows(
            &project,
            "SELECT t.role, a.attempt_id, a.session_id, a.started_at, a.ended_at, a.state FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id WHERE t.role IN ('qa', 'reviewer') ORDER BY t.role",
        )?;
        let [qa, review] = attempts.as_array().map(Vec::as_slice).unwrap_or_default() else {
            return Err(format!("not one attempt each: {attempts}").into());
        };
        assert_eq!((&qa["role"], &review["role"]), (&json!("qa"), &json!("reviewer")));
        assert_eq!((&qa["state"], &review["state"]), (&json!("SETTLED"), &json!("SETTLED")));
        assert_ne!(qa["session_id"], review["session_id"], "{attempts}");
        assert!(qa["session_id"].is_string() && review["session_id"].is_string());
        let at = |a: &Value, k: &str| a[k].as_str().unwrap_or_default().to_owned();
        assert!(
            at(qa, "started_at") < at(review, "ended_at") && at(review, "started_at") < at(qa, "ended_at"),
            "their runs overlap: {attempts}"
        );
        let handoffs = rows(
            &project,
            "SELECT recipient_role AS role, json_extract(payload_json, '$.payload.candidate') AS candidate FROM outbox \
             WHERE json_extract(payload_json, '$.payload.handoff_kind') = 'verification' ORDER BY recipient_role",
        )?;
        let candidates: Vec<Value> = handoffs
            .as_array()
            .into_iter()
            .flatten()
            .map(|h| serde_json::from_str(h["candidate"].as_str().unwrap_or("null")).unwrap_or(Value::Null))
            .collect();
        assert_eq!(candidates.len(), 2, "{handoffs}");
        assert!(candidates[0].is_object(), "{handoffs}");
        assert_eq!(candidates[0], candidates[1], "field by field");
        // The reviewer's report, kept.
        let reported = rows(
            &project,
            &format!(
                "SELECT json_extract(payload_json, '$.status') AS status, json_extract(payload_json, '$.report.path') AS \
                 path, json_extract(payload_json, '$.report.sha256') AS sha FROM events WHERE kind = 'verification.report' \
                 AND json_extract(payload_json, '$.attempt_id') = '{}'",
                at(review, "attempt_id")
            ),
        )?;
        assert_eq!(reported[0]["status"], json!("PASS"), "{reported}");
        let report_file = project.join(reported[0]["path"].as_str().ok_or("no report path")?);
        assert_eq!(file_sha256(&report_file)?, reported[0]["sha"].as_str().unwrap_or_default());
        let report: Value = serde_json::from_slice(&std::fs::read(&report_file)?)?;
        let raw = &report["raw_session"];
        assert_eq!(
            file_sha256(&project.join(raw["path"].as_str().ok_or("no raw session")?))?,
            raw["sha256"].as_str().unwrap_or_default()
        );
        let native_file = project.join(report["native_attempt"]["path"].as_str().ok_or("no native attempt")?);
        assert_eq!(
            file_sha256(&native_file)?,
            report["native_attempt"]["sha256"].as_str().unwrap_or_default()
        );
        let native: Value = serde_json::from_slice(&std::fs::read(&native_file)?)?;
        hoh_core::schema::native_attempt()
            .validate(&native)
            .map_err(|v| format!("{v:?}"))?;
        assert!(
            native["session_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&report["session_id"])),
            "{native} {report}"
        );
        assert_eq!(report["session_id"], review["session_id"]);
        let state = rows(&project, "SELECT state FROM candidates")?;
        assert_ne!(state[0]["state"], json!("ACCEPTED"));
        assert_eq!(state, json!([{"state": "REJECTED"}]));
        keep(
            "m2t16-test-054-03",
            &json!({"attempts": attempts, "handoff_candidate": candidates[0], "report": report,
                "native_attempt": native, "candidate_state": state[0]["state"]}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reports_qa_and_review_settle_through_the_host_are_current_rows_of_a_completed_native_attempt()
-> R {
    // docs/adr/ADR-029.md decision 7 (-400, -450), the product path: D completes into C1; the QA and Reviewer
    // FakeAgents complete with PASS reports, settled through the host. Each report's independent_reports row is
    // CURRENT, on C1, in its attempt's HoH session, and its native attempt export says COMPLETED (one of the locked
    // release policy's accepted statuses; a forged SETTLED is REJECTED, the store's NATIVE_ATTEMPT_INCOMPLETE test).
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Team::new(Commander::Answers, &spec, &project);
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') AND a.state = 'SETTLED'",
            &|r| r[0]["n"] == json!(2),
        )?;
        let (candidate, _, _) = candidate(&project)?;
        let reports = rows(
            &project,
            "SELECT r.role, r.state, r.reject_reason, r.candidate_id, r.session_id, a.session_id AS attempt_session, \
             (SELECT json_extract(e.payload_json, '$.native_attempt.path') FROM events e \
               WHERE e.kind = 'verification.report' AND json_extract(e.payload_json, '$.attempt_id') = r.attempt_id) \
               AS native_path FROM independent_reports r JOIN attempts a ON a.attempt_id = r.attempt_id \
             ORDER BY r.role",
        )?;
        let reports = reports.as_array().cloned().unwrap_or_default();
        assert_eq!(
            reports.iter().map(|r| (r["role"].clone(), r["state"].clone())).collect::<Vec<_>>(),
            [(json!("qa"), json!("CURRENT")), (json!("reviewer"), json!("CURRENT"))],
            "{reports:?}"
        );
        let mut natives = Vec::new();
        for report in &reports {
            assert_eq!(report["candidate_id"], json!(candidate), "{report}");
            assert_eq!(report["session_id"], report["attempt_session"], "{report}");
            assert_eq!(report["reject_reason"], Value::Null, "{report}");
            let native: Value = serde_json::from_slice(&std::fs::read(
                project.join(report["native_path"].as_str().ok_or("no native attempt")?),
            )?)?;
            assert_eq!(native["status"], "COMPLETED", "{native}");
            natives.push(native);
        }
        keep(
            "m2t18-product-report-state",
            &json!({"independent_reports": reports, "native_attempts": natives}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// Keep `value` as `<name>.json` in the check's evidence directory, when the test runs under run_check.
fn keep(name: &str, value: &Value) -> R {
    if let Ok(dir) = std::env::var("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

/// TEST-037-03's run: the qa FakeAgent runs `qa` (sh, in its run directory) and reports PASS; the reviewer runs the
/// passing check and reports PASS, ending once `$OUT/end-review` exists. The project, its clock and the out directory.
async fn tampering_qa(
    dir: &Path,
    qa: &str,
) -> R<(PathBuf, Arc<FakeClock>, Arc<Team>, PathBuf, Host)> {
    let (project, spec) = project(dir).await?;
    // The passing check is AC-035-03's scenario check, so a PASS row could be written: only the veto keeps it out
    // (M2T16R-52).
    seed(
        &project,
        &format!(
            "{}\nUPDATE acceptance_criteria SET check_id = 'pass';",
            world(&spec)
        ),
    )?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.join("out");
    std::fs::create_dir_all(&team.out)?;
    *team.qa_script.lock().map_err(|e| e.to_string())? = Some(qa.to_owned());
    *team.review_script.lock().map_err(|e| e.to_string())? = Some(
        "\"$HOH_RUN_CHECK\" pass > \"$OUT/review.txt\"; while [ ! -e \"$OUT/end-review\" ] && [ -d \"$OUT\" ]; do sleep 0.1; done; exit 0"
            .to_owned(),
    );
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let out = team.out.clone();
    let host = host(&project, team.clone(), clock.clone()).await?;
    Ok((project, clock, team, out, host))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_037_03_a_qa_that_rewrites_a_tracked_file_makes_its_evidence_invalid_and_c1_has_no_pass()
-> R {
    // AC-037-03 (docs/adr/ADR-028.md decisions 6, 8 and 11; M2T16D-356, -655, -750): the qa FakeAgent rewrites a
    // tracked source file of its run directory after 1 s, then asks to run the passing check (the pre-check refuses
    // it: the checkout is not the candidate, verification.check_refused) and reports PASS. The reviewer runs the passing
    // check and reports PASS. At QA's end its run directory is checked again: tree_checked with a pre_tree and a
    // post_tree that differ and git's status naming the file; the attempt-level record makes the evidence INVALID and
    // C1 INVALID; C1 has no PASS row (the reviewer's PASS rows are not written: C1 is INVALID at its round's close);
    // the round closes superseded and D goes back to be frozen anew, counted by repair.reverify (refreeze/D).
    let dir = tempfile::tempdir()?;
    let (project, clock, team, out, host) = tampering_qa(
        dir.path(),
        "sleep 1; echo '// changed by qa' >> src/lib.rs; \"$HOH_RUN_CHECK\" pass > \"$OUT/qa.txt\"; \
         echo $? > \"$OUT/qa.code\"; exit 0",
    )
    .await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let checked = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.equal') AS equal, json_extract(payload_json, '$.status') AS status, \
             json_extract(payload_json, '$.pre_tree') AS pre, json_extract(payload_json, '$.post_tree') AS post \
             FROM events WHERE kind = 'verification.tree_checked' AND json_extract(payload_json, '$.attempt_id') LIKE '%task/qa%'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(checked[0]["equal"], json!(0), "{checked}");
        assert!(checked[0]["status"].as_str().is_some_and(|s| s.contains("src/lib.rs")), "{checked}");
        assert!(checked[0]["pre"].is_string() && checked[0]["post"].is_string(), "{checked}");
        assert_ne!(checked[0]["pre"], checked[0]["post"], "the tree hashes differ");
        let read = |name: &str| std::fs::read_to_string(out.join(name)).unwrap_or_default();
        assert_eq!(read("qa.code").trim(), "2", "refused: {}", read("qa.txt"));
        assert_eq!(
            rows(
                &project,
                "SELECT json_extract(payload_json, '$.check_id') AS check_id FROM events \
                 WHERE kind = 'verification.check_refused'"
            )?,
            json!([{"check_id": "pass"}])
        );
        let invalid = wait_for(
            &project,
            &clock,
            step,
            "SELECT ev.outcome, ev.check_id, t.role FROM evidence ev JOIN attempts a ON a.attempt_id = ev.attempt_id \
             JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'qa'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(invalid, json!([{"outcome": "INVALID", "check_id": "independent-qa", "role": "qa"}]));
        assert_eq!(rows(&project, "SELECT state FROM candidates")?, json!([{"state": "INVALID"}]));
        std::fs::write(out.join("end-review"), "")?;
        let closed = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.verdict') AS verdict FROM events WHERE kind = 'verification.round_closed'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(closed, json!([{"verdict": "superseded"}]));
        assert_eq!(
            rows(&project, "SELECT DISTINCT state FROM handoffs")?,
            json!([{"state": "REJECTED"}]),
            "a superseded round's handoffs"
        );
        assert_eq!(
            rows(&project, "SELECT count(*) AS n FROM acceptance_results WHERE status = 'PASS'")?,
            json!([{"n": 0}]),
            "C1 has no PASS row"
        );
        // The positive control: the reviewer's evidence concluded a PASS row for AC-035-03, which the close left out.
        assert_eq!(
            rows(
                &project,
                "SELECT json_extract(e.payload_json, '$.rows[0].status') AS status FROM events e \
                 JOIN evidence ev ON ev.evidence_id = json_extract(e.payload_json, '$.evidence_id') \
                 JOIN attempts a ON a.attempt_id = ev.attempt_id JOIN tasks t ON t.task_id = a.task_id \
                 WHERE e.kind = 'evidence.recorded' AND t.role = 'reviewer'"
            )?,
            json!([{"status": "PASS"}])
        );
        assert!(
            rows(&project, "SELECT outcome FROM evidence ev JOIN attempts a ON a.attempt_id = ev.attempt_id \
                 JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'reviewer'")?
                .as_array()
                .is_some_and(|a| a.iter().any(|e| e["outcome"] == json!("PASS"))),
            "the reviewer ran its passing check"
        );
        wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.round_root') AS root FROM events WHERE kind = 'repair.reverify'",
            &|r| r == &json!([{"root": "refreeze/D"}]),
        )?;
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM events WHERE kind = 'task.superseded' AND json_extract(payload_json, '$.task_id') = 'D'"
            )?,
            json!([{"n": 1}])
        );
        keep("m2t16-test-037-03", &json!({"tree_checked": checked, "evidence": invalid, "closed": closed}))?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_037_03_a_qa_that_ran_a_passing_check_before_rewriting_a_tracked_file_still_makes_c1_invalid()
-> R {
    // AC-037-03's variant (docs/adr/ADR-028.md decision 8): the passing check runs first, on the untouched checkout,
    // then the qa FakeAgent rewrites the tracked file; the check at its end still finds the change: C1 INVALID, no PASS
    // row.
    let dir = tempfile::tempdir()?;
    let (project, clock, team, out, host) = tampering_qa(
        dir.path(),
        "\"$HOH_RUN_CHECK\" pass > \"$OUT/qa.txt\"; echo $? > \"$OUT/qa.code\"; sleep 1; \
         echo '// changed by qa' >> src/lib.rs; exit 0",
    )
    .await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(&project, &clock, step, "SELECT state FROM candidates", &|r| r[0]["state"] == json!("INVALID"))?;
        let read = |name: &str| std::fs::read_to_string(out.join(name)).unwrap_or_default();
        assert_eq!(read("qa.code").trim(), "0", "{}", read("qa.txt"));
        std::fs::write(out.join("end-review"), "")?;
        wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.verdict') AS verdict FROM events WHERE kind = 'verification.round_closed'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(
            rows(&project, "SELECT count(*) AS n FROM acceptance_results WHERE status = 'PASS'")?,
            json!([{"n": 0}])
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_037_04_a_change_request_during_the_barrier_is_refused_and_fixed_only_on_c2_after_it()
-> R {
    // AC-037-04 (docs/adr/ADR-028.md decisions 1, 11, 12 and 13; M2T16D-656, -757): C1's QA runs its passing check,
    // then asks through `ask` that the Developer change the candidate and waits. While QA still runs: the request is
    // refused, one Issue on C1 is open, nothing is queued for the developer (no wait-for edge from QA), and D is not
    // dispatched. Once both verifiers end, the round closes repair (driven by that Issue alone; every check and both
    // reports pass), C1 is REJECTED and D's rework is dispatched after the close. The FakeAgent Developer commits and
    // completes into C2, which has no acceptance row at its freeze, nor while its QA has written evidence and its
    // reviewer still runs (no join point then); once C2's round closes, every row is C2's, from C2's verifiers.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    // The passing check is AC-035-03's scenario check here, so a round's rows can satisfy it.
    seed(
        &project,
        &format!(
            "{}\nUPDATE acceptance_criteria SET check_id = 'pass';",
            world(&spec)
        ),
    )?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.path().join("out");
    std::fs::create_dir_all(&team.out)?;
    *team.qa_script.lock().map_err(|e| e.to_string())? = Some(format!(
        "SPEC={spec}; {MESSAGE}\
         \"$HOH_RUN_CHECK\" pass > /dev/null; \
         if [ ! -e \"$OUT/asked\" ]; then : > \"$OUT/asked\"; \
           message m-change steer developer '{{\"text\": \"请 Developer 修改当前候选后我继续\", \"severity\": \"high\"}}'; \
           \"$HOH_ASK\" \"$OUT/m-change.json\" > \"$OUT/change.txt\"; : > \"$OUT/qa-waiting\"; \
           while [ ! -e \"$OUT/qa-go\" ] && [ -d \"$OUT\" ]; do sleep 0.1; done; \
         fi; exit 0"
    ));
    *team.review_script.lock().map_err(|e| e.to_string())? = Some(
        "\"$HOH_RUN_CHECK\" pass > /dev/null; \
         if [ -e \"$OUT/second-review\" ]; then : > \"$OUT/review-2-waiting\"; \
           while [ ! -e \"$OUT/end-review-2\" ] && [ -d \"$OUT\" ]; do sleep 0.1; done; fi; \
         : > \"$OUT/second-review\"; exit 0"
            .to_owned(),
    );
    *team.developer_script.lock().map_err(|e| e.to_string())? = Some(
        "git -c user.name=t -c user.email=t@example.invalid commit -q --allow-empty -m work || exit 1; exit 0".to_owned(),
    );
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let out = team.out.clone();
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let until_file = |name: &str| -> R {
            let end = Instant::now() + Duration::from_secs(60);
            while !out.join(name).exists() {
                if Instant::now() > end {
                    return Err(format!("no {name}").into());
                }
                clock.advance(step);
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        };
        until_file("qa-waiting")?;
        let c1 = candidate(&project)?.0;
        // QA still runs: refused, one Issue on C1, nothing queued for the developer, D not dispatched again.
        assert!(
            std::fs::read_to_string(out.join("change.txt"))?.contains("\"status\":\"rejected\""),
            "{}",
            std::fs::read_to_string(out.join("change.txt"))?
        );
        assert_eq!(
            rows(
                &project,
                "SELECT a.state FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'qa'"
            )?,
            json!([{"state": "RUNNING"}])
        );
        assert_eq!(
            rows(&project, "SELECT candidate_id, state FROM issues")?,
            json!([{"candidate_id": c1, "state": "OPEN"}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM outbox WHERE state IN ('PENDING', 'DELIVERING') \
                 AND json_extract(payload_json, '$.kind') IN ('query', 'steer') \
                 AND json_extract(payload_json, '$.sender_role') = 'qa' AND recipient_role = 'developer'"
            )?,
            json!([{"n": 0}]),
            "no wait-for edge from QA to the developer"
        );
        assert_eq!(
            rows(&project, "SELECT count(*) AS n FROM attempts WHERE task_id = 'D'")?,
            json!([{"n": 1}]),
            "no repair dispatched while the barrier is open"
        );
        std::fs::write(out.join("qa-go"), "")?;
        let closed = wait_for(
            &project,
            &clock,
            step,
            "SELECT seq, json_extract(payload_json, '$.verdict') AS verdict, \
             json_extract(payload_json, '$.inputs.failing_issues') AS issues FROM events \
             WHERE kind = 'verification.round_closed' ORDER BY seq",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(closed[0]["verdict"], json!("repair"), "{closed}");
        assert_eq!(
            rows(&project, "SELECT outcome FROM evidence ORDER BY outcome")?,
            json!([{"outcome": "PASS"}, {"outcome": "PASS"}]),
            "only the Issue fails the round"
        );
        assert_eq!(
            rows(&project, &format!("SELECT state FROM candidates WHERE candidate_id = '{c1}'"))?,
            json!([{"state": "REJECTED"}])
        );
        let rework = wait_for(
            &project,
            &clock,
            step,
            "SELECT o.event_seq AS seq FROM outbox o WHERE o.message_id = 'D/attempt-2/dispatch'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert!(
            rework[0]["seq"].as_i64() > closed[0]["seq"].as_i64(),
            "the rework is dispatched after the close: {rework} {closed}"
        );
        // C2 frozen: no acceptance row of its own yet.
        let c2 = wait_for(
            &project,
            &clock,
            step,
            &format!("SELECT candidate_id FROM candidates WHERE candidate_id <> '{c1}'"),
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?[0]["candidate_id"]
            .as_str()
            .ok_or("no C2")?
            .to_owned();
        let rows_of_c2 = format!("SELECT count(*) AS n FROM acceptance_results WHERE candidate_id = '{c2}'");
        assert_eq!(rows(&project, &rows_of_c2)?, json!([{"n": 0}]), "at C2's freeze");
        until_file("review-2-waiting")?;
        wait_for(
            &project,
            &clock,
            step,
            &format!(
                "SELECT count(*) AS n FROM evidence ev JOIN attempts a ON a.attempt_id = ev.attempt_id \
                 JOIN tasks t ON t.task_id = a.task_id WHERE ev.candidate_id = '{c2}' AND t.role = 'qa' \
                 AND a.state = 'SETTLED'"
            ),
            &|r| r[0]["n"].as_i64().is_some_and(|n| n >= 1),
        )?;
        assert_eq!(rows(&project, &rows_of_c2)?, json!([{"n": 0}]), "C2's QA wrote evidence, its reviewer runs");
        let join = format!(
            "SELECT count(*) AS n FROM events WHERE kind = 'verification.candidate_ready_for_join' \
             AND json_extract(payload_json, '$.candidate_id') = '{c2}'"
        );
        assert_eq!(rows(&project, &join)?, json!([{"n": 0}]), "no join point while its round is open");
        std::fs::write(out.join("end-review-2"), "")?;
        let second = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.verdict') AS verdict, json_extract(payload_json, '$.inputs') AS inputs \
             FROM events WHERE kind = 'verification.round_closed' ORDER BY seq",
            &|r| r.as_array().is_some_and(|a| a.len() >= 2),
        )?;
        assert_eq!(second[1]["verdict"], json!("passable"), "{second}");
        wait_for(&project, &clock, step, &join, &|r| r[0]["n"] == json!(1))?;
        let accepted = rows(
            &project,
            &format!(
                "SELECT ar.candidate_id AS candidate, ev.candidate_id AS evidence_candidate, t.role, \
                 json_extract(o.payload_json, '$.candidate_id') AS round_candidate FROM acceptance_results ar \
                 JOIN evidence ev ON ev.evidence_id = ar.evidence_id JOIN attempts a ON a.attempt_id = ev.attempt_id \
                 JOIN tasks t ON t.task_id = a.task_id JOIN events o ON o.kind = 'verification.round_opened' \
                 AND t.task_id IN (json_extract(o.payload_json, '$.tasks.qa'), json_extract(o.payload_json, '$.tasks.reviewer')) \
                 WHERE ar.candidate_id = '{c2}'"
            ),
        )?;
        assert!(accepted.as_array().is_some_and(|a| !a.is_empty()), "C2 has its rows: {accepted}");
        for row in accepted.as_array().into_iter().flatten() {
            assert_eq!(
                (&row["evidence_candidate"], &row["round_candidate"]),
                (&json!(c2), &json!(c2)),
                "{row}"
            );
        }
        assert_eq!(
            rows(&project, &format!("SELECT state FROM candidates WHERE candidate_id = '{c1}'"))?,
            json!([{"state": "REJECTED"}])
        );
        keep(
            "m2t16-test-037-04",
            &json!({"c1": c1, "c2": c2, "closed": closed, "rework_dispatch": rework, "c2_rows": accepted}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verifier_whose_candidate_is_no_longer_verifying_is_refused_its_run_directory() -> R {
    // docs/adr/ADR-028.md decision 10 (M2T16D-253): between a verifier's dispatch and its run directory's preparation
    // its candidate stops being VERIFYING (here a veto made it INVALID): the preparation refuses it, nothing starts,
    // no run directory is made, and the round's verifiers end CANCELLED.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Team::new(Commander::Answers, &spec, &project);
    *team
        .on_verifier_delivery
        .lock()
        .map_err(|e| e.to_string())? = Some("UPDATE candidates SET state = 'INVALID';".to_owned());
    let team = Arc::new(team);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let refused = wait_for(
            &project,
            &clock,
            step,
            "SELECT a.failure_reason AS reason FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') AND a.state = 'FAILED'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let reason = refused[0]["reason"].as_str().unwrap_or_default();
        assert!(reason.contains("RUN_DIR_REFUSED") && reason.contains("VERIFYING"), "{refused}");
        assert!(
            team.run_dirs.lock().map(|d| d.is_empty()).unwrap_or(false),
            "nothing started"
        );
        wait_for(
            &project,
            &clock,
            step,
            "SELECT DISTINCT state FROM tasks WHERE role IN ('qa', 'reviewer')",
            &|r| r == &json!([{"state": "CANCELLED"}]),
        )?;
        // Ended without a decision: nothing for the Commander to answer (M2T16R-55).
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM events WHERE kind = 'need_decision' \
                 AND json_extract(payload_json, '$.task_id') IN (SELECT task_id FROM tasks WHERE role IN ('qa', 'reviewer'))"
            )?,
            json!([{"n": 0}])
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_request_left_in_a_verifiers_bridge_is_answered_when_a_reconcile_ends_it() -> R {
    // docs/adr/ADR-028.md decision 13 (the bridge file as well as the request helper): the qa FakeAgent writes a steer
    // asking the developer to change the candidate into its bridge file and runs on; the host stops. The next host's
    // reconcile ends the attempt (its effect unknown) and answers what its bridge holds as the runner's own end would:
    // request.rejected and an OPEN Issue on the candidate, nothing queued for the developer.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.path().join("out");
    std::fs::create_dir_all(&team.out)?;
    let attempts = project.join(".hoh/attempts");
    *team.qa_script.lock().map_err(|e| e.to_string())? = Some(format!(
        "SPEC={spec}; {MESSAGE}\
         message m-left steer developer '{{\"text\": \"请 Developer 修改当前候选\", \"severity\": \"critical\"}}'; \
         b=\"{}/$(printf %s \"$ATTEMPT_ID\" | sed 's|%|%25|g; s|/|%2F|g')\"; mkdir -p \"$b\"; \
         {{ cat \"$OUT/m-left.json\"; echo; }} >> \"$b/messages.jsonl\"; : > \"$OUT/written\"; while [ -d \"$OUT\" ]; do sleep 0.1; done",
        attempts.display()
    ));
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let out = team.out.clone();
    let step = Duration::from_millis(100);
    let first = host(&project, team.clone(), clock.clone()).await?;
    let written = async {
        let end = Instant::now() + Duration::from_secs(60);
        while !out.join("written").exists() {
            if Instant::now() > end {
                return Err("the qa FakeAgent wrote nothing".into());
            }
            clock.advance(step);
            std::thread::sleep(Duration::from_millis(100));
        }
        R::Ok(())
    }
    .await;
    first.shutdown().await;
    written?;
    let c1 = candidate(&project)?.0;
    let second = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let rejected = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.message_id') AS m FROM events WHERE kind = 'request.rejected'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(rejected, json!([{"m": "m-left"}]));
        assert_eq!(
            rows(&project, "SELECT candidate_id, severity, state FROM issues")?,
            json!([{"candidate_id": c1, "severity": "critical", "state": "OPEN"}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM outbox WHERE recipient_role = 'developer' \
                 AND json_extract(payload_json, '$.kind') IN ('steer', 'query')"
            )?[0]["n"],
            json!(0)
        );
        R::Ok(())
    }
    .await;
    second.shutdown().await;
    team.end_agents();
    outcome
}

/// Submit run R's control command `kind` (its id and key `name`) at R's revision as the database has it, and again at
/// the revision the host names while it answers STALE_REVISION ([`common::fresh`]): the last answer's status and body.
async fn fresh_command(host: &Host, project: &Path, kind: &str, name: &str) -> R<(u16, Value)> {
    let revision =
        rows(project, "SELECT revision FROM runs WHERE run_id = 'R'")?[0]["revision"].clone();
    common::fresh(revision, |revision| {
        let body = json!({"schema_version": "1.0", "command_id": format!("c-{name}"), "idempotency_key": name,
            "expected_revision": revision, "kind": kind, "run_id": "R", "payload": {"reason": name}});
        async move {
            let (status, said) = answer(host, &body).await?;
            Ok((status, serde_json::from_str(&said).unwrap_or(Value::String(said))))
        }
    })
    .await
}

/// POST command `body` to the host's commands endpoint: the answer's status and body.
async fn answer(host: &Host, body: &Value) -> R<(u16, String)> {
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
    Ok((
        head.split(' ').nth(1).ok_or("no status")?.parse()?,
        body.to_owned(),
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_runs_open_round_is_closed_superseded_and_its_handoffs_rejected() -> R {
    // docs/adr/ADR-028.md decision 11 (M2T16R-53): while C1's verifiers run, the run is cancelled. Its verification is
    // still reconciled: the verifier tasks end CANCELLED, the round closes superseded (a cancelling run), its delivered
    // handoffs REJECTED.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') AND a.state = 'RUNNING'",
            &|r| r[0]["n"] == json!(2),
        )?;
        let (status, said) = fresh_command(&host, &project, "CancelRun", "cancel").await?;
        assert_eq!(status, 202, "{said}");
        let closed = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.verdict') AS verdict FROM events WHERE kind = 'verification.round_closed'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(closed, json!([{"verdict": "superseded"}]));
        assert_eq!(
            rows(&project, "SELECT DISTINCT state FROM tasks WHERE role IN ('qa', 'reviewer')")?,
            json!([{"state": "CANCELLED"}])
        );
        assert_eq!(
            rows(&project, "SELECT DISTINCT state FROM handoffs")?,
            json!([{"state": "REJECTED"}])
        );
        // Their run directories go too (M2T16R-54).
        let dirs: Vec<PathBuf> = team
            .run_dirs
            .lock()
            .map(|d| d.iter().filter_map(|(_, p)| p.clone()).collect())
            .unwrap_or_default();
        assert_eq!(dirs.len(), 2);
        let end = Instant::now() + Duration::from_secs(30);
        while dirs.iter().any(|d| d.exists()) {
            assert!(Instant::now() < end, "{dirs:?}");
            clock.advance(step);
            std::thread::sleep(Duration::from_millis(100));
        }
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_cut_short_after_its_attempts_ended_closes_the_round_when_the_host_starts_again()
-> R {
    // docs/adr/ADR-031.md decision 4's crash safety (M2T13D-302; test plan "取消的验证收尾"): the host died while the run
    // was cancelling, its verifiers' attempts ended and their round still open, before the cancel's last step. After the
    // restart the run is still CANCEL_REQUESTED, and the first cancel step closes the round superseded, rejects its
    // handoffs and ends the run, in one transaction whose last event is the run's run.result.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    let first = host(&project, team.clone(), clock.clone()).await?;
    let running = wait_for(
        &project,
        &clock,
        Duration::from_millis(100),
        "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
         WHERE t.role IN ('qa', 'reviewer') AND a.state = 'RUNNING'",
        &|r| r[0]["n"] == json!(2),
    );
    first.shutdown().await;
    team.end_agents();
    running?;
    // What the host left had it died after the cancel ended the attempts, before the cancel's last transaction.
    seed(
        &project,
        "UPDATE runs SET state = 'CANCEL_REQUESTED', resume_state = state, revision = revision + 1 WHERE run_id = 'R';
         DELETE FROM leases WHERE attempt_id IN (SELECT attempt_id FROM attempts WHERE state IN ('PREPARED', 'STARTING', 'RUNNING'));
         UPDATE attempts SET state = 'CANCELLED', cancel_requested = 1, ended_at = '2026-11-12T00:00:30Z'
           WHERE state IN ('PREPARED', 'STARTING', 'RUNNING');",
    )?;
    assert_eq!(
        rows(
            &project,
            "SELECT count(*) AS n FROM events WHERE kind = 'verification.round_closed'"
        )?,
        json!([{"n": 0}])
    );
    let second = host(&project, team.clone(), clock.clone()).await?;
    let ended = wait_for(
        &project,
        &clock,
        Duration::from_millis(100),
        "SELECT state FROM runs WHERE run_id = 'R'",
        &|r| r[0]["state"] == "CANCELLED",
    );
    second.shutdown().await;
    team.end_agents();
    ended?;
    let events = rows(
        &project,
        "SELECT kind, json_extract(payload_json, '$.verdict') AS verdict FROM events WHERE run_id = 'R' \
         AND seq >= (SELECT min(seq) FROM events WHERE kind = 'verification.round_closed') ORDER BY seq",
    )?;
    let kinds: Vec<&str> = events
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert_eq!(
        kinds.first(),
        Some(&"verification.round_closed"),
        "{events}"
    );
    assert_eq!(events[0]["verdict"], "superseded", "{events}");
    assert_eq!(kinds.last(), Some(&"run.result"), "{events}");
    assert!(kinds.contains(&"run.cancelled"), "{events}");
    assert_eq!(
        rows(
            &project,
            "SELECT DISTINCT state FROM tasks WHERE role IN ('qa', 'reviewer')"
        )?,
        json!([{"state": "CANCELLED"}])
    );
    assert_eq!(
        rows(&project, "SELECT DISTINCT state FROM handoffs")?,
        json!([{"state": "REJECTED"}])
    );
    Ok(())
}

/// TEST-053-01's specification: REQ-090 with AC-090-01 (feature a, which the project has) and AC-090-02 (feature b,
/// which it lacks), each with its TEST.
const SPEC_AB: &str = "{\"requirements\": [{\"id\": \"REQ-090\", \"acceptance\": [{\"id\": \"AC-090-01\", \"test_id\": \"TEST-090-01\"}, {\"id\": \"AC-090-02\", \"test_id\": \"TEST-090-02\"}]}]}\n";

/// Its check plan: each criterion's scenario check is a real command over the candidate's source, writing its TEST's
/// result line and failing when the feature is missing; each with a timeout, as [`PLAN`]'s.
const PLAN_AB: &str = r#"{"schema_version": "1.0", "plan_id": "missing-feature", "checks": {
  "feature-a": {"mode": "product", "artifacts": ["test-results.jsonl"], "timeout_seconds": 60, "argv": ["sh", "-c",
    "if grep -q 'pub fn a()' src/lib.rs; then s=PASS; else s=FAIL; fi; printf '{\"schema_version\": \"1.0\", \"test_id\": \"TEST-090-01\", \"name\": \"test_090_01_feature_a\", \"status\": \"%s\"}\\n' \"$s\" > \"$HOH_EVIDENCE_DIR/test-results.jsonl\"; test \"$s\" = PASS"]},
  "feature-b": {"mode": "product", "artifacts": ["test-results.jsonl"], "timeout_seconds": 60, "argv": ["sh", "-c",
    "if grep -q 'pub fn b()' src/lib.rs; then s=PASS; else s=FAIL; fi; printf '{\"schema_version\": \"1.0\", \"test_id\": \"TEST-090-02\", \"name\": \"test_090_02_feature_b\", \"status\": \"%s\"}\\n' \"$s\" > \"$HOH_EVIDENCE_DIR/test-results.jsonl\"; test \"$s\" = PASS"]}}}
"#;

/// [`world`] with D's requirement REQ-090 (AC-090-01 checked by `feature-a`, AC-090-02 by `feature-b`) instead of
/// REQ-035.
fn world_ab(spec: &str) -> String {
    format!(
        "{}
DELETE FROM task_requirements WHERE task_id = 'D';
DELETE FROM acceptance_criteria WHERE requirement_id = 'REQ-035';
DELETE FROM requirements WHERE requirement_id = 'REQ-035';
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-090', 's1', 'features', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-090-01', 'REQ-090', 's1', 'feature a', 'feature-a'), ('AC-090-02', 'REQ-090', 's1', 'feature b', 'feature-b');
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-090', 's1');",
        world(spec)
    )
}

/// TEST-053-01's run: the Developer's FakeAgent completes (its self-test record claims every TEST passed); QA's runs
/// both scenario checks through `run-check` and reports FAIL, with one high finding on AC-090-02 when `finding`; the
/// reviewer reports PASS. Once the round closes: the observables, after the assertions.
async fn missing_feature(finding: bool) -> R<Value> {
    let dir = tempfile::tempdir()?;
    let (project, spec) = project_with(dir.path(), SPEC_AB, PLAN_AB).await?;
    seed(&project, &world_ab(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.path().join("out");
    std::fs::create_dir_all(&team.out)?;
    team.self_test.store(true, Ordering::SeqCst);
    *team.report.lock().map_err(|e| e.to_string())? = "FAIL".to_owned();
    *team.review_report.lock().map_err(|e| e.to_string())? = Some("PASS".to_owned());
    *team.scope.lock().map_err(|e| e.to_string())? =
        vec!["AC-090-01".to_owned(), "AC-090-02".to_owned()];
    if finding {
        *team.qa_findings.lock().map_err(|e| e.to_string())? =
            vec![json!({"id": "F1", "severity": "high",
            "summary": "AC-090-02 FAIL：src/lib.rs 没有 b()，test_090_02_feature_b 失败"})];
    }
    *team.qa_script.lock().map_err(|e| e.to_string())? = Some(
        "\"$HOH_RUN_CHECK\" feature-a > \"$OUT/a.txt\"; \"$HOH_RUN_CHECK\" feature-b > \"$OUT/b.txt\"; exit 0".to_owned(),
    );
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let closed = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.verdict') AS verdict, json_extract(payload_json, '$.inputs') AS inputs \
             FROM events WHERE kind = 'verification.round_closed'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let (c, _, _) = candidate(&project)?;
        let results = rows(
            &project,
            "SELECT ar.ac_id, ar.candidate_id, ar.status, ar.evidence_id, ev.check_id, ev.outcome, t.role AS producer \
             FROM acceptance_results ar JOIN evidence ev ON ev.evidence_id = ar.evidence_id \
             JOIN attempts a ON a.attempt_id = ev.attempt_id JOIN tasks t ON t.task_id = a.task_id ORDER BY ar.ac_id",
        )?;
        let of = |ac: &str| -> Vec<(Value, Value)> {
            results
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["ac_id"] == json!(ac))
                .map(|r| (r["candidate_id"].clone(), r["status"].clone()))
                .collect()
        };
        let recorded = rows(
            &project,
            "SELECT ev.check_id, ev.outcome, e.payload_json AS recorded FROM evidence ev \
             LEFT JOIN events e ON e.event_id = ev.evidence_id || '/evidence' ORDER BY ev.check_id",
        )?;
        let refused = rows(&project, "SELECT kind, payload_json FROM events WHERE kind LIKE 'verification.check%'")?;
        let outs: Vec<String> = ["a.txt", "b.txt"]
            .iter()
            .map(|f| std::fs::read_to_string(team.out.join(f)).unwrap_or_else(|e| e.to_string()))
            .collect();
        assert_eq!(of("AC-090-01"), [(json!(c), json!("PASS"))], "{results} {recorded} {refused} {outs:?}");
        assert_eq!(of("AC-090-02"), [(json!(c), json!("FAIL"))], "{results}");
        // No row from a Developer's attempt: every producer is a verifier.
        for row in results.as_array().into_iter().flatten() {
            assert!(row["producer"] == json!("qa") || row["producer"] == json!("reviewer"), "{row}");
        }
        let issues = rows(
            &project,
            "SELECT issue_id, candidate_id, severity, state, report_id, finding_id, description FROM issues \
             ORDER BY issue_id",
        )?;
        let open: Vec<&Value> = issues
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["candidate_id"] == json!(c) && i["state"] == json!("OPEN"))
            .collect();
        let acceptance_issue = hoh_core::ids::child_id(&c, "acceptance/AC-090-02/test_090_02_feature_b");
        assert!(
            open.iter().any(|i| i["issue_id"] == json!(acceptance_issue) && i["report_id"].is_null()),
            "the acceptance failure's Issue: {issues}"
        );
        if finding {
            assert!(
                open.iter().any(|i| i["finding_id"] == json!("F1") && i["severity"] == json!("high")),
                "the finding's Issue: {issues}"
            );
            assert!(open.len() >= 2, "{issues}");
        }
        let states = rows(
            &project,
            &format!(
                "SELECT (SELECT state FROM candidates WHERE candidate_id = '{c}') AS candidate, \
                 (SELECT state FROM tasks WHERE task_id = 'D') AS task"
            ),
        )?;
        assert_ne!(states[0]["candidate"], json!("ACCEPTED"), "{states}");
        assert_eq!(states[0]["candidate"], json!("REJECTED"), "{states}");
        assert_ne!(states[0]["task"], json!("ACCEPTED"), "{states}");
        assert_eq!(closed[0]["verdict"], json!("repair"), "{closed}");
        let self_tests = rows(
            &project,
            "SELECT e.event_id AS event, json_extract(j.value, '$.name') AS name, json_extract(j.value, '$.sha256') \
             AS sha256 FROM events e, json_each(json_extract(e.payload_json, '$.items')) j \
             WHERE e.kind = 'attempt.artifacts' AND json_extract(j.value, '$.kind') = 'self_test' ORDER BY e.seq",
        )?;
        let digest = <sha2::Sha256 as sha2::Digest>::digest(SELF_TEST.as_bytes());
        let sha256: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(self_tests[0]["sha256"], json!(sha256), "{self_tests}");
        let tasks = rows(&project, "SELECT task_id, role, state FROM tasks ORDER BY task_id")?;
        let candidates = rows(&project, "SELECT candidate_id, state FROM candidates")?;
        let observables = json!({
            "variant": if finding { "qa_finding" } else { "no_finding" },
            "acceptance_results": results, "issues": issues, "candidates": candidates, "tasks": tasks,
            "round_closed": closed, "developer_self_tests": self_tests,
        });
        println!("{observables}");
        R::Ok(observables)
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_053_01_a_missing_feature_fails_ac_b_on_c_and_opens_an_issue() -> R {
    // AC-053-01 (REQ-053 behaviour 5, failure mode 1; docs/adr/ADR-029.md decision 9, TEST-053-01): a project missing
    // feature b, the Developer's FakeAgent COMPLETED with its self-test all PASS; QA's FakeAgent runs both scenario
    // checks (real commands) through `run-check` and reports FAIL with a high finding on AC-090-02; the reviewer PASS.
    // (AC-090-02, C) FAIL and (AC-090-01, C) PASS, every row from a verifier's evidence (none from the Developer's
    // attempt); OPEN Issues on C, the acceptance failure's and the finding's; C and D not ACCEPTED. The variant whose
    // QA report has no finding still has C's OPEN acceptance Issue (-55).
    let with = missing_feature(true).await?;
    let without = missing_feature(false).await?;
    keep(
        "m2t18-test-053-01",
        &json!({"with_finding": with, "without_finding": without}),
    )?;
    Ok(())
}

/// GET `path` of the host's API: the status and the JSON body.
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

/// TEST-053-04's run: [`world_ab`]'s criteria under a requirement each, REQ-091 (AC-090-01, feature a) for task T1
/// READY and REQ-092 (AC-090-02, feature b) for task T2 PENDING until T1 is ACCEPTED.
fn world_two(spec: &str) -> String {
    format!(
        "{}
DELETE FROM task_requirements WHERE task_id = 'D';
DELETE FROM tasks WHERE task_id = 'D';
DELETE FROM acceptance_criteria WHERE requirement_id = 'REQ-035';
DELETE FROM requirements WHERE requirement_id = 'REQ-035';
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES
  ('REQ-091', 's1', 'feature a', 1), ('REQ-092', 's1', 'feature b', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-090-01', 'REQ-091', 's1', 'feature a', 'feature-a'), ('AC-090-02', 'REQ-092', 's1', 'feature b', 'feature-b');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's1', 'READY', 1, 'developer', 1, 1, 1), ('T2', 'R', 's1', 'PENDING', 1, 'developer', 1, 2, 1);
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('T1', 'REQ-091', 's1'), ('T2', 'REQ-092', 's1');
INSERT INTO task_dependencies(task_id, depends_on) VALUES ('T2', 'T1');",
        world(spec)
    )
}

/// Run R's acceptance as the host shows it now: the snapshot (`Reader::acceptance`), getRun's acceptance summary and
/// listAcceptance's page, with the number of acceptance rows.
async fn acceptance_now(host: &Host, project: &Path) -> R<Value> {
    let snapshot = host.store().reader()?.acceptance("R")?.ok_or("no run R")?;
    let (status, run) = get(host, "/v1/runs/R").await?;
    assert_eq!(status, 200, "{run}");
    let (status, page) = get(host, "/v1/runs/R/acceptance").await?;
    assert_eq!(status, 200, "{page}");
    let count = rows(project, "SELECT count(*) AS n FROM acceptance_results")?;
    Ok(
        json!({"snapshot": snapshot, "projection": run["projection"]["acceptance"], "acceptance": page,
        "acceptance_results": count[0]["n"]}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_053_04_a_new_candidate_resets_the_current_projection_and_the_accepted_count() -> R {
    // AC-053-04 (REQ-053 failure mode 5; docs/adr/ADR-029.md decision 5, TEST-053-04): T1 (AC-090-01) completes into
    // C1; its QA runs feature-a, both verifiers PASS, the join accepts C1 and T1, and the snapshot's accepted holds
    // AC-090-01. T2 (AC-090-02, waiting for T1) then completes into C2: the snapshot's AC-090-01 is NOT_RUN on C2,
    // accepted one less, the acceptance rows as many as before, C2's qa and reviewer handoffs one each, and the events
    // `candidate.frozen{superseded: [C1]}`, `report.stale` and C2's `round_opened`. Both snapshots kept.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project_with(dir.path(), SPEC_AB, PLAN_AB).await?;
    seed(&project, &world_two(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.path().join("out");
    std::fs::create_dir_all(&team.out)?;
    *team.scope.lock().map_err(|e| e.to_string())? = vec!["AC-090-01".to_owned()];
    *team.qa_script.lock().map_err(|e| e.to_string())? =
        Some("\"$HOH_RUN_CHECK\" feature-a > \"$OUT/a.txt\"; exit 0".to_owned());
    *team.developer_script.lock().map_err(|e| e.to_string())? = Some(
        "git -c user.name=t -c user.email=t@example.invalid commit -q --allow-empty -m work || exit 1; exit 0".to_owned(),
    );
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let c1 = wait_for(&project, &clock, step, "SELECT candidate_id FROM candidates", &|r| {
            r.as_array().is_some_and(|a| !a.is_empty())
        })?[0]["candidate_id"]
            .as_str()
            .ok_or("no C1")?
            .to_owned();
        // T2's Developer waits until the snapshot before it is read.
        team.developer.store(false, Ordering::SeqCst);
        wait_for(
            &project,
            &clock,
            step,
            &format!(
                "SELECT (SELECT state FROM candidates WHERE candidate_id = '{c1}') AS c1, \
                 (SELECT state FROM tasks WHERE task_id = 'T1') AS t1"
            ),
            &|r| r[0]["c1"] == json!("ACCEPTED") && r[0]["t1"] == json!("ACCEPTED"),
        )?;
        let before = acceptance_now(&host, &project).await?;
        // C2's verifiers wait, so the snapshot after C2's freeze comes before any row of its own.
        team.release.store(false, Ordering::SeqCst);
        team.developer.store(true, Ordering::SeqCst);
        let c2 = wait_for(
            &project,
            &clock,
            step,
            &format!("SELECT candidate_id FROM candidates WHERE candidate_id <> '{c1}'"),
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?[0]["candidate_id"]
            .as_str()
            .ok_or("no C2")?
            .to_owned();
        let handoffs = wait_for(
            &project,
            &clock,
            step,
            &format!(
                "SELECT recipient_role AS role, count(*) AS n FROM outbox \
                 WHERE json_extract(payload_json, '$.payload.handoff_kind') = 'verification' \
                 AND json_extract(payload_json, '$.candidate_id') = '{c2}' GROUP BY recipient_role ORDER BY recipient_role"
            ),
            &|r| r.as_array().is_some_and(|a| a.len() == 2),
        )?;
        let after = acceptance_now(&host, &project).await?;
        let snapshot = |v: &Value, ac: &str| -> Value {
            v["snapshot"]["acs"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|a| a["ac_id"] == json!(ac))
                .map(|a| json!([a["label"], a["why"], a["counted"]]))
                .unwrap_or(Value::Null)
        };
        assert_eq!(
            before["snapshot"]["candidate"],
            json!({"candidate_id": c1, "state": "ACCEPTED"}),
            "{before}"
        );
        assert_eq!(snapshot(&before, "AC-090-01"), json!(["PASS", null, true]), "{before}");
        assert_eq!(snapshot(&before, "AC-090-02"), json!(["NOT_RUN", null, true]), "{before}");
        assert_eq!(
            before["projection"],
            json!({"candidate_id": c1, "candidate_state": "ACCEPTED", "accepted": 1, "passing": 1, "total": 2})
        );
        assert_eq!(
            after["snapshot"]["candidate"],
            json!({"candidate_id": c2, "state": "VERIFYING"}),
            "{after}"
        );
        assert_eq!(snapshot(&after, "AC-090-01"), json!(["NOT_RUN", null, true]), "{after}");
        assert_eq!(snapshot(&after, "AC-090-02"), json!(["NOT_RUN", null, true]), "{after}");
        assert_eq!(
            after["snapshot"]["accepted"].as_i64(),
            before["snapshot"]["accepted"].as_i64().map(|n| n - 1)
        );
        assert_eq!(
            after["projection"],
            json!({"candidate_id": c2, "candidate_state": "VERIFYING", "accepted": 0, "passing": 0, "total": 2})
        );
        assert_eq!(after["acceptance_results"], before["acceptance_results"]);
        assert!(before["acceptance_results"].as_i64().is_some_and(|n| n >= 1), "{before}");
        // listAcceptance: C2's labels first and latest, C1's STALE after it.
        let listed: Vec<Value> = after["acceptance"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| json!([i["candidate_id"], i["ac_id"], i["label"], i["why"], i["latest"]]))
            .collect();
        assert_eq!(
            listed,
            [
                json!([c2, "AC-090-01", "NOT_RUN", null, true]),
                json!([c2, "AC-090-02", "NOT_RUN", null, true]),
                json!([c1, "AC-090-01", "STALE", "CANDIDATE_STALE", false]),
                json!([c1, "AC-090-02", "STALE", "CANDIDATE_STALE", false]),
            ]
        );
        assert_eq!(
            handoffs,
            json!([{"role": "qa", "n": 1}, {"role": "reviewer", "n": 1}])
        );
        let events = rows(
            &project,
            "SELECT seq, kind, payload_json AS payload FROM events WHERE kind IN \
             ('candidate.frozen', 'report.stale', 'verification.round_opened') ORDER BY seq",
        )?;
        let of = |kind: &str| -> Vec<Value> {
            events
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| e["kind"] == json!(kind))
                .filter_map(|e| serde_json::from_str(e["payload"].as_str()?).ok())
                .collect()
        };
        assert!(
            of("candidate.frozen")
                .iter()
                .any(|f| f["candidate_id"] == json!(c2) && f["superseded"] == json!([c1])),
            "{events}"
        );
        let stale = of("report.stale");
        assert!(
            stale.iter().any(|s| s["reason"] == json!("superseded")
                && s["report_ids"].as_array().is_some_and(|r| r.len() == 2)),
            "C1's two reports made STALE: {events}"
        );
        assert!(
            of("verification.round_opened")
                .iter()
                .any(|o| o["candidate_id"] == json!(c2) && o["source_task_ids"] == json!(["T2"])),
            "{events}"
        );
        assert_eq!(
            rows(
                &project,
                "SELECT state FROM independent_reports ORDER BY report_id"
            )?,
            json!([{"state": "STALE"}, {"state": "STALE"}])
        );
        keep(
            "m2t18-test-053-04",
            &json!({"c1": c1, "c2": c2, "before": before, "after": after, "handoffs": handoffs,
                "events": events}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_054_04_findings_settled_through_the_host_trace_to_their_report_and_raw_session() -> R
{
    // AC-054-04 on the product path (docs/adr/ADR-029.md decisions 9 and 15): the reviewer's FakeAgent reports FAIL with
    // F1 critical (evidence in src/a.rs) and F2 low, QA PASS, settled through the host. Two Issues on C, critical and
    // low, each with its fingerprint and finding id; each traces through `issue_chain` and `verify_chain` to its report
    // object, its finding, the raw session and native attempt objects, the session and the stdout, every link holding.
    // C REJECTED (the critical finding fails the round).
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Team::new(Commander::Answers, &spec, &project);
    *team.review_report.lock().map_err(|e| e.to_string())? = Some("FAIL".to_owned());
    *team.review_findings.lock().map_err(|e| e.to_string())? = vec![
        json!({"id": "F1", "severity": "critical", "summary": "[缺陷] src/a.rs:12 — 越界访问",
            "evidence": [{"path": "src/a.rs", "sha256": "a".repeat(64)}]}),
        json!({"id": "F2", "severity": "low", "summary": "[建议] src/b.rs:3 — 命名"}),
    ];
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let closed = wait_for(
            &project,
            &clock,
            step,
            "SELECT json_extract(payload_json, '$.verdict') AS verdict FROM events \
             WHERE kind = 'verification.round_closed'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let (c, _, _) = candidate(&project)?;
        let issues = rows(
            &project,
            "SELECT issue_id, candidate_id, severity, fingerprint, finding_id, report_id FROM issues \
             WHERE finding_id IS NOT NULL ORDER BY finding_id",
        )?;
        let found: Vec<(Value, Value, Value)> = issues
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| (i["finding_id"].clone(), i["severity"].clone(), i["candidate_id"].clone()))
            .collect();
        assert_eq!(
            found,
            [
                (json!("F1"), json!("critical"), json!(c)),
                (json!("F2"), json!("low"), json!(c))
            ],
            "{issues}"
        );
        let mut traced = Vec::new();
        for issue in issues.as_array().into_iter().flatten() {
            assert!(
                issue["fingerprint"].as_str().is_some_and(|f| f.len() == 64),
                "{issue}"
            );
            let id = issue["issue_id"].as_str().ok_or("no issue id")?;
            let chain = host
                .store()
                .reader()?
                .issue_chain(id)?
                .ok_or("a finding's Issue has a chain")?;
            let check = hoh_core::verification::report::verify_chain(&project, &chain);
            assert!(check.holds(), "{id}: {check:?} {chain:?}");
            assert_eq!(chain.report.state, "CURRENT");
            assert_eq!(json!(chain.report.report_id), issue["report_id"]);
            traced.push(json!({"chain": chain, "check": check}));
        }
        assert_eq!(closed[0]["verdict"], json!("repair"), "{closed}");
        assert_eq!(
            rows(&project, &format!("SELECT state FROM candidates WHERE candidate_id = '{c}'"))?,
            json!([{"state": "REJECTED"}])
        );
        keep(
            "m2t18-test-054-04",
            &json!({"issues": issues, "traced": traced, "round_closed": closed}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// TEST-054-02's run: [`world_two`] with T2 independent of T1 and READY, so T2 can freeze C2 while C1's verifiers run.
fn world_side_by_side(spec: &str) -> String {
    format!(
        "{}
DELETE FROM task_dependencies WHERE task_id = 'T2';
UPDATE tasks SET state = 'READY', revision = revision + 1 WHERE task_id = 'T2';",
        world_two(spec)
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_054_02_a_report_on_a_superseded_candidate_is_stale() -> R {
    // AC-054-02 on the product path (REQ-054 behaviour 4, failure mode 2; docs/adr/ADR-029.md decision 8, ADR-028
    // decision 9): T1's FakeAgent Developer commits and completes into C1, whose QA and Reviewer FakeAgents start and
    // wait for the test's word. T2's Developer (held until then) commits and completes into C2: C1 STALE, T1 taken
    // into C2's round. Then C1's verifiers end and report PASS on C1: the Reviewer's report row is STALE (queryable,
    // kept), C1's round closes superseded, C2's review in the snapshot is NOT_RUN while C2's Reviewer has not
    // completed, and exactly one kind=handoff message goes to the reviewer with candidate_id C2.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project_with(dir.path(), SPEC_AB, PLAN_AB).await?;
    seed(&project, &world_side_by_side(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let mut team = Team::new(Commander::Answers, &spec, &project);
    team.out = dir.path().join("out");
    std::fs::create_dir_all(&team.out)?;
    let out = team.out.clone();
    let quoted = out
        .to_str()
        .ok_or("the out directory is not UTF-8")?
        .to_owned();
    *team.scope.lock().map_err(|e| e.to_string())? = vec!["AC-090-01".to_owned()];
    // The first Developer turn (T1's) commits at once; a later one (T2's) waits for `second-go` before it commits.
    *team.developer_script.lock().map_err(|e| e.to_string())? = Some(format!(
        "if [ -e '{quoted}/first-done' ]; then : > '{quoted}/second-waiting'; \
           while [ ! -e '{quoted}/second-go' ] && [ -d '{quoted}' ]; do sleep 0.1; done; fi; \
         : > '{quoted}/first-done'; \
         git -c user.name=t -c user.email=t@example.invalid commit -q --allow-empty -m work || exit 1; exit 0"
    ));
    // A verifier started before `second-round` exists is C1's and ends on `c1-go`; one started after it, C2's, on
    // `c2-go`. Each says it waits.
    let verifier = |role: &str| {
        format!(
            "if [ -e \"$OUT/second-round\" ]; then w=c2; else w=c1; fi; : > \"$OUT/$w-{role}-waiting\"; \
             while [ ! -e \"$OUT/$w-go\" ] && [ -d \"$OUT\" ]; do sleep 0.1; done; exit 0"
        )
    };
    *team.qa_script.lock().map_err(|e| e.to_string())? = Some(verifier("qa"));
    *team.review_script.lock().map_err(|e| e.to_string())? = Some(verifier("reviewer"));
    team.release.store(true, Ordering::SeqCst);
    let team = Arc::new(team);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let until_file = |name: &str| -> R {
            let end = Instant::now() + Duration::from_secs(60);
            while !out.join(name).exists() {
                if Instant::now() > end {
                    return Err(format!("no {name}").into());
                }
                clock.advance(step);
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        };
        // C1 frozen from T1; its QA and Reviewer run; T2's Developer runs, held.
        until_file("c1-qa-waiting")?;
        until_file("c1-reviewer-waiting")?;
        until_file("second-waiting")?;
        let (c1, _, _) = candidate(&project)?;
        let opened = |candidate: &str| -> R<Value> {
            let found = rows(
                &project,
                &format!(
                    "SELECT json_extract(payload_json, '$.round_id') AS round_id, \
                     json_extract(payload_json, '$.source_task_ids') AS sources FROM events \
                     WHERE kind = 'verification.round_opened' AND json_extract(payload_json, '$.candidate_id') = '{candidate}'"
                ),
            )?;
            Ok(found[0].clone())
        };
        let c1_round = opened(&c1)?;
        assert_eq!(c1_round["sources"], json!("[\"T1\"]"), "{c1_round}");
        let c1_round_id = c1_round["round_id"].as_str().ok_or("no C1 round")?.to_owned();
        assert_eq!(
            rows(
                &project,
                "SELECT t.role, a.state FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE t.role IN ('qa', 'reviewer') ORDER BY t.role"
            )?,
            json!([{"role": "qa", "state": "RUNNING"}, {"role": "reviewer", "state": "RUNNING"}]),
            "C1's verifiers run when C2 is frozen"
        );
        // T2 completes into C2.
        std::fs::write(out.join("second-round"), "")?;
        std::fs::write(out.join("second-go"), "")?;
        let c2 = wait_for(
            &project,
            &clock,
            step,
            &format!("SELECT candidate_id FROM candidates WHERE candidate_id <> '{c1}'"),
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?[0]["candidate_id"]
            .as_str()
            .ok_or("no C2")?
            .to_owned();
        assert_eq!(
            rows(&project, &format!("SELECT state FROM candidates WHERE candidate_id = '{c1}'"))?,
            json!([{"state": "STALE"}])
        );
        let c2_round = opened(&c2)?;
        let sources: Vec<String> =
            serde_json::from_str(c2_round["sources"].as_str().ok_or("no C2 sources")?)?;
        assert!(
            sources.contains(&"T1".to_owned()) && sources.contains(&"T2".to_owned()),
            "T1 is taken into C2's round: {c2_round}"
        );
        assert_eq!(
            rows(&project, "SELECT count(*) AS n FROM independent_reports")?,
            json!([{"n": 0}]),
            "no report before C1's verifiers end"
        );
        // C1's verifiers end and report PASS on C1.
        std::fs::write(out.join("c1-go"), "")?;
        let c1_reports = wait_for(
            &project,
            &clock,
            step,
            &format!(
                "SELECT report_id, role, status, state, candidate_id, session_id, attempt_id FROM independent_reports \
                 WHERE candidate_id = '{c1}' ORDER BY role"
            ),
            &|r| r.as_array().is_some_and(|a| a.len() == 2),
        )?;
        let closed = wait_for(
            &project,
            &clock,
            step,
            &format!(
                "SELECT json_extract(payload_json, '$.verdict') AS verdict FROM events \
                 WHERE kind = 'verification.round_closed' AND json_extract(payload_json, '$.round_id') = '{c1_round_id}'"
            ),
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        // C2's Reviewer has not completed (it waits for `c2-go`): C2's review is NOT_RUN.
        let snapshot = host.store().reader()?.acceptance("R")?.ok_or("no run R")?;
        let c2_rows = rows(
            &project,
            &format!("SELECT count(*) AS n FROM independent_reports WHERE candidate_id = '{c2}'"),
        )?;
        let shown = acceptance_now(&host, &project).await?;
        let handoffs = rows(
            &project,
            &format!(
                "SELECT message_id, state, event_seq FROM outbox WHERE recipient_role = 'reviewer' \
                 AND json_extract(payload_json, '$.kind') = 'handoff' \
                 AND json_extract(payload_json, '$.candidate_id') = '{c2}'"
            ),
        )?;
        let events = rows(
            &project,
            "SELECT seq, kind, payload_json AS payload FROM events WHERE kind IN ('candidate.frozen', 'report.stale', \
             'verification.report', 'verification.round_opened', 'verification.round_closed') ORDER BY seq",
        )?;
        let outbox = rows(
            &project,
            "SELECT message_id, recipient_role, state, json_extract(payload_json, '$.kind') AS kind, \
             json_extract(payload_json, '$.candidate_id') AS candidate_id FROM outbox \
             WHERE json_extract(payload_json, '$.payload.handoff_kind') = 'verification' ORDER BY event_seq, message_id",
        )?;
        keep(
            "m2t18-test-054-02",
            &json!({"c1": c1, "c2": c2, "c1_round": c1_round, "c2_round": c2_round, "c1_reports": c1_reports,
                "c1_round_closed": closed, "c2_report_rows": c2_rows, "snapshot": shown, "c2_reviewer_handoffs": handoffs,
                "verification_outbox": outbox, "events": events}),
        )?;
        let review = c1_reports
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["role"] == json!("reviewer"))
            .ok_or("no reviewer report on C1")?;
        assert_eq!(
            (&review["status"], &review["state"], &review["candidate_id"]),
            (&json!("PASS"), &json!("STALE"), &json!(c1)),
            "{c1_reports}"
        );
        assert_eq!(
            c1_reports
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| r["state"].clone())
                .collect::<Vec<_>>(),
            [json!("STALE"), json!("STALE")],
            "{c1_reports}"
        );
        let stale_events: Vec<Value> = events
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["kind"] == json!("report.stale"))
            .filter_map(|e| serde_json::from_str(e["payload"].as_str()?).ok())
            .collect();
        assert!(
            stale_events.iter().any(|s| s["report_ids"] == json!([review["report_id"]])
                && s["reason"] == json!("superseded")
                && s["candidate_id"] == json!(c1)),
            "{events}"
        );
        assert_eq!(closed, json!([{"verdict": "superseded"}]));
        assert_eq!(c2_rows, json!([{"n": 0}]), "C2's Reviewer has not completed");
        assert_eq!(
            (
                snapshot.candidate.as_ref().map(|c| c.candidate_id.clone()),
                snapshot.review,
                snapshot.qa
            ),
            (
                Some(c2.clone()),
                hoh_core::verification::classify::Report::NotRun,
                hoh_core::verification::classify::Report::NotRun
            ),
            "{shown}"
        );
        assert_eq!(shown["snapshot"]["review"], json!("NOT_RUN"), "{shown}");
        assert_eq!(handoffs.as_array().map(Vec::len), Some(1), "{handoffs}");
        // C1's report rows are kept and queryable after the close.
        assert_eq!(
            rows(
                &project,
                &format!(
                    "SELECT role, state, status FROM independent_reports WHERE candidate_id = '{c1}' ORDER BY role"
                )
            )?,
            json!([{"role": "qa", "state": "STALE", "status": "PASS"},
                   {"role": "reviewer", "state": "STALE", "status": "PASS"}])
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// TEST-020-02's team (docs/adr/ADR-030.md, its test plan): every role claude but qa, which is pi falling back to claude.
const FALLBACK_TEAM: &str = "config_version = \"1.0\"\n\
[team.commander]\nagent = \"claude\"\n[team.cto]\nagent = \"claude\"\n[team.developer]\nagent = \"claude\"\n\
[team.qa]\nagent = \"pi\"\nfallback = [\"claude\"]\n[team.researcher]\nagent = \"claude\"\n[team.reviewer]\nagent = \"claude\"\n";

/// [`world`] with every role bound as StartRun binds it: at epoch 1 of the project's current team revision with no
/// session yet (staffing grants them), claude for every role but qa (pi).
fn world_unbound(spec: &str) -> String {
    format!(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, '{spec}', 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, config_revision, created_at) VALUES
  ('R', :p, 's1', 'RUNNING', 'AUTO', 7, (SELECT max(revision) FROM team_revisions WHERE state = 'APPLIED'),
   '2026-11-12T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-035', 's1', 'r', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-035-03', 'REQ-035', 's1', 'handoffs without the Commander', 'integration');
INSERT INTO role_bindings(run_id, role, agent, role_epoch, config_revision, bound_at)
  SELECT 'R', role, agent, 1, (SELECT max(revision) FROM team_revisions WHERE state = 'APPLIED'), '2026-11-12T00:00:00Z'
  FROM (SELECT 'commander' AS role, 'claude' AS agent UNION ALL SELECT 'cto', 'claude' UNION ALL
        SELECT 'developer', 'claude' UNION ALL SELECT 'qa', 'pi' UNION ALL SELECT 'researcher', 'claude' UNION ALL
        SELECT 'reviewer', 'claude');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('D', 'R', 's1', 'READY', 1, 'developer', 1, 1, 1);
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-035', 's1');"
    )
}

/// A fake `claude` in `bin` answering `--version` and `--help` with the unmodified recorded 2.1.318 help.
fn fake_claude(bin: &Path) -> R {
    use std::os::unix::fs::PermissionsExt as _;
    let help = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/protocol/claude/2.1.318/help.txt"
    );
    std::fs::create_dir_all(bin)?;
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '2.1.318 (Claude Code)';;\n --help) cat '{help}';;\n *) exit 2;;\nesac\n"
        ),
    )?;
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_020_02_qa_falls_back_to_claude_in_a_session_of_its_own() -> R {
    // AC-020-02 (docs/adr/ADR-030.md decisions 3 to 5, its test plan's TEST-020-02): no pi on the host's PATH, qa
    // configured pi falling back to claude, claude (answering the unmodified recorded 2.1.318 help) registered by the
    // product's compat suite. Staffing switches qa to claude at epoch 2 with its evidence; D (claude) completes into a
    // candidate and the QA attempt runs as claude in a session that is not the Developer's, with command execution.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    let bin = dir.path().join("bin");
    fake_claude(&bin)?;
    let path = format!("{}:/usr/bin:/bin", bin.display());
    std::fs::write(project.join(".hoh/config.toml"), FALLBACK_TEAM)?;
    // The file becomes the project's team revision at a host's start.
    Host::start(config(&project)).await?.shutdown().await;
    let registered = hoh_app::compat::register(
        &project,
        hoh_agents::compat::Agent::Claude,
        std::ffi::OsStr::new(&path),
    )?;
    assert_eq!(
        format!("{:?}", registered.entry.suite.status),
        "Pass",
        "{registered:?}"
    );
    seed(&project, &world_unbound(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    if let Ok(mut registry) = team.registry.lock() {
        *registry = Some(Arc::new(hoh_app::host::agents::Registry::with_pi_dir(
            path.clone().into(),
            Some(dir.path().join("pi-agent")),
        )));
    }
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let qa = wait_for(
            &project,
            &clock,
            step,
            "SELECT a.attempt_id, a.agent, a.session_id, a.role_epoch FROM attempts a JOIN tasks t \
             ON t.task_id = a.task_id WHERE t.role = 'qa'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let qa = &qa[0];
        assert_eq!(qa["agent"], "claude", "{qa}");
        assert_eq!(qa["role_epoch"], 2, "{qa}");
        let developer = rows(
            &project,
            "SELECT session_id FROM attempts WHERE task_id = 'D'",
        )?;
        let s_dev = developer[0]["session_id"].clone();
        assert!(s_dev.is_string(), "{developer}");
        assert_ne!(qa["session_id"], s_dev, "QA ran in the Developer's session");
        assert_eq!(
            rows(
                &project,
                "SELECT agent, role_epoch FROM role_bindings WHERE run_id = 'R' AND role = 'qa'"
            )?,
            json!([{"agent": "claude", "role_epoch": 2}])
        );
        let session = rows(
            &project,
            &format!(
                "SELECT agent, role, json_extract(capabilities_json, '$.command_execution') AS command_execution \
                 FROM sessions WHERE session_id = '{}'",
                qa["session_id"].as_str().unwrap_or_default()
            ),
        )?;
        assert_eq!(
            session,
            json!([{"agent": "claude", "role": "qa", "command_execution": 1}])
        );
        let fallback = rows(
            &project,
            "SELECT payload_json FROM events WHERE run_id = 'R' AND kind = 'team.binding_fallback'",
        )?;
        assert_eq!(fallback.as_array().map(Vec::len), Some(1), "{fallback}");
        let fallback: Value =
            serde_json::from_str(fallback[0]["payload_json"].as_str().unwrap_or("null"))?;
        assert_eq!(
            (&fallback["role"], &fallback["from"], &fallback["chosen"]),
            (&json!("qa"), &json!("pi"), &json!("claude")),
            "{fallback}"
        );
        assert!(
            fallback["evaluated"]
                .as_array()
                .is_some_and(|e| e.contains(&json!("pi:not_installed"))),
            "{fallback}"
        );
        let sha256 = fallback["probe_sha256"].as_str().ok_or("no probe sha256")?;
        assert!(
            fallback["probe_artifact_id"]
                .as_str()
                .is_some_and(|a| a.starts_with(sha256)),
            "{fallback}"
        );
        // The object's bytes match its name (get recomputes the digest).
        let probe: Value = serde_json::from_slice(&hoh_core::objects::get(&project, sha256)?)?;
        let pi = probe
            .as_array()
            .and_then(|items| items.iter().find(|i| i["agent"] == "pi"))
            .ok_or("no pi in the evidence")?;
        assert_eq!(pi["probe"]["class"], "NotFound", "{probe}");
        let registered_artifact = rows(
            &project,
            &format!(
                "SELECT count(*) AS n FROM artifacts WHERE artifact_id = '{}'",
                fallback["probe_artifact_id"].as_str().unwrap_or_default()
            ),
        )?;
        assert_eq!(registered_artifact, json!([{"n": 1}]));
        let qa_events = rows(
            &project,
            "SELECT kind, json_extract(payload_json, '$.role_epoch') AS role_epoch, \
             json_extract(payload_json, '$.reason') AS reason, json_extract(payload_json, '$.cause') AS cause \
             FROM events WHERE run_id = 'R' AND kind IN ('team.binding_blocked', 'team.binding_applied') \
             AND json_extract(payload_json, '$.role') = 'qa' ORDER BY seq",
        )?;
        assert_eq!(
            qa_events,
            json!([
                {"kind": "team.binding_blocked", "role_epoch": 1, "reason": "AGENT_UNAVAILABLE", "cause": "staffing"},
                {"kind": "team.binding_applied", "role_epoch": null, "reason": null, "cause": "fallback"},
            ])
        );
        keep_s3("test-020-02-probe", &probe)?;
        keep_s3(
            "test-020-02-attempts",
            &rows(
                &project,
                "SELECT a.attempt_id, t.role, a.state, a.agent, a.session_id, a.role_epoch, a.config_revision \
                 FROM attempts a JOIN tasks t ON t.task_id = a.task_id ORDER BY a.rowid",
            )?,
        )?;
        keep_s3("test-020-02-sessions", &rows(&project, "SELECT * FROM sessions ORDER BY session_id")?)?;
        keep_s3(
            "test-020-02-events",
            &rows(
                &project,
                "SELECT seq, event_id, kind, payload_json FROM events WHERE kind LIKE 'team.%' \
                 OR kind = 'handoff.prepared' ORDER BY seq",
            )?,
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// Keep `value` as `<name>.json` in `$HOH_S3_EVIDENCE` when it is set (the slice's evidence directory).
fn keep_s3(name: &str, value: &Value) -> R {
    if let Ok(dir) = std::env::var("HOH_S3_EVIDENCE") {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    keep(name, value)
}

/// TEST-020-03's team (docs/adr/ADR-030.md, its test plan): every role claude but qa, pi with no fallback.
const PI_QA_TEAM: &str = "config_version = \"1.0\"\n\
[team.commander]\nagent = \"claude\"\n[team.cto]\nagent = \"claude\"\n[team.developer]\nagent = \"claude\"\n\
[team.qa]\nagent = \"pi\"\n[team.researcher]\nagent = \"claude\"\n[team.reviewer]\nagent = \"claude\"\n";

/// The recorded pi 1.5.0 help.
const PI_HELP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/pi/1.5.0/help.txt"
);

/// A fake `pi` 1.5.0 in `bin` (`note` a comment line of its own: another implementation): the unmodified recorded
/// help; its status command (`auth check`) says not logged in (credentials not configured) until `<bin>/pi.d/logged-in`
/// exists. pi's settings in `pi_dir` name a default provider, so the registry runs that command (ADR-024 decision 7).
fn fake_pi(bin: &Path, pi_dir: &Path, note: &str) -> R {
    fake_pi_answering(bin, pi_dir, note, Path::new(PI_HELP))
}

/// [`fake_pi`] answering `--help` with the file `help`.
fn fake_pi_answering(bin: &Path, pi_dir: &Path, note: &str, help: &Path) -> R {
    use std::os::unix::fs::PermissionsExt as _;
    let d = bin.join("pi.d");
    std::fs::create_dir_all(&d)?;
    std::fs::create_dir_all(pi_dir)?;
    std::fs::write(
        pi_dir.join("settings.json"),
        "{\"defaultProvider\": \"anthropic\"}",
    )?;
    // Written beside it and renamed over it: the registry sees another file, never a half-written one.
    let staged = bin.join(".pi.new");
    std::fs::write(
        &staged,
        format!(
            "#!/bin/sh\n# {note}\ncase \"$1 $2\" in\n --version*) echo '1.5.0';;\n --help*) cat '{help}';;\n \
             'auth check') if [ -e '{d}/logged-in' ]; then echo '{{\"status\": \"ready\"}}'; else \
             echo '{{\"status\": \"not_ready\", \"reason\": \"credentials_not_configured\"}}'; exit 1; fi;;\n \
             *) exit 2;;\nesac\n",
            d = d.display(),
            help = help.display()
        ),
    )?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&staged, bin.join("pi"))?;
    Ok(())
}

/// [`world_unbound`] with qa pi and a second developer writer task T3 (independent of D) when `t3`.
fn world_pi_qa(spec: &str, t3: bool) -> String {
    let mut world = world_unbound(spec);
    if t3 {
        world.push_str(
            "\nINSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
             VALUES ('T3', 'R', 's1', 'READY', 1, 'developer', 1, 2, 1);",
        );
    }
    world
}

/// A project of the logged-out-qa world (TEST-020-03's): PI_QA_TEAM, claude and pi on PATH and registered by the
/// product's compat suite, pi logged in only when `logged_in`; the run seeded (with T3 when `t3`). The project, the
/// specification's digest, the bin directory and the registry's PATH.
async fn pi_qa_world(
    dir: &Path,
    t3: bool,
    logged_in: bool,
) -> R<(PathBuf, String, PathBuf, String)> {
    pi_qa_world_with(dir, t3, logged_in, PI_QA_TEAM).await
}

/// [`pi_qa_world`] with the team configuration `team` (qa pi in it, every other role claude).
async fn pi_qa_world_with(
    dir: &Path,
    t3: bool,
    logged_in: bool,
    team: &str,
) -> R<(PathBuf, String, PathBuf, String)> {
    let (project, spec) = project(dir).await?;
    let bin = dir.join("bin");
    fake_claude(&bin)?;
    fake_pi(&bin, &dir.join("pi-agent"), "A")?;
    if logged_in {
        std::fs::write(bin.join("pi.d/logged-in"), "")?;
    }
    let path = format!("{}:/usr/bin:/bin", bin.display());
    std::fs::write(project.join(".hoh/config.toml"), team)?;
    Host::start(config(&project)).await?.shutdown().await;
    for agent in [
        hoh_agents::compat::Agent::Claude,
        hoh_agents::compat::Agent::Pi,
    ] {
        let registered = hoh_app::compat::register(&project, agent, std::ffi::OsStr::new(&path))?;
        assert_eq!(
            format!("{:?}", registered.entry.suite.status),
            "Pass",
            "{registered:?}"
        );
    }
    seed(&project, &world_pi_qa(&spec, t3))?;
    Ok((project, spec, bin, path))
}

/// The Team of the logged-out-qa world: the product's registry over `path`, pi's settings in `<dir>/pi-agent`; T3's
/// FakeAgent held.
fn pi_qa_team(dir: &Path, project: &Path, spec: &str, path: &str) -> Arc<Team> {
    let team = Arc::new(Team::new(Commander::Answers, spec, project));
    if let Ok(mut registry) = team.registry.lock() {
        *registry = Some(Arc::new(hoh_app::host::agents::Registry::with_pi_dir(
            path.to_owned().into(),
            Some(dir.join("pi-agent")),
        )));
    }
    if let Ok(mut held) = team.held_tasks.lock() {
        held.push("T3".to_owned());
    }
    team
}

/// The qa member of run R's team view.
async fn qa_member(host: &Host) -> R<Value> {
    let (status, page) = get(host, "/v1/runs/R/team").await?;
    assert_eq!(status, 200, "{page}");
    page["items"]
        .as_array()
        .and_then(|items| items.iter().find(|m| m["role"] == "qa"))
        .cloned()
        .ok_or_else(|| format!("no qa member: {page}").into())
}

/// The candidate D completed into, and its qa verification task, once qa's task is BLOCKED.
fn qa_blocked(project: &Path, clock: &FakeClock) -> R<(String, String)> {
    let step = Duration::from_millis(100);
    let opened = wait_for(
        project,
        clock,
        step,
        "SELECT json_extract(payload_json, '$.candidate_id') AS candidate, \
         json_extract(payload_json, '$.tasks.qa') AS qa FROM events WHERE kind = 'verification.round_opened'",
        &|r| r.as_array().is_some_and(|a| !a.is_empty()),
    )?;
    let candidate = opened[0]["candidate"]
        .as_str()
        .ok_or("no candidate")?
        .to_owned();
    let qa = opened[0]["qa"].as_str().ok_or("no qa task")?.to_owned();
    wait_for(
        project,
        clock,
        step,
        &format!("SELECT state, blocked_reason FROM tasks WHERE task_id = '{qa}'"),
        &|r| r[0]["state"] == "BLOCKED",
    )?;
    Ok((candidate, qa))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_020_03_a_logged_out_qa_blocks_itself_only() -> R {
    // AC-020-03 (docs/adr/ADR-030.md decisions 4 and 7, K4): pi installed and registered but its status command says
    // it is not logged in, qa pi with no fallback; D (T1) and T3 developer writers, T3 independent of T1. D completes into
    // C, which is frozen into verification; then the clock moves on an hour, tick by tick, T3's FakeAgent held. The
    // qa member is BLOCKED(BLOCKED_AUTH) missing [pi:BLOCKED_AUTH]; qa has no attempt; its verification task is BLOCKED
    // for it with attempt_count 0; C has no PASS row and stays VERIFYING; T3 was dispatched; no qa outbox row waits for
    // a retry. The variant without T3: once the reviewer ended, the run is BLOCKED(BLOCKED_AUTH), while with T3
    // running it is not.
    let mut kept = serde_json::Map::new();
    for t3 in [true, false] {
        let dir = tempfile::tempdir()?;
        let (project, spec, _bin, path) = pi_qa_world(dir.path(), t3, false).await?;
        let clock = Arc::new(FakeClock::default());
        let team = pi_qa_team(dir.path(), &project, &spec, &path);
        // The reviewer ends once dispatched; T3 is held.
        team.release.store(true, Ordering::SeqCst);
        let host = host(&project, team.clone(), clock.clone()).await?;
        let outcome = async {
            let step = Duration::from_millis(100);
            let (candidate, qa) = qa_blocked(&project, &clock)?;
            let reviewer_done = wait_for(
                &project,
                &clock,
                step,
                "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE t.role = 'reviewer' AND a.state = 'SETTLED'",
                &|r| r[0]["n"] == 1,
            )?;
            assert_eq!(reviewer_done[0]["n"], 1);
            if t3 {
                wait_for(
                    &project,
                    &clock,
                    step,
                    "SELECT count(*) AS n FROM attempts WHERE task_id = 'T3' AND state = 'RUNNING'",
                    &|r| r[0]["n"] == 1,
                )?;
                // An hour, tick by tick.
                for _ in 0..360 {
                    clock.advance(Duration::from_secs(10));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } else {
                let run = wait_for(
                    &project,
                    &clock,
                    step,
                    "SELECT state, blocked_reason FROM runs WHERE run_id = 'R'",
                    &|r| r[0]["state"] == "BLOCKED",
                )?;
                assert_eq!(run[0]["blocked_reason"], "BLOCKED_AUTH", "{run}");
            }
            let member = qa_member(&host).await?;
            assert_eq!(
                (&member["state"], &member["blocked_reason"], &member["missing"]),
                (&json!("BLOCKED"), &json!("BLOCKED_AUTH"), &json!(["pi:BLOCKED_AUTH"])),
                "{member}"
            );
            assert_eq!(
                rows(
                    &project,
                    "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'qa'"
                )?,
                json!([{"n": 0}])
            );
            assert_eq!(
                rows(
                    &project,
                    &format!(
                        "SELECT state, blocked_reason, attempt_count FROM tasks WHERE task_id = '{qa}'"
                    )
                )?,
                json!([{"state": "BLOCKED", "blocked_reason": "BLOCKED_AUTH", "attempt_count": 0}])
            );
            assert_eq!(
                rows(
                    &project,
                    &format!(
                        "SELECT count(*) AS n FROM acceptance_results WHERE candidate_id = '{candidate}' \
                         AND status = 'PASS'"
                    )
                )?,
                json!([{"n": 0}])
            );
            assert_eq!(
                rows(
                    &project,
                    &format!("SELECT state FROM candidates WHERE candidate_id = '{candidate}'")
                )?,
                json!([{"state": "VERIFYING"}])
            );
            assert_eq!(
                rows(
                    &project,
                    "SELECT count(*) AS n FROM outbox WHERE recipient_role = 'qa' AND next_retry_at IS NOT NULL"
                )?,
                json!([{"n": 0}])
            );
            let run = rows(&project, "SELECT state, blocked_reason FROM runs WHERE run_id = 'R'")?;
            if t3 {
                let t3_attempts = rows(&project, "SELECT count(*) AS n FROM attempts WHERE task_id = 'T3'")?;
                assert!(t3_attempts[0]["n"].as_i64().is_some_and(|n| n >= 1), "{t3_attempts}");
                assert_ne!(run[0]["state"], "BLOCKED", "{run}");
            } else {
                assert_eq!(
                    run,
                    json!([{"state": "BLOCKED", "blocked_reason": "BLOCKED_AUTH"}])
                );
            }
            kept.insert(
                if t3 { "with_t3" } else { "without_t3" }.to_owned(),
                json!({
                    "qa_member": member,
                    "tasks": rows(&project, "SELECT task_id, role, state, blocked_reason, attempt_count, next_retry_at FROM tasks ORDER BY created_seq")?,
                    "attempts": rows(&project, "SELECT a.attempt_id, t.role, a.state, a.agent, a.session_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id ORDER BY a.rowid")?,
                    "role_bindings": rows(&project, "SELECT role, agent, role_epoch, session_id, state, blocked_reason, missing_json FROM role_bindings ORDER BY role")?,
                    "outbox_qa": rows(&project, "SELECT message_id, state, attempt_count, next_retry_at FROM outbox WHERE recipient_role = 'qa'")?,
                    "run": run,
                    "events": rows(&project, "SELECT seq, kind, payload_json FROM events WHERE kind IN ('team.binding_blocked', 'task.blocked', 'task.unblocked', 'run.blocked', 'run.resumed') ORDER BY seq")?,
                }),
            );
            R::Ok(())
        }
        .await;
        host.shutdown().await;
        team.end_agents();
        outcome?;
    }
    keep("test-020-03", &Value::Object(kept))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_auth_verifier_is_dispatched_once_its_agent_logs_in() -> R {
    // CR-018 (docs/cr/CR-018-verification-hand-overs.md:30; docs/adr/ADR-030.md decision 7): TEST-020-03's world, its
    // qa task BLOCKED(BLOCKED_AUTH). pi logs in: after the registry's next login look, staffing grants qa a session,
    // the pace writes task.unblocked{reason: BLOCKED_AUTH}, the task is READY again and dispatched to pi.
    let dir = tempfile::tempdir()?;
    let (project, spec, bin, path) = pi_qa_world(dir.path(), false, false).await?;
    let clock = Arc::new(FakeClock::default());
    let team = pi_qa_team(dir.path(), &project, &spec, &path);
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let (_, qa) = qa_blocked(&project, &clock)?;
        std::fs::write(bin.join("pi.d/logged-in"), "")?;
        // The registry looks at the login again within its 30 s (ADR-024 decision 7).
        let until = Instant::now() + Duration::from_secs(90);
        let attempt = loop {
            let found = rows(
                &project,
                &format!("SELECT attempt_id, agent, session_id FROM attempts WHERE task_id = '{qa}'"),
            )?;
            if found.as_array().is_some_and(|a| !a.is_empty()) {
                break found[0].clone();
            }
            if Instant::now() > until {
                return Err(format!("qa was not dispatched: {found}").into());
            }
            clock.advance(step);
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(attempt["agent"], "pi", "{attempt}");
        let unblocked = rows(
            &project,
            &format!(
                "SELECT json_extract(payload_json, '$.reason') AS reason FROM events WHERE kind = 'task.unblocked' \
                 AND json_extract(payload_json, '$.task_id') = '{qa}'"
            ),
        )?;
        assert_eq!(unblocked, json!([{"reason": "BLOCKED_AUTH"}]));
        assert_eq!(
            rows(
                &project,
                "SELECT state, session_id IS NOT NULL AS bound FROM role_bindings WHERE run_id = 'R' AND role = 'qa'"
            )?,
            json!([{"state": "ACTIVE", "bound": 1}])
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_auth_failure_of_a_replaced_pi_does_not_block_qa() -> R {
    // M2T15D-07 (docs/adr/ADR-030.md decisions 2, 6 and 7, K8): pi A logged in and registered, qa's turn runs as A,
    // held. pi on PATH is replaced by B; the registry looks at B and its suite passes (the writer slot is free). A's turn
    // then fails BLOCKED_AUTH: its task and its binding are BLOCKED_AUTH, but the registry's fact stays B's (A's failure
    // is an older identity's). With no restart and no other upgrade, qa is rebound (epoch 2, its session granted under
    // B), the task is unblocked, and a new QA attempt runs in B's session.
    let dir = tempfile::tempdir()?;
    let (project, spec, bin, path) = pi_qa_world(dir.path(), false, true).await?;
    let clock = Arc::new(FakeClock::default());
    let team = pi_qa_team(dir.path(), &project, &spec, &path);
    if let Ok(mut fails) = team.verifier_fails.lock() {
        *fails = Some(hoh_core::retry::FailureClass::Auth);
    }
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let first = wait_for(
            &project,
            &clock,
            step,
            "SELECT a.attempt_id, a.session_id, a.role_epoch FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role = 'qa' AND a.state = 'RUNNING'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let (a_attempt, a_session) = (
            first[0]["attempt_id"].as_str().ok_or("no attempt")?.to_owned(),
            first[0]["session_id"].as_str().ok_or("no session")?.to_owned(),
        );
        assert_eq!(first[0]["role_epoch"], 1);
        fake_pi(&bin, &dir.path().join("pi-agent"), "B")?;
        let b_sha256 = file_sha256(&bin.join("pi"))?;
        wait_for(
            &project,
            &clock,
            step,
            &format!(
                "SELECT json_extract(payload_json, '$.status') AS status FROM events WHERE kind = 'compat.suite_completed' \
                 AND json_extract(payload_json, '$.agent') = 'pi' \
                 AND json_extract(payload_json, '$.implementation_sha256') = '{b_sha256}'"
            ),
            &|r| r.as_array().is_some_and(|a| a.len() == 1),
        )?;
        team.release.store(true, Ordering::SeqCst);
        let a_end = wait_for(
            &project,
            &clock,
            step,
            &format!("SELECT state, failure_reason FROM attempts WHERE attempt_id = '{a_attempt}'"),
            &|r| r[0]["state"] == "FAILED",
        )?;
        assert_eq!(a_end[0]["failure_reason"], "BLOCKED_AUTH");
        let blocked = rows(
            &project,
            &format!(
                "SELECT json_extract(payload_json, '$.cause') AS cause, json_extract(payload_json, '$.reason') AS reason \
                 FROM events WHERE kind = 'team.binding_blocked' AND json_extract(payload_json, '$.attempt_id') = '{a_attempt}'"
            ),
        )?;
        assert_eq!(blocked, json!([{"cause": "turn", "reason": "BLOCKED_AUTH"}]));
        let ignored = rows(
            &project,
            &format!(
                "SELECT count(*) AS n FROM events WHERE kind = 'registry.turn_auth_ignored' \
                 AND json_extract(payload_json, '$.attempt_id') = '{a_attempt}'"
            ),
        )?;
        assert_eq!(ignored, json!([{"n": 1}]), "A's failure was not only a diagnostic");
        // The task is unblocked and dispatched again in a new session granted under B.
        let second = wait_for(
            &project,
            &clock,
            step,
            &format!(
                "SELECT a.attempt_id, a.session_id, a.role_epoch FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE t.role = 'qa' AND a.attempt_id <> '{a_attempt}'"
            ),
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let b_session = second[0]["session_id"].as_str().ok_or("no session")?.to_owned();
        assert_ne!(b_session, a_session);
        assert_eq!(second[0]["role_epoch"], 2);
        let applied = rows(
            &project,
            "SELECT json_extract(payload_json, '$.cause') AS cause FROM events WHERE kind = 'team.binding_applied' \
             AND json_extract(payload_json, '$.role') = 'qa'",
        )?;
        assert_eq!(applied, json!([{"cause": "rebind"}]));
        let bound = rows(
            &project,
            &format!(
                "SELECT json_extract(payload_json, '$.identity.implementation_sha256') AS sha256 FROM events \
                 WHERE kind = 'team.session_bound' AND json_extract(payload_json, '$.session_id') = '{b_session}'"
            ),
        )?;
        assert_eq!(bound, json!([{"sha256": b_sha256}]));
        let unblocked = rows(
            &project,
            "SELECT json_extract(payload_json, '$.reason') AS reason FROM events WHERE kind = 'task.unblocked'",
        )?;
        assert_eq!(unblocked, json!([{"reason": "BLOCKED_AUTH"}]));
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turns_auth_failure_while_pi_says_logged_in_switches_qa_to_its_fallback() -> R {
    // AC-020-05's then-clause on the product path (docs/adr/ADR-030.md decisions 2 and 4 to 7, M2T15R-50):
    // TEST-020-03's world with TEST-020-02's team (qa pi falling back to claude), pi logged in and registered. qa's
    // turn runs as pi and fails BLOCKED_AUTH under pi's current identity while pi's status command keeps saying it is
    // logged in. The host runner's end of the turn latches it as pi's fact: staffing switches qa to claude at epoch 2
    // (its evidence shows pi's fact is the turn's failure), and the task is unblocked and dispatched to claude in a
    // session of its own. pi is never unblocked: no team.binding_unblocked, and no other qa turn runs as pi.
    let dir = tempfile::tempdir()?;
    let (project, spec, bin, path) =
        pi_qa_world_with(dir.path(), false, true, FALLBACK_TEAM).await?;
    let clock = Arc::new(FakeClock::default());
    let team = pi_qa_team(dir.path(), &project, &spec, &path);
    if let Ok(mut fails) = team.verifier_fails.lock() {
        *fails = Some(hoh_core::retry::FailureClass::Auth);
    }
    // Only pi's turn fails: the reviewer (claude) and qa's fallback (claude) report.
    if let Ok(mut fails_as) = team.verifier_fails_as.lock() {
        *fails_as = Some("pi".to_owned());
    }
    let registry = hoh_app::host::Executor::registry(&*team).ok_or("no registry")?;
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        let first = wait_for(
            &project,
            &clock,
            step,
            "SELECT a.attempt_id, a.agent, a.session_id, a.role_epoch FROM attempts a JOIN tasks t \
             ON t.task_id = a.task_id WHERE t.role = 'qa' AND a.state = 'RUNNING'",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        let (a_attempt, a_session) = (
            first[0]["attempt_id"].as_str().ok_or("no attempt")?.to_owned(),
            first[0]["session_id"].as_str().ok_or("no session")?.to_owned(),
        );
        assert_eq!(
            (&first[0]["agent"], &first[0]["role_epoch"]),
            (&json!("pi"), &json!(1)),
            "{first}"
        );
        // pi's status command says it is logged in: its fact is usable before the turn fails.
        let before = registry.fact(hoh_agents::compat::Agent::Pi);
        assert!(
            matches!(before, Some(hoh_core::staffing::Fact::Usable(_))),
            "{before:?}"
        );
        team.release.store(true, Ordering::SeqCst);
        let a_end = wait_for(
            &project,
            &clock,
            step,
            &format!("SELECT state, failure_reason FROM attempts WHERE attempt_id = '{a_attempt}'"),
            &|r| r[0]["state"] == "FAILED",
        )?;
        assert_eq!(a_end[0]["failure_reason"], "BLOCKED_AUTH", "{a_end}");
        // The turn blocked pi's binding, and its failure was latched (not only said, as an older identity's is).
        assert_eq!(
            rows(
                &project,
                &format!(
                    "SELECT json_extract(payload_json, '$.cause') AS cause, json_extract(payload_json, '$.reason') \
                     AS reason, json_extract(payload_json, '$.agent') AS agent, json_extract(payload_json, \
                     '$.role_epoch') AS role_epoch FROM events WHERE kind = 'team.binding_blocked' \
                     AND json_extract(payload_json, '$.attempt_id') = '{a_attempt}'"
                ),
            )?,
            json!([{"cause": "turn", "reason": "BLOCKED_AUTH", "agent": "pi", "role_epoch": 1}])
        );
        assert_eq!(
            rows(
                &project,
                &format!(
                    "SELECT count(*) AS n FROM events WHERE kind = 'registry.turn_auth_ignored' \
                     AND json_extract(payload_json, '$.attempt_id') = '{a_attempt}'"
                ),
            )?,
            json!([{"n": 0}])
        );
        // Whichever comes first: qa switched to its fallback, or pi's blocked binding unblocked.
        let judged = wait_for(
            &project,
            &clock,
            step,
            "SELECT kind, payload_json FROM events WHERE run_id = 'R' \
             AND kind IN ('team.binding_fallback', 'team.binding_unblocked') \
             AND json_extract(payload_json, '$.role') = 'qa' ORDER BY seq",
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(
            judged.as_array().map(|j| j.iter().map(|e| e["kind"].clone()).collect::<Vec<_>>()),
            Some(vec![json!("team.binding_fallback")]),
            "pi was not switched from: {judged}"
        );
        let fallback: Value =
            serde_json::from_str(judged[0]["payload_json"].as_str().unwrap_or("null"))?;
        assert_eq!(
            (&fallback["from"], &fallback["chosen"], &fallback["evaluated"]),
            (
                &json!("pi"),
                &json!("claude"),
                &json!(["pi:BLOCKED_AUTH", "claude:usable"])
            ),
            "{fallback}"
        );
        // The switch's evidence: pi found ready, its fact the latched turn's failure, not its status command's answer.
        let sha256 = fallback["probe_sha256"].as_str().ok_or("no probe sha256")?;
        let probe: Value = serde_json::from_slice(&hoh_core::objects::get(&project, sha256)?)?;
        let pi = probe
            .as_array()
            .and_then(|items| items.iter().find(|i| i["agent"] == "pi"))
            .ok_or("no pi in the evidence")?;
        assert_eq!(
            (&pi["line"], &pi["probe"]["class"], &pi["probe"]["detail"]),
            (
                &json!("pi:BLOCKED_AUTH"),
                &Value::Null,
                &json!(format!("turn {a_attempt} failed BLOCKED_AUTH"))
            ),
            "{probe}"
        );
        // The task is unblocked and dispatched to claude at epoch 2, in a new session of claude's.
        let second = wait_for(
            &project,
            &clock,
            step,
            &format!(
                "SELECT a.attempt_id, a.agent, a.session_id, a.role_epoch FROM attempts a JOIN tasks t \
                 ON t.task_id = a.task_id WHERE t.role = 'qa' AND a.attempt_id <> '{a_attempt}'"
            ),
            &|r| r.as_array().is_some_and(|a| !a.is_empty()),
        )?;
        assert_eq!(second.as_array().map(Vec::len), Some(1), "{second}");
        assert_eq!(
            (&second[0]["agent"], &second[0]["role_epoch"]),
            (&json!("claude"), &json!(2)),
            "{second}"
        );
        let b_session = second[0]["session_id"].as_str().ok_or("no session")?;
        assert_ne!(b_session, a_session);
        assert_eq!(
            rows(
                &project,
                &format!("SELECT agent, role FROM sessions WHERE session_id = '{b_session}'")
            )?,
            json!([{"agent": "claude", "role": "qa"}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT agent, role_epoch, session_id FROM role_bindings WHERE run_id = 'R' AND role = 'qa'"
            )?,
            json!([{"agent": "claude", "role_epoch": 2, "session_id": b_session}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT json_extract(payload_json, '$.cause') AS cause FROM events WHERE kind = 'team.binding_applied' \
                 AND json_extract(payload_json, '$.role') = 'qa'"
            )?,
            json!([{"cause": "fallback"}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT json_extract(payload_json, '$.reason') AS reason, json_extract(payload_json, '$.role_epoch') \
                 AS role_epoch FROM events WHERE kind = 'task.unblocked' \
                 AND json_extract(payload_json, '$.role') = 'qa'"
            )?,
            json!([{"reason": "BLOCKED_AUTH", "role_epoch": 2}])
        );
        // pi was never unblocked, though its status command still says it is logged in: its fact stays the turn's.
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM events WHERE kind = 'team.binding_unblocked' \
                 AND json_extract(payload_json, '$.role') = 'qa'"
            )?,
            json!([{"n": 0}])
        );
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE t.role = 'qa' AND a.agent = 'pi'"
            )?,
            json!([{"n": 1}])
        );
        let status = Command::new(bin.join("pi")).args(["auth", "check"]).output()?;
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stdout)
        );
        assert_eq!(
            registry.fact(hoh_agents::compat::Agent::Pi),
            Some(hoh_core::staffing::Fact::LoginMissing(format!(
                "turn {a_attempt} failed BLOCKED_AUTH"
            )))
        );
        keep(
            "auth-latch-product-path",
            &json!({
                "probe": probe,
                "attempts": rows(&project, "SELECT a.attempt_id, t.role, a.state, a.agent, a.session_id, a.role_epoch, a.failure_reason FROM attempts a JOIN tasks t ON t.task_id = a.task_id ORDER BY a.rowid")?,
                "role_bindings": rows(&project, "SELECT role, agent, role_epoch, session_id, state, blocked_reason, missing_json FROM role_bindings ORDER BY role")?,
                "events": rows(&project, "SELECT seq, kind, payload_json FROM events WHERE kind LIKE 'team.%' OR kind LIKE 'task.%' OR kind LIKE 'registry.%' ORDER BY seq")?,
            }),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// The capability product path's team (docs/adr/ADR-030.md, its test plan "能力的产品路径"): every role claude but the
/// developer, which is codex falling back to pi.
const PI_DEVELOPER_TEAM: &str = "config_version = \"1.0\"\n\
[team.commander]\nagent = \"claude\"\n[team.cto]\nagent = \"claude\"\n[team.developer]\nagent = \"codex\"\n\
fallback = [\"pi\"]\n[team.qa]\nagent = \"claude\"\n[team.researcher]\nagent = \"claude\"\n[team.reviewer]\n\
agent = \"claude\"\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_developer_fallback_whose_help_turns_bash_off_is_not_selected() -> R {
    // docs/adr/ADR-030.md decision 12 and its test plan ("能力的产品路径", M2T15D-06, -150): the developer is codex,
    // not on the host's PATH, falling back to pi. pi is logged in and registered by the product's compat suite, but its
    // help is the recorded 1.5.0 help with one fact changed: `bash` is `(off by default)`. Staffing does not select pi:
    // the developer binding stays codex's at epoch 1, BLOCKED with the missing line
    // `pi:CAPABILITY_UNSUPPORTED:command_execution`, the evidence of the block shows pi's probe found no command
    // execution, and D has no attempt. The positive control on the product path is TEST-020-02.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    let bin = dir.path().join("bin");
    fake_claude(&bin)?;
    let recorded = std::fs::read_to_string(PI_HELP)?;
    let bash = "  bash       - Execute bash commands\n";
    assert_eq!(
        recorded.matches(bash).count(),
        1,
        "the recorded help's bash line"
    );
    let help = dir.path().join("pi-help.txt");
    std::fs::write(
        &help,
        recorded.replace(
            bash,
            "  bash       - Execute bash commands (off by default)\n",
        ),
    )?;
    let pi_dir = dir.path().join("pi-agent");
    fake_pi_answering(&bin, &pi_dir, "A", &help)?;
    std::fs::write(bin.join("pi.d/logged-in"), "")?;
    let path = format!("{}:/usr/bin:/bin", bin.display());
    std::fs::write(project.join(".hoh/config.toml"), PI_DEVELOPER_TEAM)?;
    // The file becomes the project's team revision at a host's start.
    Host::start(config(&project)).await?.shutdown().await;
    for agent in [
        hoh_agents::compat::Agent::Claude,
        hoh_agents::compat::Agent::Pi,
    ] {
        let registered = hoh_app::compat::register(&project, agent, std::ffi::OsStr::new(&path))?;
        assert_eq!(
            format!("{:?}", registered.entry.suite.status),
            "Pass",
            "{registered:?}"
        );
    }
    // Run R bound as StartRun binds this team: the developer codex, every other role claude.
    seed(
        &project,
        &format!(
            "{}\nUPDATE role_bindings SET agent = 'codex' WHERE run_id = 'R' AND role = 'developer';\n\
             UPDATE role_bindings SET agent = 'claude' WHERE run_id = 'R' AND role = 'qa';",
            world_unbound(&spec)
        ),
    )?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    if let Ok(mut registry) = team.registry.lock() {
        *registry = Some(Arc::new(hoh_app::host::agents::Registry::with_pi_dir(
            path.clone().into(),
            Some(pi_dir.clone()),
        )));
    }
    let host = host(&project, team.clone(), clock.clone()).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        // Whichever comes first: the developer blocked, or pi selected (the binding moved on from codex).
        let developer = wait_for(
            &project,
            &clock,
            step,
            "SELECT agent, role_epoch, state, blocked_reason, missing_json FROM role_bindings \
             WHERE run_id = 'R' AND role = 'developer'",
            &|r| r[0]["state"] == "BLOCKED" || r[0]["agent"] != "codex",
        )?;
        let developer = &developer[0];
        let missing: Value =
            serde_json::from_str(developer["missing_json"].as_str().unwrap_or("null"))?;
        assert_eq!(
            (
                &developer["agent"],
                &developer["role_epoch"],
                &developer["state"],
                &developer["blocked_reason"],
                &missing
            ),
            (
                &json!("codex"),
                &json!(1),
                &json!("BLOCKED"),
                &json!("AGENT_UNAVAILABLE"),
                &json!(["codex:not_installed", "pi:CAPABILITY_UNSUPPORTED:command_execution"])
            ),
            "{developer}"
        );
        let blocked = rows(
            &project,
            "SELECT payload_json FROM events WHERE run_id = 'R' AND kind = 'team.binding_blocked' \
             AND json_extract(payload_json, '$.role') = 'developer'",
        )?;
        assert_eq!(blocked.as_array().map(Vec::len), Some(1), "{blocked}");
        let blocked: Value =
            serde_json::from_str(blocked[0]["payload_json"].as_str().unwrap_or("null"))?;
        assert_eq!(blocked["missing"], missing, "{blocked}");
        let sha256 = blocked["probe_sha256"].as_str().ok_or("no probe sha256")?;
        let probe: Value = serde_json::from_slice(&hoh_core::objects::get(&project, sha256)?)?;
        let pi = probe
            .as_array()
            .and_then(|items| items.iter().find(|i| i["agent"] == "pi"))
            .ok_or("no pi in the evidence")?;
        assert_eq!(
            pi["line"], "pi:CAPABILITY_UNSUPPORTED:command_execution",
            "{probe}"
        );
        assert_eq!(
            (
                &pi["probe"]["capabilities"]["command_execution"],
                &pi["probe"]["capabilities"]["structured_events"]
            ),
            (&json!(false), &json!(true)),
            "{probe}"
        );
        assert_eq!(
            rows(
                &project,
                "SELECT count(*) AS n FROM events WHERE run_id = 'R' \
                 AND kind IN ('team.binding_fallback', 'team.binding_applied') \
                 AND json_extract(payload_json, '$.role') = 'developer'"
            )?,
            json!([{"n": 0}])
        );
        assert_eq!(
            rows(&project, "SELECT count(*) AS n FROM attempts WHERE task_id = 'D'")?,
            json!([{"n": 0}])
        );
        keep(
            "capability-product-path",
            &json!({
                "role_bindings": rows(&project, "SELECT role, agent, role_epoch, session_id, state, blocked_reason, missing_json FROM role_bindings ORDER BY role")?,
                "probe": probe,
                "events": rows(&project, "SELECT seq, kind, payload_json FROM events WHERE kind LIKE 'team.%' OR kind LIKE 'task.%' ORDER BY seq")?,
            }),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome
}

/// The runner's ticks (counted after each tick's staffing) and every Held answer of the store's writer with the tick
/// it fell in (docs/adr/ADR-031.md correction D's property).
#[derive(Debug, Default)]
struct HeldTicks {
    tick: Arc<std::sync::atomic::AtomicU64>,
    held: std::sync::Mutex<Vec<(u64, &'static str, String)>>,
}

impl hoh_core::persistence::FaultHooks for HeldTicks {
    fn held(&self, job: &'static str, run: &str) {
        let tick = self.tick.load(Ordering::SeqCst);
        if let Ok(mut held) = self.held.lock() {
            held.push((tick, job, run.to_owned()));
        }
    }
}

#[derive(Debug)]
struct CountTicks(Arc<std::sync::atomic::AtomicU64>);

impl hoh_app::host::HostHooks for CountTicks {
    fn runner(&self, point: hoh_app::host::RunnerPoint) {
        if point == hoh_app::host::RunnerPoint::AfterStaffing {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// [`host`] with its store's and its runner's hooks `held`.
async fn host_held(
    project: &Path,
    team: Arc<Team>,
    clock: Arc<FakeClock>,
    held: &Arc<HeldTicks>,
) -> R<Host> {
    let mut config = config(project);
    config.clock = hoh_app::host::HostClock(clock);
    config.store.faults = held.clone();
    config.hooks = Arc::new(CountTicks(held.tick.clone()));
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: team,
        context: Context {
            required_capabilities: ["commander", "developer", "qa", "reviewer", "researcher"]
                .map(|r| (r.to_owned(), Default::default()))
                .into(),
            commander_dir: project.parent().map(|d| d.join("state/hoh/commander")),
            ..Context::default()
        },
    });
    Ok(Host::start(config).await?)
}

/// Wait until the runner has ticked `n` more times, moving `clock` on.
fn ticked(held: &HeldTicks, clock: &FakeClock, n: u64) -> R {
    let until = held.tick.load(Ordering::SeqCst) + n;
    let end = Instant::now() + Duration::from_secs(30);
    while held.tick.load(Ordering::SeqCst) < until {
        if Instant::now() > end {
            return Err(format!("the runner did not tick {n} times").into());
        }
        clock.advance(Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_the_verification_world_a_cancelled_run_is_held_only_in_the_tick_of_its_stop() -> R {
    // docs/adr/ADR-031.md test plan, 停住之后的写入口 (2), over this file's world (C1's verifiers running when the run is
    // cancelled, the world of the cancel close-out): every Held of the run falls in one tick; after five more ticks and
    // a restart with five more, none, and nothing of the run after its run.result.
    let dir = tempfile::tempdir()?;
    let (project, spec) = project(dir.path()).await?;
    seed(&project, &world(&spec))?;
    let clock = Arc::new(FakeClock::default());
    let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
    let held = Arc::new(HeldTicks::default());
    let host = host_held(&project, team.clone(), clock.clone(), &held).await?;
    let outcome = async {
        let step = Duration::from_millis(100);
        wait_for(
            &project,
            &clock,
            step,
            "SELECT count(*) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') AND a.state = 'RUNNING'",
            &|r| r[0]["n"] == json!(2),
        )?;
        let (status, said) = fresh_command(&host, &project, "CancelRun", "cancel").await?;
        assert_eq!(status, 202, "{said}");
        wait_for(
            &project,
            &clock,
            step,
            "SELECT state FROM runs WHERE run_id = 'R'",
            &|r| r[0]["state"] == "CANCELLED",
        )?;
        ticked(&held, &clock, 5)
    }
    .await;
    host.shutdown().await;
    team.end_agents();
    outcome?;
    let first = held.held.lock().map(|h| h.clone()).unwrap_or_default();
    let ticks: std::collections::BTreeSet<u64> = first.iter().map(|(t, ..)| *t).collect();
    assert!(ticks.len() <= 1, "{first:?}");
    let last = rows(
        &project,
        "SELECT kind, seq FROM events WHERE run_id = 'R' ORDER BY seq DESC LIMIT 1",
    )?;
    assert_eq!(last[0]["kind"], "run.result");
    let restarted = Arc::new(HeldTicks::default());
    let host = host_held(&project, team.clone(), clock.clone(), &restarted).await?;
    let outcome = ticked(&restarted, &clock, 5);
    host.shutdown().await;
    team.end_agents();
    outcome?;
    assert_eq!(restarted.held.lock().map(|h| h.len()).unwrap_or(1), 0);
    assert_eq!(
        rows(
            &project,
            "SELECT kind, seq FROM events WHERE run_id = 'R' ORDER BY seq DESC LIMIT 1"
        )?,
        last
    );
    Ok(())
}

/// Makes its directory readable again when dropped, however the test ends, so the test's directory can be removed.
struct ReadableAgain(PathBuf);

impl Drop for ReadableAgain {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// The world after a first host whose verifiers' starts were refused (C1's round open, its verifier attempts FAILED,
/// none running), with a pending record of C1 its recovery cannot read (a transient fault: its check directory made
/// unreadable): the candidate, the pending file and the check directory.
async fn with_unreadable_pending(
    dir: &Path,
    team: &Arc<Team>,
    clock: &Arc<FakeClock>,
    project: &Path,
) -> R<(String, PathBuf, PathBuf)> {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = dir;
    team.refuse_verifiers.store(true, Ordering::SeqCst);
    let first = host(project, team.clone(), clock.clone()).await?;
    let started = async {
        wait_for(
            project,
            clock,
            Duration::from_millis(100),
            "SELECT count(DISTINCT t.task_id) AS n FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role IN ('qa', 'reviewer') AND a.state = 'FAILED'",
            &|r| r[0]["n"] == json!(2),
        )?;
        R::Ok(candidate(project)?.0)
    }
    .await;
    first.shutdown().await;
    team.refuse_verifiers.store(false, Ordering::SeqCst);
    let id = started?;
    let record = "00000000-0000-4000-8000-000000000004";
    let check_dir = project.join(format!(".hoh/evidence/{id}/unit"));
    std::fs::create_dir_all(check_dir.join(record))?;
    std::fs::create_dir_all(project.join(".hoh/pending"))?;
    let pending = project.join(format!(".hoh/pending/{record}.json"));
    std::fs::write(
        &pending,
        json!({"record": {"candidate_id": id}}).to_string(),
    )?;
    std::fs::set_permissions(&check_dir, std::fs::Permissions::from_mode(0o000))?;
    Ok((id, pending, check_dir))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_stopped_incomplete_leaves_its_pending_record_alone_and_a_paused_one_recovers_it() -> R
{
    // docs/adr/ADR-031.md correction D, A1 (-400; the test plan's 停住之后的写入口 (5)): the verifiers' attempts ended, C1's
    // round open, its pending record in backoff after a transient failure. The deadline passes: pace stops the run
    // INCOMPLETE with its run.result. Five ticks past the record's recovery_not_before and a restart with five more: no
    // event of the run after its run.result, no project-scoped record of it, the pending file still there, the count of
    // candidate.reconcile_failed unchanged. The same world PAUSED by PauseRun instead: the recovery goes on as today, its
    // event the run's. (The INCOMPLETE world resumed is slice S1d's ResumeRun.)
    use std::os::unix::fs::PermissionsExt as _;
    for paused in [false, true] {
        let dir = tempfile::tempdir()?;
        let (project, spec) = project(dir.path()).await?;
        seed(&project, &world(&spec))?;
        let clock = Arc::new(FakeClock::default());
        let team = Arc::new(Team::new(Commander::Answers, &spec, &project));
        let (_, pending, check_dir) =
            with_unreadable_pending(dir.path(), &team, &clock, &project).await?;
        let _readable = ReadableAgain(check_dir.clone());
        if !paused {
            seed(
                &project,
                "UPDATE runs SET deadline_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '+20 seconds') WHERE run_id = 'R';",
            )?;
        }
        let held = Arc::new(HeldTicks::default());
        let host = host_held(&project, team.clone(), clock.clone(), &held).await?;
        let step = Duration::from_millis(500);
        let failed = "SELECT count(*) AS n FROM events WHERE kind = 'candidate.reconcile_failed'";
        let outcome = async {
            wait_for(&project, &clock, Duration::from_millis(100), failed, &|r| {
                r[0]["n"].as_i64().is_some_and(|n| n >= 1)
            })?;
            if paused {
                let (status, said) = fresh_command(&host, &project, "PauseRun", "pause").await?;
                assert_eq!(status, 202, "{said}");
                wait_for(
                    &project,
                    &clock,
                    step,
                    "SELECT state FROM runs WHERE run_id = 'R'",
                    &|r| r[0]["state"] == "PAUSED",
                )?;
                let result = rows(
                    &project,
                    "SELECT max(seq) AS s FROM events WHERE run_id = 'R' AND kind = 'run.result'",
                )?[0]["s"]
                    .as_i64()
                    .ok_or("no run.result")?;
                // The fault goes and the record is put aside: the recovery, not held by PAUSED, succeeds, as the run's.
                std::fs::set_permissions(&check_dir, std::fs::Permissions::from_mode(0o755))?;
                std::fs::remove_file(&pending)?;
                let recovered = wait_for(
                    &project,
                    &clock,
                    step,
                    "SELECT run_id, seq FROM events WHERE kind = 'candidate.reconcile_recovered'",
                    &|r| r.as_array().is_some_and(|a| !a.is_empty()),
                )?;
                assert_eq!(recovered[0]["run_id"], "R");
                assert!(
                    recovered[0]["seq"].as_i64().is_some_and(|s| s > result),
                    "{recovered}"
                );
                return R::Ok(());
            }
            wait_for(
                &project,
                &clock,
                step,
                "SELECT state FROM runs WHERE run_id = 'R'",
                &|r| r[0]["state"] == "INCOMPLETE",
            )?;
            R::Ok(())
        }
        .await;
        if paused {
            host.shutdown().await;
            team.end_agents();
            outcome?;
            continue;
        }
        let snapshot = || -> R<(Value, Value, Value)> {
            Ok((
                rows(
                    &project,
                    "SELECT seq, kind FROM events WHERE run_id = 'R' ORDER BY seq",
                )?,
                rows(
                    &project,
                    "SELECT count(*) AS n FROM events WHERE run_id IS NULL AND json_extract(payload_json, '$.run_id') = 'R'",
                )?,
                rows(&project, failed)?,
            ))
        };
        let outcome = async {
            outcome?;
            let at_stop = snapshot()?;
            assert_eq!(
                at_stop
                    .0
                    .as_array()
                    .and_then(|a| a.last())
                    .map(|e| e["kind"].clone()),
                Some(json!("run.result"))
            );
            for _ in 0..5 {
                clock.advance(Duration::from_secs(5));
                ticked(&held, &clock, 1)?;
            }
            R::Ok(at_stop)
        }
        .await;
        host.shutdown().await;
        team.end_agents();
        let at_stop = outcome?;
        assert_eq!(snapshot()?, at_stop);
        let restarted = Arc::new(HeldTicks::default());
        let host = host_held(&project, team.clone(), clock.clone(), &restarted).await?;
        let outcome = (|| {
            for _ in 0..5 {
                clock.advance(Duration::from_secs(5));
                ticked(&restarted, &clock, 1)?;
            }
            R::Ok(())
        })();
        host.shutdown().await;
        team.end_agents();
        outcome?;
        assert_eq!(snapshot()?, at_stop);
        assert!(pending.exists(), "the pending record is still there");
        assert_eq!(restarted.held.lock().map(|h| h.len()).unwrap_or(1), 0);
        std::fs::set_permissions(&check_dir, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
