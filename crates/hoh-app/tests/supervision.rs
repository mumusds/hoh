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
            ran: None,
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

// The Commander on the product's path (docs/adr/ADR-030.md decision 11, its test plan's "Commander", M2T15D-05): the
// product's host over fake agent CLIs. Staffing treats the commander binding as any role's (fallback), a supervision
// turn's permanent failure reaches staffing through the binding it blocks, and an owed wake waits, uncounted, while the
// Commander's agent is not verified.

const PROTOCOL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/protocol");
const PI_RPC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hoh-agents/tests/fakes/pi_rpc.py"
);

/// A project whose agents are fake CLIs of its own, found only on its own PATH (never an agent installed on this
/// machine), under a temporary root.
struct Fakes {
    dir: tempfile::TempDir,
}

impl Fakes {
    fn new() -> R<Fakes> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("project"))?;
        std::fs::create_dir_all(dir.path().join("bin"))?;
        let tools = dir.path().join("tools");
        std::fs::create_dir_all(&tools)?;
        for tool in ["cat", "python3", "timeout"] {
            let found = ["/usr/bin", "/bin"]
                .iter()
                .map(|d| Path::new(d).join(tool))
                .find(|p| p.exists())
                .ok_or(format!("no {tool}"))?;
            std::os::unix::fs::symlink(found, tools.join(tool))?;
        }
        Ok(Fakes { dir })
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    /// The registry's PATH: the fakes, then the tools they use.
    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.bin().display(),
            self.dir.path().join("tools").display()
        )
    }

    /// The executable `name` running `body` (bash), written beside it and renamed over it: the registry sees another
    /// file, never a half-written one.
    fn script(&self, name: &str, body: &str) -> R {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(self.bin().join(format!("{name}.d")))?;
        let staged = self.bin().join(format!(".{name}.new"));
        std::fs::write(&staged, format!("#!/bin/bash\n{body}\n"))?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&staged, self.bin().join(name))?;
        Ok(())
    }

    /// A fake `claude` 2.1.318 (`note` a comment line of its own: another note, another implementation): the probe's
    /// answers from the recording; a turn reads its prompt and replays the recorded `success` turn.
    fn claude(&self, note: &str) -> R {
        let d = self.bin().join("claude.d");
        self.script(
            "claude",
            &format!(
                "# {note}\nf='{PROTOCOL}/claude/2.1.318'; d='{d}'\n\
                 case \"$1\" in --version) echo '2.1.318 (Claude Code)'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
                 echo $$ > \"$d/pid.$$\"; cat > /dev/null\ncat \"$f/success/stdout.jsonl\"\n",
                d = d.display()
            ),
        )
    }

    /// A fake `pi` 1.5.0 whose model is not there: the probe's answers from the recording; a turn's get_state is
    /// answered as recorded and its prompt refused `Model not found: …` (pi.rs's MODEL_UNAVAILABLE).
    fn pi_without_its_model(&self) -> R {
        let d = self.bin().join("pi.d");
        std::fs::create_dir_all(&d)?;
        let state =
            std::fs::read_to_string(format!("{PROTOCOL}/pi/1.5.0/rpc-prompt/stdout.jsonl"))?
                .lines()
                .next()
                .ok_or("no get_state answer")?
                .to_owned();
        let refused = json!({"id": "p1", "type": "response", "command": "prompt", "success": false,
            "error": "Model not found: zai-coding-cn/glm-5.3"});
        std::fs::write(d.join("frames.jsonl"), format!("{state}\n{refused}\n"))?;
        std::fs::write(
            d.join("commands.json"),
            json!({"s1": "get_state", "p1": "prompt"}).to_string(),
        )?;
        self.script(
            "pi",
            &format!(
                "f='{PROTOCOL}/pi/1.5.0'; d='{d}'\n\
                 case \"$1\" in --version) echo '1.5.0'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
                 echo $$ > \"$d/pid.$$\"\n\
                 exec timeout 60 python3 '{PI_RPC}' \"$d/frames.jsonl\" \"$d/commands.json\" \"$d/log.$$\" -- \"$@\"\n",
                d = d.display()
            ),
        )
    }

    /// The project's own `.hoh/config.toml` (its team revision once a host starts).
    fn team(&self, toml: &str) -> R {
        std::fs::create_dir_all(self.project().join(".hoh"))?;
        std::fs::write(self.project().join(".hoh/config.toml"), toml)?;
        Ok(())
    }

    /// `agent` registered by the product's compat suite (before a host starts), which must pass.
    fn register(&self, agent: hoh_agents::compat::Agent) -> R {
        let registered =
            hoh_app::compat::register(&self.project(), agent, std::ffi::OsStr::new(&self.path()))?;
        assert_eq!(
            format!("{:?}", registered.entry.suite.status),
            "Pass",
            "{registered:?}"
        );
        Ok(())
    }

    /// The product's host of the project, its agents found only among the fakes, pi's settings in a directory of the
    /// world's own and Commander sessions' directories in the world's.
    fn config(&self) -> HostConfig {
        let registry = hoh_app::host::agents::Registry::with_pi_dir(
            self.path().into(),
            Some(self.dir.path().join("pi-agent")),
        );
        let mut config = HostConfig::product_with(
            &self.project(),
            hoh_core::config::Config::default(),
            env!("CARGO_BIN_EXE_hoh").into(),
            Arc::new(registry),
        );
        config.port = 0;
        config.store.wal_probe = Some(probe());
        if let Some(runner) = config.runner.as_mut() {
            runner.context.commander_dir = Some(self.dir.path().join("state/hoh/commander"));
        }
        config
    }

    /// Every fake turn process gone: one still there is killed only while its command line names this world's fakes.
    fn stop_fakes(&self) {
        let bin = self.bin().display().to_string();
        for name in ["claude", "pi"] {
            let Ok(entries) = std::fs::read_dir(self.bin().join(format!("{name}.d"))) else {
                continue;
            };
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if let Some(pid) = file.strip_prefix("pid.")
                    && std::fs::read(format!("/proc/{pid}/cmdline"))
                        .is_ok_and(|c| String::from_utf8_lossy(&c).contains(&bin))
                {
                    let _ = Command::new("kill").args(["-KILL", pid]).status();
                }
            }
        }
    }
}

/// Holds each compat suite after its run, before its publication, until `<dir>/release` exists (or `dir` is gone).
#[derive(Debug)]
struct HoldSuite(PathBuf);

impl hoh_app::host::HostHooks for HoldSuite {
    fn suite_ran(&self, _agent: &str) {
        let _ = std::fs::write(self.0.join("held"), "");
        let end = Instant::now() + Duration::from_secs(120);
        while self.0.exists() && !self.0.join("release").exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

async fn start_when_free(config: HostConfig) -> Result<Host, hoh_app::host::StartError> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        match Host::start(config.clone()).await {
            Err(e) if e.exit_code == 69 && Instant::now() < end => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

/// A run started over the host's HTTP API: its id.
async fn started(host: &Host) -> R<String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let body = json!({
        "schema_version": "1.0", "command_id": "c-start", "idempotency_key": "start",
        "expected_revision": 0, "kind": "StartRun", "run_id": null,
        "payload": {"goal": "Ship the parser", "mode": "AUTO", "interactive": false},
    })
    .to_string();
    let request = format!(
        "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-HoH-Token: {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {length}\r\n\r\n{body}",
        port = host.port,
        token = host.token,
        length = body.len(),
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8(raw)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    assert!(head.starts_with("HTTP/1.1 202"), "{text}");
    let accepted: Value = serde_json::from_str(body)?;
    Ok(accepted["run_id"].as_str().ok_or("no run_id")?.to_owned())
}

/// Wait until `sql`'s rows satisfy `done`, for at most `limit`; the rows.
async fn until(
    project: &Path,
    sql: &str,
    limit: Duration,
    done: &dyn Fn(&Value) -> bool,
) -> R<Value> {
    let end = Instant::now() + limit;
    loop {
        let found = rows(&db(project), sql)?;
        if done(&found) {
            return Ok(found);
        }
        if Instant::now() > end {
            return Err(format!("not reached: {sql}: {found}").into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn some(rows: &Value) -> bool {
    rows.as_array().is_some_and(|a| !a.is_empty())
}

/// Run `run_id` RUNNING on a spec (a run needs one to be supervised, docs/adr/ADR-025.md decision 4), once its
/// commander binding has a session, and a need_decision of fingerprint `fp` about it.
async fn supervised(project: &Path, run_id: &str, fp: &str) -> R {
    until(
        project,
        &format!(
            "SELECT 1 FROM role_bindings WHERE run_id = '{run_id}' AND role = 'commander' AND session_id IS NOT NULL"
        ),
        Duration::from_secs(60),
        &some,
    )
    .await?;
    seed(
        project,
        &format!(
            "INSERT OR IGNORE INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
             UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = '{run_id}';"
        ),
    )?;
    need_decision(project, run_id, fp)
}

/// A need_decision of fingerprint `fp` in run `run_id`.
fn need_decision(project: &Path, run_id: &str, fp: &str) -> R {
    seed(
        project,
        &format!(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES \
               (:p, 'nd-{fp}', '{run_id}', 'need_decision', '{{\"fingerprint\": \"{fp}\"}}', '2026-11-14T00:00:00Z');"
        ),
    )
}

/// The query of run `run_id`'s supervision turns: wake, attempt, agent, epoch and state, in allocation order.
fn turns(run_id: &str) -> String {
    format!(
        "SELECT json_extract(e.payload_json, '$.wake') AS wake, a.attempt_id, a.agent, a.role_epoch, a.state, \
         a.failure_reason FROM events e JOIN attempts a ON a.attempt_id = json_extract(e.payload_json, '$.attempt_id') \
         WHERE e.run_id = '{run_id}' AND e.kind = 'supervisor.allocated' ORDER BY e.seq"
    )
}

/// How many of run `run_id`'s blocker claims are COMMANDER_UNAVAILABLE.
fn commander_claims(project: &Path, run_id: &str) -> R<Value> {
    Ok(rows(
        &db(project),
        &format!(
            "SELECT count(*) AS n FROM events WHERE run_id = '{run_id}' AND kind = 'run.blocker_claimed' \
             AND json_extract(payload_json, '$.reason') = 'COMMANDER_UNAVAILABLE'"
        ),
    )?[0]["n"]
        .clone())
}

/// docs/adr/ADR-030.md decision 11 (its test plan's "Commander"): a world with only claude installed, the commander
/// configured codex falling back to claude: staffing switches the commander to claude at epoch 2 (the other roles'
/// blocks wake it), its turns answer, and the run is never claimed COMMANDER_UNAVAILABLE.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commander_not_installed_falls_back_and_its_run_is_not_blocked_for_it() -> R {
    let fakes = Fakes::new()?;
    fakes.claude("A")?;
    fakes.team(
        "config_version = \"1.0\"\n[team.commander]\nagent = \"codex\"\nfallback = [\"claude\"]\n",
    )?;
    fakes.register(hoh_agents::compat::Agent::Claude)?;
    let host = start_when_free(fakes.config()).await?;
    let project = fakes.project();
    let outcome = async {
        let run_id = started(&host).await?;
        supervised(&project, &run_id, "fp-1").await?;
        let commander = rows(
            &db(&project),
            &format!(
                "SELECT agent, role_epoch, state FROM role_bindings WHERE run_id = '{run_id}' AND role = 'commander'"
            ),
        )?;
        assert_eq!(
            commander,
            json!([{"agent": "claude", "role_epoch": 2, "state": "ACTIVE"}])
        );
        let fallback = rows(
            &db(&project),
            &format!(
                "SELECT json_extract(payload_json, '$.from') AS \"from\", json_extract(payload_json, '$.chosen') AS chosen, \
                 json_extract(payload_json, '$.evaluated') AS evaluated FROM events WHERE run_id = '{run_id}' \
                 AND kind = 'team.binding_fallback' AND json_extract(payload_json, '$.role') = 'commander'"
            ),
        )?;
        assert_eq!(
            fallback,
            json!([{"from": "codex", "chosen": "claude", "evaluated": "[\"codex:not_installed\",\"claude:usable\"]"}])
        );
        let answered = until(
            &project,
            &turns(&run_id),
            Duration::from_secs(60),
            &|r: &Value| {
                r.as_array()
                    .is_some_and(|a| a.iter().any(|t| t["state"] == "SETTLED"))
            },
        )
        .await?;
        assert!(
            answered
                .as_array()
                .is_some_and(|a| a.iter().all(|t| t["agent"] == "claude" && t["role_epoch"] == 2)),
            "{answered}"
        );
        assert_eq!(commander_claims(&project, &run_id)?, json!(0));
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    fakes.stop_fakes();
    outcome
}

/// docs/adr/ADR-030.md decision 11 with docs/adr/ADR-025.md decision 5 (its implementation note on the worlds with only
/// claude installed): with no fallback, the commander bound to codex, not installed, is BLOCKED, and the run, owing
/// it the decisions the other roles' blocks wake, is BLOCKED(COMMANDER_UNAVAILABLE).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_fallback_a_commander_not_installed_blocks_its_run() -> R {
    let fakes = Fakes::new()?;
    fakes.claude("A")?;
    fakes.register(hoh_agents::compat::Agent::Claude)?;
    let host = start_when_free(fakes.config()).await?;
    let project = fakes.project();
    let outcome = async {
        let run_id = started(&host).await?;
        until(
            &project,
            &format!(
                "SELECT 1 FROM role_bindings WHERE run_id = '{run_id}' AND role = 'commander' AND state = 'BLOCKED' \
                 AND blocked_reason = 'AGENT_UNAVAILABLE'"
            ),
            Duration::from_secs(60),
            &some,
        )
        .await?;
        seed(
            &project,
            &format!(
                "INSERT OR IGNORE INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
                 UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = '{run_id}';"
            ),
        )?;
        let run = until(
            &project,
            &format!("SELECT state, blocked_reasons_json AS reasons FROM runs WHERE run_id = '{run_id}'"),
            Duration::from_secs(60),
            &|r: &Value| r[0]["state"] == "BLOCKED",
        )
        .await?;
        assert!(
            run[0]["reasons"]
                .as_str()
                .is_some_and(|r| r.contains("COMMANDER_UNAVAILABLE")),
            "{run}"
        );
        assert_eq!(
            rows(
                &db(&project),
                &format!(
                    "SELECT agent, role_epoch FROM role_bindings WHERE run_id = '{run_id}' AND role = 'commander'"
                ),
            )?,
            json!([{"agent": "codex", "role_epoch": 1}])
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    fakes.stop_fakes();
    outcome
}

/// docs/adr/ADR-030.md decision 11 (M2T15D-05, its test plan's "Commander"): the commander is a pi whose prompt is
/// refused `Model not found` (MODEL_UNAVAILABLE), falling back to claude. Its first supervision turn fails; the commander
/// binding is BLOCKED(MODEL_UNAVAILABLE, cause turn) with pi's exclusion recorded, and staffing switches it to claude at
/// epoch 2, whose turn answers the wake: the Commander did not back off on pi up to its cap (no failures_capped).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commanders_unavailable_model_falls_back() -> R {
    let fakes = Fakes::new()?;
    fakes.claude("A")?;
    fakes.pi_without_its_model()?;
    fakes.team(
        "config_version = \"1.0\"\n[team.commander]\nagent = \"pi\"\nfallback = [\"claude\"]\n\
         [team.reviewer]\nagent = \"claude\"\n",
    )?;
    fakes.register(hoh_agents::compat::Agent::Claude)?;
    fakes.register(hoh_agents::compat::Agent::Pi)?;
    let host = start_when_free(fakes.config()).await?;
    let project = fakes.project();
    let outcome = async {
        let run_id = started(&host).await?;
        supervised(&project, &run_id, "fp-1").await?;
        // The wake of the need_decision is the first one given a turn; it is answered once a turn of it settles.
        let answered = until(
            &project,
            &turns(&run_id),
            Duration::from_secs(60),
            &|r: &Value| {
                r.as_array().is_some_and(|a| {
                    a.iter()
                        .any(|t| t["wake"] == a[0]["wake"] && t["state"] == "SETTLED")
                })
            },
        )
        .await?;
        let turns = answered.as_array().ok_or("no turns")?;
        let first = &turns[0];
        assert_eq!(
            (&first["agent"], &first["role_epoch"], &first["state"], &first["failure_reason"]),
            (&json!("pi"), &json!(1), &json!("FAILED"), &json!("MODEL_UNAVAILABLE")),
            "{answered}"
        );
        let of_wake: Vec<(&Value, &Value, &Value)> = turns
            .iter()
            .filter(|t| t["wake"] == first["wake"])
            .map(|t| (&t["agent"], &t["role_epoch"], &t["state"]))
            .collect();
        assert_eq!(
            of_wake,
            [
                (&json!("pi"), &json!(1), &json!("FAILED")),
                (&json!("claude"), &json!(2), &json!("SETTLED"))
            ],
            "one failure on pi, then the wake answered by claude: {answered}"
        );
        // The later wakes (the commander's own block wakes it) are claude's at epoch 2 too.
        assert!(
            turns[1..]
                .iter()
                .all(|t| t["agent"] == "claude" && t["role_epoch"] == 2),
            "{answered}"
        );
        let failed = first["attempt_id"].as_str().ok_or("no attempt")?;
        let blocked = rows(
            &db(&project),
            &format!(
                "SELECT json_extract(payload_json, '$.role_epoch') AS role_epoch, json_extract(payload_json, '$.agent') AS agent, \
                 json_extract(payload_json, '$.reason') AS reason, json_extract(payload_json, '$.cause') AS cause, \
                 json_extract(payload_json, '$.attempt_id') AS attempt_id FROM events WHERE run_id = '{run_id}' \
                 AND kind = 'team.binding_blocked' AND json_extract(payload_json, '$.role') = 'commander'"
            ),
        )?;
        assert_eq!(
            blocked,
            json!([{"role_epoch": 1, "agent": "pi", "reason": "MODEL_UNAVAILABLE", "cause": "turn", "attempt_id": failed}])
        );
        let excluded = rows(
            &db(&project),
            &format!(
                "SELECT json_extract(payload_json, '$.role') AS role, json_extract(payload_json, '$.agent') AS agent, \
                 json_extract(payload_json, '$.reason') AS reason, json_extract(payload_json, '$.attempt_id') AS attempt_id \
                 FROM events WHERE run_id = '{run_id}' AND kind = 'team.binding_excluded'"
            ),
        )?;
        assert_eq!(
            excluded,
            json!([{"role": "commander", "agent": "pi", "reason": "MODEL_UNAVAILABLE", "attempt_id": failed}])
        );
        let fallback = rows(
            &db(&project),
            &format!(
                "SELECT json_extract(payload_json, '$.from') AS \"from\", json_extract(payload_json, '$.chosen') AS chosen \
                 FROM events WHERE run_id = '{run_id}' AND kind = 'team.binding_fallback'"
            ),
        )?;
        assert_eq!(fallback, json!([{"from": "pi", "chosen": "claude"}]));
        let capped = rows(
            &db(&project),
            &format!(
                "SELECT count(*) AS n FROM events WHERE run_id = '{run_id}' AND kind = 'supervisor.unavailable' \
                 AND payload_json LIKE '%failures_capped%'"
            ),
        )?;
        assert_eq!(capped, json!([{"n": 0}]));
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    fakes.stop_fakes();
    outcome
}

/// docs/adr/ADR-030.md decision 11 (-53, its test plan's "Commander"): the Commander's agent (claude) changes after its
/// session was granted, and the new implementation's suite is held: staffing keeps the binding (compat pending), and the
/// allocation writes supervisor.deferred{COMPAT_UNVERIFIED} for the owed wake instead of a turn, its failures not
/// growing; once the suite published PASS the Commander is bound again and the wake answered, never failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commanders_wake_waits_uncounted_while_its_agent_awaits_verification() -> R {
    let fakes = Fakes::new()?;
    fakes.claude("A")?;
    fakes.team(
        "config_version = \"1.0\"\n[team.commander]\nagent = \"claude\"\n[team.qa]\nagent = \"claude\"\n\
         [team.researcher]\nagent = \"claude\"\n[team.reviewer]\nagent = \"claude\"\n",
    )?;
    fakes.register(hoh_agents::compat::Agent::Claude)?;
    let hold = fakes.dir.path().join("hold");
    std::fs::create_dir_all(&hold)?;
    let mut config = fakes.config();
    config.hooks = Arc::new(HoldSuite(hold.clone()));
    let host = start_when_free(config).await?;
    let project = fakes.project();
    let outcome = async {
        let run_id = started(&host).await?;
        supervised(&project, &run_id, "fp-1").await?;
        let settled = |r: &Value| {
            r.as_array()
                .is_some_and(|a| a.iter().filter(|t| t["state"] == "SETTLED").count() >= 1)
        };
        until(&project, &turns(&run_id), Duration::from_secs(60), &settled).await?;
        // Another implementation of claude: not verified until its suite, held here, publishes.
        fakes.claude("B")?;
        let limit = Duration::from_secs(60);
        let end = Instant::now() + limit;
        while !hold.join("held").exists() {
            if Instant::now() > end {
                return Err("claude's suite never ran".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let before = rows(&db(&project), &turns(&run_id))?;
        need_decision(&project, &run_id, "fp-2")?;
        let deferred = until(
            &project,
            &format!(
                "SELECT json_extract(payload_json, '$.wakes') AS wakes FROM events WHERE run_id = '{run_id}' \
                 AND kind = 'supervisor.deferred' AND json_extract(payload_json, '$.reason') = 'COMPAT_UNVERIFIED'"
            ),
            limit,
            &some,
        )
        .await?;
        // Ticks go on meanwhile (one each 200 ms): no turn, no other deferral, no failure.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(rows(&db(&project), &turns(&run_id))?, before, "no turn while unverified");
        let again = rows(
            &db(&project),
            &format!(
                "SELECT count(*) AS n FROM events WHERE run_id = '{run_id}' AND kind = 'supervisor.deferred' \
                 AND json_extract(payload_json, '$.reason') = 'COMPAT_UNVERIFIED'"
            ),
        )?;
        assert_eq!(again, json!([{"n": 1}]), "{deferred}");
        let wake: Vec<Value> = serde_json::from_str(deferred[0]["wakes"].as_str().unwrap_or("[]"))?;
        let [wake] = wake.as_slice() else {
            return Err(format!("one owed wake: {deferred}").into());
        };
        std::fs::write(hold.join("release"), "")?;
        let answered = until(
            &project,
            &turns(&run_id),
            limit,
            &|r: &Value| {
                r.as_array().is_some_and(|a| {
                    a.iter().any(|t| t["wake"] == *wake && t["state"] == "SETTLED")
                })
            },
        )
        .await?;
        let of_wake: Vec<&Value> = answered
            .as_array()
            .map(|a| a.iter().filter(|t| t["wake"] == *wake).collect())
            .unwrap_or_default();
        assert_eq!(of_wake.len(), 1, "answered at its first turn: {answered}");
        assert_eq!(
            (&of_wake[0]["agent"], &of_wake[0]["role_epoch"]),
            (&json!("claude"), &json!(2)),
            "bound again to the verified implementation: {answered}"
        );
        let failures = rows(
            &db(&project),
            &format!(
                "SELECT count(*) AS n FROM events WHERE run_id = '{run_id}' AND kind = 'supervisor.rearmed' \
                 AND json_extract(payload_json, '$.reason') = 'failure'"
            ),
        )?;
        assert_eq!(failures, json!([{"n": 0}]));
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(hold.join("release"), "");
    host.shutdown().await;
    fakes.stop_fakes();
    outcome
}
