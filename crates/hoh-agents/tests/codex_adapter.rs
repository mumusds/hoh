//! M1-T04 Codex adapter against fakes that replay recorded codex 0.166.0 traffic (TEST-024-01..03): `codex
//! app-server` by tests/fakes/app_server.py, `codex exec --json` by a bash replay. The adapter's results are checked
//! in the store it wrote: attempts, sessions, the input queue and events.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use common::{R, World};
use hoh_agents::adapter::AttemptRef;
use hoh_agents::codex::{self, Launch, Mode, Outcome, Steer, SteerReceipt, Turn};
use hoh_agents::process::{Escalation, group_members};
use hoh_core::lifecycle::{AttemptState, InputState};
use hoh_core::persistence::{CancelLatch, CancelReceipt, NewInput};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/codex/0.166.0"
);
const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/app_server.py");

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read(path: impl AsRef<Path>) -> R<String> {
    Ok(std::fs::read_to_string(path)?)
}

/// A recorded app-server case: the server frames and the method of each recorded request id.
struct Case {
    frames: Vec<Value>,
    requests: BTreeMap<i64, String>,
}

impl Case {
    fn load(name: &str) -> R<Case> {
        let frames = read(format!("{FIXTURES}/{name}/stdout.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Value>, _>>()?;
        let mut requests = BTreeMap::new();
        for line in read(format!("{FIXTURES}/{name}/stdin.txt"))?.lines() {
            let Ok(m) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let (Some(id), Some(method)) = (m["id"].as_i64(), m["method"].as_str()) {
                requests.insert(id, method.to_owned());
            }
        }
        Ok(Case { frames, requests })
    }

    fn position(&self, method: &str) -> R<usize> {
        Ok(self
            .frames
            .iter()
            .position(|f| f["method"] == method)
            .ok_or(format!("no {method}"))?)
    }

    fn thread_id(&self) -> R<String> {
        Ok(self
            .frames
            .iter()
            .find_map(|f| f["result"]["thread"]["id"].as_str())
            .ok_or("no thread")?
            .to_owned())
    }
}

/// The help texts and the known notifications of codex 0.166.0, from the recording.
fn launch(program: &Path, cwd: &Path) -> R<Launch> {
    let schema: Value = serde_json::from_str(&read(format!(
        "{FIXTURES}/app-server-schema/ServerNotification.json"
    ))?)?;
    Ok(Launch {
        program: program.to_path_buf(),
        app_server_help: Some(read(format!("{FIXTURES}/help-app-server.txt"))?),
        exec_help: read(format!("{FIXTURES}/help-exec.txt"))?,
        exec_resume_help: None,
        cwd: cwd.to_path_buf(),
        yolo: true,
        resume: None,
        model: None,
        known: codex::known_notifications(&schema),
    })
}

/// Fakes, turns and runs for the tests.
trait Fakes {
    /// A fake `codex`: `app-server` replays `case` (or answers nothing if `None`), `exec` replays the recorded exec
    /// case `exec`; every invocation records its argv.
    fn fake(&self, name: &str, case: Option<&Case>, exec: &str, exit_when_done: bool)
    -> R<PathBuf>;
    fn turn(&self, program: &Path, logs: &str) -> R<Turn>;
    async fn attempt(&self, id: &str) -> R<AttemptRef>;
}

impl Fakes for World {
    fn fake(
        &self,
        name: &str,
        case: Option<&Case>,
        exec: &str,
        exit_when_done: bool,
    ) -> R<PathBuf> {
        let d = self.dir.path().join(name);
        std::fs::create_dir_all(&d)?;
        let frames = case.map_or(String::new(), |c| {
            c.frames.iter().map(|f| format!("{f}\n")).collect()
        });
        std::fs::write(d.join("frames.jsonl"), frames)?;
        let requests: BTreeMap<String, &String> = case
            .map(|c| c.requests.iter().map(|(k, v)| (k.to_string(), v)).collect())
            .unwrap_or_default();
        std::fs::write(d.join("requests.json"), serde_json::to_vec(&requests)?)?;
        let lines: Vec<String> = read(format!("{FIXTURES}/{exec}/stdout.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        for (i, line) in lines.iter().enumerate() {
            std::fs::write(d.join(format!("exec.{i:04}")), format!("{line}\n"))?;
        }
        let exit: i64 = serde_json::from_str::<Value>(
            &read(format!("{FIXTURES}/{exec}/meta.json")).unwrap_or_default(),
        )
        .ok()
        .and_then(|m| m["exit_code"].as_i64())
        .unwrap_or(0);
        let script = d.join("codex");
        std::fs::write(
            &script,
            format!(
                "#!/bin/bash\nd='{d}'\nprintf '%s\\n' \"$@\" > \"$d/argv.$$\"\n\
                 if [ \"$1\" = app-server ]; then exec python3 '{FAKE}' \"$d/frames.jsonl\" \"$d/requests.json\" \"$d/log\" {opt}; fi\n\
                 cat > \"$d/stdin.$$\"\nfor f in \"$d\"/exec.*; do [ -e \"$f\" ] && cat \"$f\"; done\nexit {exit}\n",
                d = d.display(),
                opt = if exit_when_done {
                    "--exit-when-done"
                } else {
                    ""
                },
            ),
        )?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        Ok(script)
    }

    fn turn(&self, program: &Path, logs: &str) -> R<Turn> {
        Ok(Turn {
            launch: launch(program, self.dir.path())?,
            prompt: "Reply with exactly HOH_SAMPLE_OK and nothing else.".to_owned(),
            logs: self.dir.path().join(logs),
            escalation: Escalation {
                native_grace: Duration::from_secs(2),
                term_grace: Duration::from_secs(1),
                ..Turn::default_escalation()
            },
            handshake_timeout: Duration::from_secs(10),
            deadline: Some(Duration::from_secs(60)),
        })
    }

    async fn attempt(&self, id: &str) -> R<AttemptRef> {
        self.attempt_in("codex", id, "S1", "0.166.0").await
    }
}

/// The argv of every invocation of `fake`, in no particular order.
fn argvs(fake: &Path) -> R<Vec<Vec<String>>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(fake.parent().ok_or("no dir")?)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("argv."))
        {
            out.push(read(path)?.lines().map(str::to_owned).collect());
        }
    }
    Ok(out)
}

/// What the fake app-server received, as JSON values.
fn received(fake: &Path) -> R<Vec<Value>> {
    let path = fake.parent().ok_or("no dir")?.join("log/received.jsonl");
    Ok(read(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

async fn run(world: &World, attempt: &AttemptRef, turn: &Turn) -> R<Outcome> {
    let (_steer, steers) = mpsc::channel(4);
    Ok(codex::run(&world.store, attempt, turn, &CancelLatch::new(), steers).await?)
}

fn events(world: &World, kind: &str) -> R<Vec<Value>> {
    Ok(world
        .store
        .reader()?
        .events("R")?
        .into_iter()
        .filter(|(k, _)| k == kind)
        .map(|(_, p)| p)
        .collect())
}

/// A count from the store, read with Python's sqlite3 (test code).
fn count(world: &World, sql: &str) -> R<i64> {
    let out = std::process::Command::new("python3")
        .args(["-c", "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute(sys.argv[2]).fetchone()[0])"])
        .arg(world.dir.path().join(".hoh/state.db"))
        .arg(sql)
        .output()?;
    Ok(String::from_utf8(out.stdout)?.trim().parse()?)
}

/// Wait until the adapter recorded the active turn of `attempt_id`; `(threadId, turnId)`.
async fn active_turn(world: &World, attempt_id: &str) -> R<(String, String)> {
    for _ in 0..400 {
        if let Some(t) = events(world, "adapter.turn")?
            .iter()
            .find(|t| t["attempt_id"] == attempt_id)
        {
            return Ok((
                t["threadId"].as_str().ok_or("no thread")?.to_owned(),
                t["turnId"].as_str().ok_or("no turn")?.to_owned(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err("no active turn".into())
}

#[tokio::test]
async fn test_024_01_a_recorded_turn_settles_in_handshake_order_with_one_unknown_event() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    let unknown = json!({"method": "hoh/unrecorded", "params": {"note": "not in the 0.166.0 schema", "n": [1, 2]}});
    let at = case.position("turn/completed")?;
    case.frames.insert(at, unknown.clone());
    let fake = world.fake("turn", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(
        (outcome.state, outcome.mode),
        (AttemptState::Settled, Mode::AppServer)
    );
    // The client spoke in the handshake order, and `initialized` only after the initialize response.
    let methods: Vec<String> = received(&fake)?
        .iter()
        .filter_map(|m| m["method"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        methods[..4],
        ["initialize", "initialized", "thread/start", "turn/start"]
    );
    let trace: Vec<Value> = read(fake.parent().ok_or("no dir")?.join("log/trace.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let first_out = trace
        .iter()
        .position(|t| t["dir"] == "out")
        .ok_or("nothing sent")?;
    let initialized = trace
        .iter()
        .position(|t| t["dir"] == "in" && t["method"] == "initialized")
        .ok_or("no initialized")?;
    assert!(
        trace[first_out]["response"] == true && first_out < initialized,
        "{trace:?}"
    );
    // The store: the attempt, the session's thread and the resolved model of the thread/start response.
    let reader = world.store.reader()?;
    let row = reader.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.resolved_model.as_deref()),
        (AttemptState::Settled, Some("gpt-6-astra"))
    );
    let session = reader.session("S1")?.ok_or("no session")?;
    assert_eq!(session.native_id, Some(case.thread_id()?));
    assert_eq!(session.capabilities["mid_turn_steer"], true);
    // One item event per item/started and item/completed, each with its thread, turn and item.
    let recorded = case
        .frames
        .iter()
        .filter(|f| f["method"] == "item/started" || f["method"] == "item/completed")
        .count();
    let items = events(&world, "adapter.item")?;
    assert_eq!(items.len(), recorded);
    assert!(
        items.iter().all(|i| i["threadId"].is_string()
            && i["turnId"].is_string()
            && i["itemId"].is_string()),
        "{items:?}"
    );
    // The raw log is what the fake sent; the unknown notification is kept as it was, once.
    let sent = std::fs::read(fake.parent().ok_or("no dir")?.join("log/sent.jsonl"))?;
    assert_eq!(outcome.stdout_sha256, hex(&sent));
    assert_eq!(hex(&std::fs::read(&outcome.stdout)?), hex(&sent));
    let unknowns = events(&world, "adapter.unknown_event")?;
    assert_eq!(unknowns.len(), 1);
    assert_eq!(unknowns[0]["frame"], unknown);
    assert!(group_members(outcome.pid.ok_or("no pid")?).is_empty());
    Ok(())
}

#[tokio::test]
async fn test_024_02_interrupt_unauthorized_eof_and_resume() -> R {
    let world = World::new()?;
    // (a) A cancel mid-turn sends turn/interrupt for the active thread and turn; the turn ends interrupted.
    let fake = world.fake(
        "interrupt",
        Some(&Case::load("app-server-interrupt")?),
        "exec",
        false,
    )?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/A1")?;
    let cancel = CancelLatch::new();
    let (_steer, steers) = mpsc::channel(4);
    let running = codex::run(&world.store, &attempt, &turn, &cancel, steers);
    let trigger = async {
        let active = active_turn(&world, "A1").await?;
        assert_eq!(cancel.request(), CancelReceipt::Accepted);
        Ok::<(String, String), Box<dyn std::error::Error>>(active)
    };
    let (a, active) = tokio::join!(running, trigger);
    let (a, (thread, turn_id)) = (a?, active?);
    assert_eq!(a.state, AttemptState::Cancelled);
    let interrupt = received(&fake)?
        .into_iter()
        .find(|m| m["method"] == "turn/interrupt")
        .ok_or("no turn/interrupt")?;
    assert_eq!(
        interrupt["params"],
        json!({"threadId": thread, "turnId": turn_id})
    );
    assert_eq!(a.status.as_deref(), Some("interrupted"));
    assert!(group_members(a.pid.ok_or("no pid")?).is_empty());
    let row = world.store.reader()?.attempt("A1")?.ok_or("no A1")?;
    assert_eq!(
        (row.state, row.cancel_requested),
        (AttemptState::Cancelled, true)
    );
    let session = world.store.reader()?.session("S1")?.ok_or("no S1")?;
    assert_eq!(session.native_id.as_deref(), Some(thread.as_str()));

    // (b) turn/completed failed with codexErrorInfo=unauthorized: synthesized from the recorded turn by the 0.166.0
    // schema (CR-003: the value was never recorded), so check that the schema names the values used.
    let schema: Value = serde_json::from_str(&read(format!(
        "{FIXTURES}/app-server-schema/codex_app_server_protocol.v2.schemas.json"
    ))?)?;
    let defs = &schema["definitions"];
    assert!(
        defs["TurnStatus"]["enum"]
            .as_array()
            .is_some_and(|e| e.contains(&json!("failed")))
    );
    assert!(
        defs["CodexErrorInfo"]["anyOf"][0]["enum"]
            .as_array()
            .is_some_and(|e| e.contains(&json!("unauthorized")))
    );
    let mut case = Case::load("app-server-turn")?;
    let at = case.position("turn/completed")?;
    case.frames[at]["params"]["turn"]["status"] = json!("failed");
    case.frames[at]["params"]["turn"]["error"] = json!({"message": "401 Unauthorized", "codexErrorInfo": "unauthorized", "additionalDetails": null});
    let fake = world.fake("unauthorized", Some(&case), "exec", false)?;
    let attempt = world.attempt_in("codex", "A2", "S2", "0.166.0").await?;
    let b = run(&world, &attempt, &world.turn(&fake, "logs/A2")?).await?;
    assert_eq!(
        (b.state, b.failure.as_deref()),
        (AttemptState::Failed, Some("BLOCKED_AUTH"))
    );
    let failed = events(&world, "adapter.turn_failed")?;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["blocked_reason"], "BLOCKED_AUTH");
    assert_eq!(failed[0]["retry"], "none");
    assert!(group_members(b.pid.ok_or("no pid")?).is_empty());

    // (c) The stream ends mid-turn: never SETTLED.
    let mut case = Case::load("app-server-turn")?;
    let at = case.position("turn/started")?;
    case.frames.truncate(at + 1);
    let fake = world.fake("eof", Some(&case), "exec", true)?;
    let attempt = world.attempt_in("codex", "A3", "S3", "0.166.0").await?;
    let c = run(&world, &attempt, &world.turn(&fake, "logs/A3")?).await?;
    assert!(
        matches!(c.state, AttemptState::Failed | AttemptState::UnknownEffect),
        "{:?} {:?}",
        c.state,
        c.failure
    );
    assert!(group_members(c.pid.ok_or("no pid")?).is_empty());

    // resume: a new process for (a)'s session sends thread/resume with (a)'s thread, not thread/start.
    let mut case = Case::load("app-server-resume")?;
    let recorded = case.thread_id()?;
    for f in &mut case.frames {
        *f = serde_json::from_str(&f.to_string().replace(&recorded, &thread))?;
    }
    let fake = world.fake("resume", Some(&case), "exec", false)?;
    let attempt = world.attempt("A4").await?;
    let mut turn = world.turn(&fake, "logs/A4")?;
    turn.launch.resume = world
        .store
        .reader()?
        .session("S1")?
        .and_then(|s| s.native_id);
    assert_eq!(turn.launch.resume.as_deref(), Some(thread.as_str()));
    let resumed = run(&world, &attempt, &turn).await?;
    let got = received(&fake)?;
    let resume = got
        .iter()
        .find(|m| m["method"] == "thread/resume")
        .ok_or("no thread/resume")?;
    assert_eq!(resume["params"]["threadId"], json!(thread));
    assert!(!got.iter().any(|m| m["method"] == "thread/start"));
    assert_eq!(resumed.state, AttemptState::Settled);
    // No retry was made: one attempt per case, none created by the adapter.
    assert_eq!(count(&world, "SELECT count(*) FROM attempts")?, 4);
    assert_eq!(
        count(
            &world,
            "SELECT count(*) FROM outbox WHERE next_retry_at IS NOT NULL"
        )?,
        0
    );
    Ok(())
}

/// Queue a steer input and send it to the adapter; the receipt arrives on the returned channel.
async fn steer(
    world: &World,
    tx: &mpsc::Sender<Steer>,
    id: &str,
    text: &str,
    expected_turn: Option<&str>,
) -> R<oneshot::Receiver<SteerReceipt>> {
    world
        .store
        .enqueue_input(NewInput {
            input_id: id.to_owned(),
            run_id: "R".to_owned(),
            kind: "STEER".to_owned(),
            target_role: Some("developer".to_owned()),
            payload: json!({"text": text}),
        })
        .await?;
    let (reply, receipt) = oneshot::channel();
    tx.send(Steer {
        input_id: id.to_owned(),
        text: text.to_owned(),
        expected_turn: expected_turn.map(str::to_owned),
        reply,
    })
    .await
    .map_err(|_| "the adapter dropped its steer channel")?;
    Ok(receipt)
}

fn input_state(world: &World, id: &str) -> R<InputState> {
    Ok(world.store.reader()?.input(id)?.ok_or("no input")?.state)
}

#[tokio::test]
async fn test_024_03_degraded_exec_steer_and_missing_flag() -> R {
    let world = World::new()?;
    // ① without an app-server, and with one whose initialize is "method not found": exec --json, no steer.
    let mut not_found = Case {
        frames: Vec::new(),
        requests: BTreeMap::new(),
    };
    not_found.requests.insert(0, "initialize".to_owned());
    not_found.frames.push(
        json!({"id": 0, "error": {"code": -32601, "message": "Method not found: initialize"}}),
    );
    for (i, case) in [None, Some(&not_found)].into_iter().enumerate() {
        let name = format!("degraded-{i}");
        let fake = world.fake(&name, case, "exec", false)?;
        let attempt = world.attempt(&format!("A{i}")).await?;
        let mut turn = world.turn(&fake, &format!("logs/{name}"))?;
        if case.is_none() {
            turn.launch.app_server_help = None;
        }
        let (tx, steers) = mpsc::channel(4);
        let receipt = steer(
            &world,
            &tx,
            &format!("I{i}"),
            "Stop and reply STEERED.",
            None,
        )
        .await?;
        let outcome =
            codex::run(&world.store, &attempt, &turn, &CancelLatch::new(), steers).await?;
        assert_eq!(
            (outcome.state, outcome.mode),
            (AttemptState::Settled, Mode::Exec),
            "{name}"
        );
        assert_eq!(
            receipt.await?,
            SteerReceipt::CapabilityUnsupported,
            "{name}"
        );
        assert_eq!(
            input_state(&world, &format!("I{i}"))?,
            InputState::Queued,
            "{name}"
        );
        let exec = argvs(&fake)?
            .into_iter()
            .find(|a| a.first().map(String::as_str) == Some("exec"))
            .ok_or("exec never ran")?;
        assert_eq!(exec[..2], ["exec", "--json"], "{name}");
        assert!(exec.iter().any(|a| a == codex::YOLO_FLAG), "{exec:?}");
        assert_eq!(outcome.args[..2], ["exec", "--json"]);
        let session = world.store.reader()?.session("S1")?.ok_or("no session")?;
        assert_eq!(session.capabilities["mid_turn_steer"], false, "{name}");
        assert_eq!(
            session.native_id.as_deref(),
            Some("01a1a3fe-be5e-7d50-9545-92d20e884ca3")
        );
        let degraded: Vec<Value> = events(&world, "adapter.degraded")?
            .into_iter()
            .filter(|e| e["attempt_id"] == format!("A{i}"))
            .collect();
        assert_eq!(degraded.len(), 1, "{name}: {degraded:?}");
    }

    // A stale expectedTurnId is refused by codex: REJECTED; a steer for the active turn is delivered and applied.
    let fake = world.fake(
        "steer",
        Some(&Case::load("app-server-steer")?),
        "exec",
        false,
    )?;
    let attempt = world.attempt("A5").await?;
    let turn = world.turn(&fake, "logs/A5")?;
    let (tx, steers) = mpsc::channel(4);
    let latch = CancelLatch::new();
    let running = codex::run(&world.store, &attempt, &turn, &latch, steers);
    let steering = async {
        let (_, turn_id) = active_turn(&world, "A5").await?;
        let stale = steer(
            &world,
            &tx,
            "I5",
            "Stop and reply STEERED.",
            Some("hoh-stale-turn-id"),
        )
        .await?;
        let stale = stale.await?;
        let current = steer(
            &world,
            &tx,
            "I6",
            "Stop running the command now and reply exactly STEERED.",
            None,
        )
        .await?;
        Ok::<(SteerReceipt, SteerReceipt, String), Box<dyn std::error::Error>>((
            stale,
            current.await?,
            turn_id,
        ))
    };
    let (outcome, steering) = tokio::join!(running, steering);
    let (outcome, (stale, current, turn_id)) = (outcome?, steering?);
    assert!(matches!(stale, SteerReceipt::Rejected { .. }), "{stale:?}");
    assert_eq!(input_state(&world, "I5")?, InputState::Rejected);
    assert_eq!(
        current,
        SteerReceipt::Delivered {
            turn_id: turn_id.clone()
        }
    );
    assert_eq!(input_state(&world, "I6")?, InputState::Applied);
    let steers: Vec<Value> = received(&fake)?
        .into_iter()
        .filter(|m| m["method"] == "turn/steer")
        .collect();
    assert_eq!(steers[0]["params"]["expectedTurnId"], "hoh-stale-turn-id");
    assert_eq!(steers[1]["params"]["expectedTurnId"], json!(turn_id));
    assert_eq!(outcome.state, AttemptState::Settled);

    // ② exec without the YOLO flag and no app-server: nothing starts.
    let fake = world.fake("no-yolo", None, "exec", false)?;
    let attempt = world.attempt("A7").await?;
    let mut turn = world.turn(&fake, "logs/no-yolo")?;
    turn.launch.app_server_help = None;
    turn.launch.exec_help = turn.launch.exec_help.replace(codex::YOLO_FLAG, "");
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("BLOCKED_CAPABILITY"))
    );
    assert!(argvs(&fake)?.is_empty(), "no process may start");
    let row = world.store.reader()?.attempt("A7")?.ok_or("no A7")?;
    assert_eq!(row.failure_reason.as_deref(), Some("BLOCKED_CAPABILITY"));
    Ok(())
}

/// Approvals and user-input requests are answered at once by the headless policy: declined or refused, never a
/// made-up human choice, each recorded (REQ-024).
#[tokio::test]
async fn server_requests_are_answered_at_once_without_a_human_choice() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    let at = case.position("turn/completed")?;
    let thread = case.thread_id()?;
    case.frames.insert(
        at,
        json!({"id": 900, "method": "item/commandExecution/requestApproval",
               "params": {"threadId": thread, "turnId": "t", "itemId": "i", "command": "rm -rf /tmp/x"}}),
    );
    case.frames.insert(
        at + 1,
        json!({"id": 901, "method": "item/tool/requestUserInput",
               "params": {"threadId": thread, "turnId": "t", "itemId": "j", "questions": []}}),
    );
    let fake = world.fake("requests", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(outcome.state, AttemptState::Settled);
    let got = received(&fake)?;
    let answer = |id: i64| {
        got.iter()
            .find(|m| m["id"] == id && m.get("method").is_none())
            .cloned()
    };
    assert_eq!(
        answer(900).ok_or("900 unanswered")?["result"],
        json!({"decision": "decline"})
    );
    let refused = answer(901).ok_or("901 unanswered")?;
    assert!(
        refused["error"]["code"].is_i64() && refused.get("result").is_none(),
        "{refused}"
    );
    let recorded = events(&world, "adapter.server_request")?;
    assert_eq!(
        recorded
            .iter()
            .map(|e| e["method"].as_str().unwrap_or(""))
            .collect::<Vec<_>>(),
        [
            "item/commandExecution/requestApproval",
            "item/tool/requestUserInput"
        ]
    );
    Ok(())
}

/// An interrupt codex never answers: after the native grace SIGTERM and SIGKILL go to the group; still CANCELLED.
#[tokio::test]
async fn an_unanswered_interrupt_escalates_to_the_group_after_the_native_grace() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-interrupt")?;
    // The recorded turn/interrupt response, and everything after it, never comes.
    let interrupt = case
        .requests
        .iter()
        .find(|(_, m)| *m == "turn/interrupt")
        .map(|(id, _)| *id)
        .ok_or("no turn/interrupt")?;
    case.requests.remove(&interrupt);
    let fake = world.fake("deaf", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/A1")?;
    let cancel = CancelLatch::new();
    let (_steer, steers) = mpsc::channel(4);
    let running = codex::run(&world.store, &attempt, &turn, &cancel, steers);
    let trigger = async {
        active_turn(&world, "A1").await?;
        cancel.request();
        Ok::<std::time::Instant, Box<dyn std::error::Error>>(std::time::Instant::now())
    };
    let (outcome, at) = tokio::join!(running, trigger);
    let (outcome, at) = (outcome?, at?);
    assert_eq!(outcome.state, AttemptState::Cancelled);
    assert!(
        received(&fake)?
            .iter()
            .any(|m| m["method"] == "turn/interrupt")
    );
    let signals: Vec<_> = outcome.steps.iter().map(|s| (s.signal, s.target)).collect();
    use hoh_agents::process::{Signal, Target};
    assert_eq!(
        signals.first(),
        Some(&(Signal::Terminate, Target::Group)),
        "{signals:?}"
    );
    // The grace is waited, but not much longer.
    let took = at.elapsed();
    assert!(took >= turn.escalation.native_grace, "{took:?}");
    assert!(
        took <= turn.escalation.native_grace + turn.escalation.term_grace + Duration::from_secs(3),
        "{took:?}"
    );
    assert!(group_members(outcome.pid.ok_or("no pid")?).is_empty());
    Ok(())
}

/// The deadline interrupts the turn like a cancel, and the turn fails as DEADLINE_EXCEEDED.
#[tokio::test]
async fn a_deadline_interrupts_the_turn_and_fails_it() -> R {
    let world = World::new()?;
    let fake = world.fake(
        "slow",
        Some(&Case::load("app-server-interrupt")?),
        "exec",
        false,
    )?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.deadline = Some(Duration::from_millis(500));
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("DEADLINE_EXCEEDED"))
    );
    assert_eq!(outcome.status.as_deref(), Some("interrupted"));
    assert!(
        received(&fake)?
            .iter()
            .any(|m| m["method"] == "turn/interrupt")
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.cancel_requested),
        (AttemptState::Failed, false)
    );
    Ok(())
}

/// An `error` notification that codex will retry is recorded and is no end of the turn.
#[tokio::test]
async fn an_error_codex_retries_is_no_end() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    let at = case.position("turn/completed")?;
    let thread = case.thread_id()?;
    case.frames.insert(
        at,
        json!({"method": "error", "params": {"threadId": thread, "turnId": "t", "willRetry": true,
               "error": {"message": "stream disconnected", "codexErrorInfo": "serverOverloaded"}}}),
    );
    let fake = world.fake("retry", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(outcome.state, AttemptState::Settled);
    let errors = events(&world, "adapter.error")?;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["willRetry"], true);
    Ok(())
}
