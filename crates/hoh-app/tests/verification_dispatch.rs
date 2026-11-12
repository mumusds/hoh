//! A verification round's dispatch through a host with its runner (docs/adr/ADR-028.md decisions 3, 4, 9 and 10):
//! TEST-035-03, TEST-035-05, TEST-036-03, TEST-021-05. A Team executor plays every role: the Developer's FakeAgent writes a
//! COMPLETED TaskResult through its bridge (its first `fail` turns fail transiently), the verifiers' FakeAgents run
//! until released, and the Commander's turns are counted (answered with an assumption, or refused at their start).

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

/// The project's frozen check plan: `unit` fails with exit code 1 and changes nothing.
const PLAN: &str = r#"{"schema_version": "1.0", "plan_id": "fixture", "checks": {
  "unit": {"mode": "product", "argv": ["sh", "-c", "exit 1"]},
  "pass": {"mode": "product", "artifacts": ["test-results.jsonl"], "argv": ["sh", "-c",
    "printf '%s\\n' '{\"schema_version\": \"1.0\", \"test_id\": \"TEST-035-03\", \"name\": \"test_035_03_works\", \"status\": \"PASS\"}' > \"$HOH_EVIDENCE_DIR/test-results.jsonl\""]}}}
"#;

/// The project's specification as committed (its sha256 is the run's spec digest).
const SPEC: &str = "{\"requirements\": [{\"id\": \"REQ-035\", \"acceptance\": [{\"id\": \"AC-035-03\", \"test_id\": \"TEST-035-03\"}]}]}\n";

/// A committed project whose layout the freeze takes (docs/adr/ADR-028.md decision 4), its store made by one host's
/// start and stop; the project and the specification's digest.
async fn project(dir: &Path) -> R<(PathBuf, String)> {
    let project = dir.join("p");
    std::fs::create_dir_all(&project)?;
    git(&project, &["init", "-q"])?;
    for (path, text) in [
        ("src/lib.rs", "pub fn a() {}\n"),
        ("contracts/requirements.json", SPEC),
        ("tests/check-plan.json", PLAN),
    ] {
        let file = project.join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, text)?;
    }
    git(&project, &["add", "."])?;
    git(&project, &["commit", "-q", "-m", "work"])?;
    Host::start(config(&project)).await?.shutdown().await;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(SPEC.as_bytes());
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
}

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
        }
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
                held = Some(self.developer.clone());
                failed = self
                    .fail
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                if !failed {
                    messages.push(
                        json!({
                            "schema_version": "1.0", "run_id": work.run_id, "task_id": work.task_id,
                            "attempt_id": work.attempt_id, "role": work.role,
                            "role_epoch": work.assignment.role_epoch, "status": "COMPLETED",
                            "summary": "implemented", "candidate_id": null, "evidence": [], "issues": [],
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
                // A verifier reports its work done through its bridge, and its verdict in a hoh-report block of its
                // final message, its raw session log kept as an object, as the product's agent and executor do.
                if work.assignment.handoff.is_some() {
                    let status = match self.review_report.lock().ok().and_then(|r| r.clone()) {
                        Some(review) if work.role == "reviewer" => review,
                        _ => self.report.lock().map(|s| s.clone()).unwrap_or_default(),
                    };
                    final_text = Some(format!(
                        "done\n```hoh-report\n{}\n```\n",
                        json!({"status": status, "scope": ["AC-035-03"], "findings": []})
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
                detail: if failed { "TransientExternal" } else { "ended" }.to_owned(),
                group_gone,
                class: transient,
                observed: hoh_core::persistence::Observed {
                    messages,
                    raw_log_path: Some(raw_log_path),
                    final_text,
                    artifacts,
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
        team.release.store(true, Ordering::SeqCst);
        R::Ok((turns, handoffs, verifiers))
    }
    .await;
    host.shutdown().await;
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
        team.release.store(true, Ordering::SeqCst);
        R::Ok(())
    }
    .await;
    host.shutdown().await;
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
        team.release.store(true, Ordering::SeqCst);
        R::Ok(())
    }
    .await;
    host.shutdown().await;
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
        team.release.store(true, Ordering::SeqCst);
        R::Ok(())
    }
    .await;
    host.shutdown().await;
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
        team.release.store(true, Ordering::SeqCst);
        second.shutdown().await;
    }
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
    team.release.store(true, Ordering::SeqCst);
    let _ = std::fs::write(out.join("end-review"), "");
    host.shutdown().await;
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
    team.release.store(true, Ordering::SeqCst);
    let _ = std::fs::write(out.join("end-review"), "");
    host.shutdown().await;
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
    let _ = std::fs::write(out.join("qa-go"), "");
    let _ = std::fs::write(out.join("end-review-2"), "");
    host.shutdown().await;
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
    outcome
}

/// POST `body` to the host's commands endpoint; the status.
async fn command(host: &Host, body: &Value) -> R<u16> {
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
    let (head, _) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    Ok(head.split(' ').nth(1).ok_or("no status")?.parse()?)
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
        let revision = rows(&project, "SELECT revision FROM runs WHERE run_id = 'R'")?[0]["revision"].clone();
        let status = command(
            &host,
            &json!({"schema_version": "1.0", "command_id": "c-cancel", "idempotency_key": "cancel",
                "expected_revision": revision, "kind": "CancelRun", "run_id": "R", "payload": {"reason": "cancel"}}),
        )
        .await?;
        assert_eq!(status, 202);
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
    team.release.store(true, Ordering::SeqCst);
    host.shutdown().await;
    outcome
}
