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

/// The project's specification as committed (its sha256 is the run's spec digest).
const SPEC: &str =
    "{\"requirements\": [{\"id\": \"REQ-035\", \"acceptance\": [{\"id\": \"AC-035-03\"}]}]}\n";

/// A committed project whose layout the freeze takes (docs/adr/ADR-028.md decision 4), its store made by one host's
/// start and stop; the project and the specification's digest.
async fn project(dir: &Path) -> R<(PathBuf, String)> {
    let project = dir.join("p");
    std::fs::create_dir_all(&project)?;
    git(&project, &["init", "-q"])?;
    for (path, text) in [
        ("src/lib.rs", "pub fn a() {}\n"),
        ("contracts/requirements.json", SPEC),
        ("tests/check-plan.json", "{\"checks\": {}}\n"),
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
    spec: String,
    /// The Commander's turns started (refused ones included).
    commander_turns: Arc<AtomicU32>,
    /// The Developer turns still to fail (transiently) before one completes.
    fail: AtomicU32,
    /// Lets the held FakeAgents (verifiers and other readers) end.
    release: Arc<AtomicBool>,
    /// The qa FakeAgent writes a file into its working directory before it ends.
    tamper: AtomicBool,
    /// Each verifier work's role and run directory, as given to its start.
    run_dirs: Arc<std::sync::Mutex<Vec<RunDir>>>,
    /// Lets the Developer's FakeAgent end (set: it is not held).
    developer: Arc<AtomicBool>,
}

impl Team {
    fn new(commander: Commander, spec: &str) -> Team {
        Team {
            refuse: AtomicBool::new(commander.fails()),
            spec: spec.to_owned(),
            commander_turns: Arc::default(),
            fail: AtomicU32::new(0),
            release: Arc::default(),
            tamper: AtomicBool::new(false),
            run_dirs: Arc::default(),
            developer: Arc::new(AtomicBool::new(true)),
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
        let transient = hoh_core::retry::FailureClass::Transient { retry_after: None };
        let mut messages = Vec::new();
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
                held = Some(self.release.clone());
                if let Ok(mut dirs) = self.run_dirs.lock() {
                    dirs.push((role.to_owned(), work.run_dir.clone()));
                }
            }
        }
        let script = if work.role == "qa" && self.tamper.load(Ordering::SeqCst) {
            "read -r go; echo changed > tampered.txt; exit 0"
        } else {
            "read -r go; exit 0"
        };
        let mut command = Command::new("sh");
        if let Some(dir) = &work.run_dir {
            command.current_dir(dir);
        }
        let mut child = command
            .args(["-c", script])
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
    let team = Arc::new(Team::new(Commander::Answers, &spec));
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
    let team = Arc::new(Team::new(Commander::FailsAtStart, &spec));
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
    let team = Arc::new(Team::new(Commander::Answers, &spec));
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
    let team = Arc::new(Team::new(Commander::Answers, &spec));
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
    let team = Arc::new(Team::new(Commander::Answers, &spec));
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
    let team = Arc::new(Team::new(Commander::Answers, &spec));
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
