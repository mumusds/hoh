//! M1-T04 Codex adapter against fakes that replay recorded codex 0.166.0 traffic (TEST-024-01..03): `codex
//! app-server` by tests/fakes/app_server.py, `codex exec --json` by a bash replay. The adapter's results are checked
//! in the store it wrote: attempts, sessions, the input queue and events.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use common::{R, World, probed};
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
        capabilities: recorded_capabilities(),
    })
}

/// What `probe()` finds for the recorded codex 0.166.0: its schema has `thread/resume`, `turn/steer`,
/// `item/agentMessage/delta` and `thread/tokenUsage/updated` (compat_probe tests the probe itself).
fn recorded_capabilities() -> BTreeMap<String, Option<bool>> {
    probed(&[
        ("structured_events", true),
        ("resume", true),
        ("mid_turn_steer", true),
        ("partial_messages", true),
        ("token_usage", true),
    ])
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
    assert_eq!(outcome.reply.as_deref(), Some("HOH_SAMPLE_OK"));
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
    // Every notification of the resumed run (deprecationNotice, thread/goal/cleared, ...) is in the 0.166.0 schema.
    let unknown: Vec<Value> = events(&world, "adapter.unknown_event")?
        .into_iter()
        .filter(|e| e["attempt_id"] == "A4")
        .collect();
    assert!(unknown.is_empty(), "{unknown:?}");
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
        assert_eq!(outcome.reply.as_deref(), Some("HOH_SAMPLE_OK"), "{name}");
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

/// Replace the exec lines a fake replays.
fn rewrite_exec(fake: &Path, lines: &[Value]) -> R {
    let d = fake.parent().ok_or("no dir")?;
    for entry in std::fs::read_dir(d)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("exec."))
        {
            std::fs::remove_file(path)?;
        }
    }
    for (i, line) in lines.iter().enumerate() {
        std::fs::write(d.join(format!("exec.{i:04}")), format!("{line}\n"))?;
    }
    Ok(())
}

/// exec's recorded failures (CR-003): a 401 in turn.failed or in an earlier `error` event is BLOCKED_AUTH; another
/// failure keeps its own reason.
#[tokio::test]
async fn exec_failures_are_classified_from_the_recording() -> R {
    let world = World::new()?;
    let recorded: Vec<Value> = read(format!("{FIXTURES}/exec-unauthenticated/stdout.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    // Only the earlier `error` events say 401.
    let mut hinted = recorded.clone();
    let last = hinted.len() - 1;
    assert_eq!(hinted[last]["type"], "turn.failed");
    hinted[last]["error"]["message"] = json!("stream disconnected before completion");
    for (i, (exec, lines, reason)) in [
        ("exec-unauthenticated", None, "BLOCKED_AUTH"),
        ("exec-unauthenticated", Some(&hinted), "BLOCKED_AUTH"),
        ("exec-turn-failed", None, "turn failed"),
    ]
    .into_iter()
    .enumerate()
    {
        let fake = world.fake(&format!("exec-failed-{i}"), None, exec, false)?;
        if let Some(lines) = lines {
            rewrite_exec(&fake, lines)?;
        }
        let attempt = world.attempt(&format!("A{i}")).await?;
        let mut turn = world.turn(&fake, &format!("logs/exec-failed-{i}"))?;
        turn.launch.app_server_help = None;
        let outcome = run(&world, &attempt, &turn).await?;
        assert_eq!(
            (outcome.state, outcome.failure.as_deref(), outcome.mode),
            (AttemptState::Failed, Some(reason), Mode::Exec),
            "{exec} {i}"
        );
        let failed: Vec<Value> = events(&world, "adapter.turn_failed")?
            .into_iter()
            .filter(|e| e["attempt_id"] == format!("A{i}"))
            .collect();
        let blocked = (reason == "BLOCKED_AUTH").then_some("BLOCKED_AUTH");
        assert_eq!(failed.len(), 1, "{exec} {i}");
        assert_eq!(failed[0]["blocked_reason"], json!(blocked), "{exec} {i}");
    }
    Ok(())
}

/// A turn codex reports interrupted without our cancel is CANCELLED, with no cancel recorded as requested.
#[tokio::test]
async fn an_interrupted_turn_is_cancelled_without_a_cancel_request() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    let at = case.position("turn/completed")?;
    case.frames[at]["params"]["turn"]["status"] = json!("interrupted");
    let fake = world.fake("interrupted", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(
        (outcome.state, outcome.failure),
        (AttemptState::Cancelled, None)
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.cancel_requested),
        (AttemptState::Cancelled, false)
    );
    Ok(())
}

/// Notifications of another thread or turn neither end this turn nor change its reply or items (M1T04R-01), also
/// when they come before this turn's id is known.
#[tokio::test]
async fn notifications_of_another_thread_or_turn_are_not_this_turns() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    let thread = case.thread_id()?;
    let foreign_done = |thread: &str, turn: &str| {
        json!({"method": "turn/completed", "params": {"threadId": thread,
               "turn": {"id": turn, "status": "completed", "items": [], "error": null}}})
    };
    let foreign_reply = json!({"method": "item/completed", "params": {"threadId": thread, "turnId": "other-turn",
        "item": {"type": "agentMessage", "id": "m-x", "text": "FOREIGN_REPLY"}}});
    // Before turn/start is answered (this turn's id unknown yet) and after it.
    let start = case
        .frames
        .iter()
        .position(|f| f["id"] == 2)
        .ok_or("no turn/start response")?;
    // An item of this very turn before its id is known: kept, and counted once the id is known.
    let early_item = case
        .frames
        .iter()
        .find(|f| f["method"] == "item/completed")
        .cloned()
        .ok_or("no item")?;
    case.frames.insert(start, early_item);
    let start = start + 1;
    case.frames
        .insert(start, foreign_done("other-thread", "t-1"));
    case.frames
        .insert(start + 1, foreign_done(&thread, "other-turn"));
    case.frames.insert(start + 2, foreign_reply.clone());
    let done = case.position("turn/completed")?;
    let late = case.frames[done..]
        .iter()
        .position(|f| f["method"] == "turn/completed")
        .ok_or("no end")?
        + done;
    case.frames.insert(late, foreign_reply.clone());
    case.frames
        .insert(late + 1, foreign_done(&thread, "other-turn"));
    let ours = Case::load("app-server-turn")?
        .frames
        .iter()
        .filter(|f| f["method"] == "item/started" || f["method"] == "item/completed")
        .count();
    let fake = world.fake("foreign", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(outcome.state, AttemptState::Settled);
    assert_eq!(outcome.reply.as_deref(), Some("HOH_SAMPLE_OK"));
    assert_eq!(
        events(&world, "adapter.item")?.len(),
        ours + 1,
        "this turn's early item was dropped"
    );

    // Only another turn's end, then the stream ends: not SETTLED.
    let mut case = Case::load("app-server-turn")?;
    let start = case
        .frames
        .iter()
        .position(|f| f["id"] == 2)
        .ok_or("no turn/start response")?;
    case.frames.truncate(start + 1);
    case.frames.push(foreign_done(&thread, "other-turn"));
    case.frames.push(foreign_done("other-thread", "t-2"));
    let fake = world.fake("foreign-only", Some(&case), "exec", true)?;
    let attempt = world.attempt_in("codex", "A2", "S2", "0.166.0").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A2")?).await?;
    assert_ne!(
        outcome.state,
        AttemptState::Settled,
        "{:?}",
        outcome.failure
    );
    let row = world.store.reader()?.attempt("A2")?.ok_or("no A2")?;
    assert_ne!(row.state, AttemptState::Settled);
    Ok(())
}

/// An initialize answer that comes after the handshake timed out does not undo the move to exec (M1T04R-02).
#[tokio::test]
async fn a_late_initialize_answer_does_not_undo_the_degradation() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    case.frames.insert(0, json!({"hoh_sleep": 0.4}));
    let fake = world.fake("late", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.handshake_timeout = Duration::from_millis(50);
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.mode),
        (AttemptState::Settled, Mode::Exec)
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(row.state, AttemptState::Settled);
    assert!(row.ended_at.is_some());
    assert_eq!(events(&world, "adapter.degraded")?.len(), 1);
    Ok(())
}

/// App Server and exec events of one attempt never collide (M1T04R-03).
#[tokio::test]
async fn app_server_and_exec_events_of_one_attempt_do_not_collide() -> R {
    let world = World::new()?;
    let mut case = Case {
        frames: Vec::new(),
        requests: BTreeMap::new(),
    };
    case.requests.insert(0, "initialize".to_owned());
    case.frames
        .push(json!({"method": "hoh/unrecorded", "params": {}}));
    case.frames
        .push(json!({"id": 0, "error": {"code": -32601, "message": "Method not found"}}));
    let fake = world.fake("collide", Some(&case), "exec", false)?;
    let mut lines: Vec<Value> = read(format!("{FIXTURES}/exec/stdout.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    lines.insert(1, json!({"type": "hoh.unrecorded"}));
    lines.insert(2, json!("not an object"));
    rewrite_exec(&fake, &lines)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(
        (outcome.state, outcome.mode),
        (AttemptState::Settled, Mode::Exec)
    );
    assert_eq!(events(&world, "adapter.unknown_event")?.len(), 2);
    Ok(())
}

/// After the app-server exits, a prompt still being written (its stdin held open, unread, by a process outside the
/// group) does not hold the turn up (M1T04R-04).
#[tokio::test]
async fn an_unwritten_prompt_does_not_hold_up_the_end() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    let thread_response = case
        .frames
        .iter()
        .position(|f| f["id"] == 1)
        .ok_or("no thread response")?;
    case.frames.truncate(thread_response + 1);
    let fake = world.fake("holder", Some(&case), "exec", true)?;
    // The fake leaves a sleeper in its own session holding stdin's read end (`<&0`: bash gives a background job
    // /dev/null otherwise) and reading nothing.
    let d = fake.parent().ok_or("no dir")?.to_path_buf();
    let script = read(&fake)?.replace(
        "if [ \"$1\" = app-server ]; then exec",
        &format!(
            "if [ \"$1\" = app-server ]; then setsid sleep 30 <&0 >/dev/null 2>&1 & echo $! > '{}/sleeper.pid'; exec",
            d.display()
        ),
    );
    std::fs::write(&fake, script)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.prompt = "p".repeat(4 << 20);
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(20), run(&world, &attempt, &turn)).await;
    let took = started.elapsed();
    if let Ok(pid) = read(d.join("sleeper.pid")) {
        let _ = std::process::Command::new("kill").arg(pid.trim()).status();
    }
    let outcome = outcome??;
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_ne!(outcome.state, AttemptState::Settled);
    Ok(())
}

/// A steer whose user message arrives before codex's answer to turn/steer still ends APPLIED (M1T04R-05).
#[tokio::test]
async fn a_steer_seen_before_its_answer_is_applied() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-steer")?;
    let text = "Stop running the command now and reply exactly STEERED.";
    let item = case
        .frames
        .iter()
        .position(|f| {
            f["method"] == "item/started" && f["params"]["item"]["content"][0]["text"] == text
        })
        .ok_or("no steer item")?;
    let moved = case.frames.remove(item);
    let ack = case
        .frames
        .iter()
        .position(|f| f["id"] == 4)
        .ok_or("no steer answer")?;
    // codex can only show the steer's message once it has the steer: wait for the turn/steer, then the message, then
    // the answer.
    case.frames.insert(
        ack,
        json!({"hoh_wait": {"method": "turn/steer", "count": 1}}),
    );
    case.frames.insert(ack + 1, moved);
    // The stale steer is not part of this case.
    let stale = case
        .frames
        .iter()
        .position(|f| f["id"] == 3)
        .ok_or("no stale answer")?;
    case.frames.remove(stale);
    case.requests.remove(&3);
    let fake = world.fake("early-item", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/A1")?;
    let (tx, steers) = mpsc::channel(4);
    let latch = CancelLatch::new();
    let running = codex::run(&world.store, &attempt, &turn, &latch, steers);
    let steering = async {
        active_turn(&world, "A1").await?;
        let receipt = steer(&world, &tx, "I1", text, None).await?;
        Ok::<SteerReceipt, Box<dyn std::error::Error>>(receipt.await?)
    };
    let (outcome, receipt) = tokio::join!(running, steering);
    let (outcome, receipt) = (outcome?, receipt?);
    assert!(
        matches!(receipt, SteerReceipt::Delivered { .. }),
        "{receipt:?}"
    );
    assert_eq!(input_state(&world, "I1")?, InputState::Applied);
    assert_eq!(outcome.state, AttemptState::Settled);
    Ok(())
}

/// A tool that started before the stream ended leaves an UNKNOWN_EFFECT attempt in the store, ended, for the
/// reconciler — over the App Server, also when this turn's id was never confirmed, and over exec (M1T04R-06/07).
#[tokio::test]
async fn a_tool_started_before_the_stream_ended_is_an_unknown_effect() -> R {
    let world = World::new()?;
    // Turn known: the interrupt case cut right after its command started.
    let mut case = Case::load("app-server-interrupt")?;
    let tool = case
        .frames
        .iter()
        .position(|f| {
            f["method"] == "item/started" && f["params"]["item"]["type"] == "commandExecution"
        })
        .ok_or("no command")?;
    let command = case.frames[tool].clone();
    case.frames.truncate(tool + 1);
    // Turn never confirmed: this thread's turn and command arrive before turn/start is answered, then the stream ends.
    let mut early = Case::load("app-server-interrupt")?;
    let start = early
        .frames
        .iter()
        .position(|f| f["id"] == 2)
        .ok_or("no turn/start answer")?;
    let started = early
        .frames
        .iter()
        .find(|f| f["method"] == "turn/started")
        .cloned()
        .ok_or("no turn/started")?;
    early.frames.truncate(start);
    early.frames.push(started);
    early.frames.push(command);
    for (i, case) in [case, early].iter().enumerate() {
        let fake = world.fake(&format!("tool-eof-{i}"), Some(case), "exec", true)?;
        let attempt = world
            .attempt_in("codex", &format!("A{i}"), &format!("S{i}"), "0.166.0")
            .await?;
        let outcome = run(&world, &attempt, &world.turn(&fake, &format!("logs/A{i}"))?).await?;
        assert_eq!(
            (outcome.state, outcome.failure.as_deref()),
            (AttemptState::UnknownEffect, Some("PROTOCOL_INCOMPLETE")),
            "case {i}"
        );
        let row = world
            .store
            .reader()?
            .attempt(&format!("A{i}"))?
            .ok_or("no attempt")?;
        assert_eq!(
            (row.state, row.ended_at.is_some()),
            (AttemptState::UnknownEffect, true),
            "case {i}"
        );
    }
    // exec: a command started, then the process ends without turn.completed.
    let fake = world.fake("exec-tool-eof", None, "exec", false)?;
    rewrite_exec(
        &fake,
        &[
            json!({"type": "thread.started", "thread_id": "t-exec"}),
            json!({"type": "turn.started"}),
            json!({"type": "item.started", "item": {"id": "item_0", "type": "command_execution", "command": "touch x"}}),
        ],
    )?;
    let attempt = world.attempt_in("codex", "A9", "S9", "0.166.0").await?;
    let mut turn = world.turn(&fake, "logs/A9")?;
    turn.launch.app_server_help = None;
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(outcome.state, AttemptState::UnknownEffect);
    let row = world.store.reader()?.attempt("A9")?.ok_or("no A9")?;
    assert_eq!(
        (row.state, row.ended_at.is_some()),
        (AttemptState::UnknownEffect, true)
    );
    Ok(())
}

/// A user message that came while a steer was waiting for its answer belongs to that steer: when codex refuses it,
/// a later steer with the same text does not take the message over (M1T04R-08).
#[tokio::test]
async fn a_refused_steers_message_is_not_taken_by_a_later_one() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-steer")?;
    let text = "Stop running the command now and reply exactly STEERED.";
    let item = case
        .frames
        .iter()
        .position(|f| {
            f["method"] == "item/started" && f["params"]["item"]["content"][0]["text"] == text
        })
        .ok_or("no steer item")?;
    let message = case.frames.remove(item);
    let refusal = case
        .frames
        .iter()
        .position(|f| f["id"] == 3)
        .ok_or("no refusal")?;
    case.frames.insert(
        refusal,
        json!({"hoh_wait": {"method": "turn/steer", "count": 1}}),
    );
    case.frames.insert(refusal + 1, message);
    let fake = world.fake("refused-then", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/A1")?;
    let (tx, steers) = mpsc::channel(4);
    let latch = CancelLatch::new();
    let running = codex::run(&world.store, &attempt, &turn, &latch, steers);
    let steering = async {
        active_turn(&world, "A1").await?;
        let first = steer(&world, &tx, "I1", text, Some("hoh-stale-turn-id"))
            .await?
            .await?;
        let second = steer(&world, &tx, "I2", text, None).await?.await?;
        Ok::<(SteerReceipt, SteerReceipt), Box<dyn std::error::Error>>((first, second))
    };
    let (outcome, receipts) = tokio::join!(running, steering);
    let (outcome, (first, second)) = (outcome?, receipts?);
    assert!(matches!(first, SteerReceipt::Rejected { .. }), "{first:?}");
    assert!(
        matches!(second, SteerReceipt::Delivered { .. }),
        "{second:?}"
    );
    assert_eq!(input_state(&world, "I1")?, InputState::Rejected);
    assert_eq!(input_state(&world, "I2")?, InputState::Delivered);
    assert_eq!(outcome.state, AttemptState::Settled);
    Ok(())
}

/// One user message item applies one steer, even if codex repeats it (M1T04R-09).
#[tokio::test]
async fn one_message_item_applies_one_steer() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-steer")?;
    let text = "Stop running the command now and reply exactly STEERED.";
    let turn_id = case
        .frames
        .iter()
        .find_map(|f| f["result"]["turn"]["id"].as_str())
        .ok_or("no turn")?
        .to_owned();
    // Both steers are taken; the message item comes twice with one id.
    let refusal = case
        .frames
        .iter()
        .position(|f| f["id"] == 3)
        .ok_or("no refusal")?;
    case.frames[refusal] = json!({"id": 3, "result": {"turnId": turn_id}});
    let item = case
        .frames
        .iter()
        .position(|f| {
            f["method"] == "item/started" && f["params"]["item"]["content"][0]["text"] == text
        })
        .ok_or("no steer item")?;
    let again = case.frames[item].clone();
    case.frames.insert(item + 1, again);
    let fake = world.fake("twice", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/A1")?;
    let (tx, steers) = mpsc::channel(4);
    let latch = CancelLatch::new();
    let running = codex::run(&world.store, &attempt, &turn, &latch, steers);
    let steering = async {
        active_turn(&world, "A1").await?;
        let first = steer(&world, &tx, "I1", text, None).await?.await?;
        let second = steer(&world, &tx, "I2", text, None).await?.await?;
        Ok::<(SteerReceipt, SteerReceipt), Box<dyn std::error::Error>>((first, second))
    };
    let (outcome, receipts) = tokio::join!(running, steering);
    let (outcome, _) = (outcome?, receipts?);
    let states = [input_state(&world, "I1")?, input_state(&world, "I2")?];
    assert_eq!(states, [InputState::Applied, InputState::Delivered]);
    assert_eq!(outcome.state, AttemptState::Settled);
    Ok(())
}

/// A message that comes after the refusal of a same-text steer goes to the later steer (the refused one is gone).
#[tokio::test]
async fn a_message_after_a_refusal_goes_to_the_later_steer() -> R {
    let world = World::new()?;
    let case = Case::load("app-server-steer")?;
    let text = "Stop running the command now and reply exactly STEERED.";
    let fake = world.fake("after-refusal", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/A1")?;
    let (tx, steers) = mpsc::channel(4);
    let latch = CancelLatch::new();
    let running = codex::run(&world.store, &attempt, &turn, &latch, steers);
    let steering = async {
        active_turn(&world, "A1").await?;
        let first = steer(&world, &tx, "I1", text, Some("hoh-stale-turn-id"))
            .await?
            .await?;
        let second = steer(&world, &tx, "I2", text, None).await?.await?;
        Ok::<(SteerReceipt, SteerReceipt), Box<dyn std::error::Error>>((first, second))
    };
    let (outcome, receipts) = tokio::join!(running, steering);
    let (outcome, (first, _)) = (outcome?, receipts?);
    assert!(matches!(first, SteerReceipt::Rejected { .. }), "{first:?}");
    assert_eq!(
        [input_state(&world, "I1")?, input_state(&world, "I2")?],
        [InputState::Rejected, InputState::Applied]
    );
    assert_eq!(outcome.state, AttemptState::Settled);
    Ok(())
}

/// stdin that breaks while codex goes on (it closed its end) stops the process, but the stream's end decides: a tool
/// that had started still makes the end UNKNOWN_EFFECT, not a stdin failure.
#[tokio::test]
async fn a_broken_stdin_leaves_the_end_to_the_stream() -> R {
    let world = World::new()?;
    let full = Case::load("app-server-interrupt")?;
    let started = full
        .frames
        .iter()
        .find(|f| f["method"] == "turn/started")
        .cloned()
        .ok_or("no turn/started")?;
    let command = full
        .frames
        .iter()
        .find(|f| {
            f["method"] == "item/started" && f["params"]["item"]["type"] == "commandExecution"
        })
        .cloned()
        .ok_or("no command")?;
    let thread_answer = full
        .frames
        .iter()
        .position(|f| f["id"] == 1)
        .ok_or("no thread answer")?;
    let mut case = Case {
        frames: full.frames[..thread_answer].to_vec(),
        requests: full.requests.clone(),
    };
    // After thread/start arrives codex closes stdin, answers it, and its turn and command follow: turn/start can
    // no longer be written.
    case.frames
        .push(json!({"hoh_wait": {"method": "thread/start", "count": 1}}));
    case.frames.push(json!({"hoh_close_stdin": true}));
    // One write: the client reads the answer and this turn's frames together, before it can stop codex.
    case.frames
        .push(json!({"hoh_batch": [full.frames[thread_answer].clone(), started, command]}));
    // codex stays up after that, so the failed write is seen while it runs (the adapter then stops it).
    case.frames.push(json!({"hoh_sleep": 5.0}));
    let fake = world.fake("broken-stdin", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::UnknownEffect, Some("PROTOCOL_INCOMPLETE"))
    );
    let stdin_failed = events(&world, "adapter.diagnostic")?
        .into_iter()
        .any(|d| d["reason"] == "stdin failed");
    assert!(stdin_failed, "the stdin failure is recorded");
    Ok(())
}

/// Two same-text steers in flight, one aimed at a stale turn: the message of this turn goes to the steer aimed at it,
/// whatever order the answers come in (M1T04R-10).
#[tokio::test]
async fn a_message_goes_to_the_steer_aimed_at_its_turn() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-steer")?;
    let text = "Stop running the command now and reply exactly STEERED.";
    let item = case
        .frames
        .iter()
        .position(|f| {
            f["method"] == "item/started" && f["params"]["item"]["content"][0]["text"] == text
        })
        .ok_or("no steer item")?;
    let message = case.frames.remove(item);
    // Both steers arrive, then this turn's message, then I2's answer, then I1's refusal.
    let refusal = case
        .frames
        .iter()
        .position(|f| f["id"] == 3)
        .ok_or("no refusal")?;
    let refused = case.frames.remove(refusal);
    let ack = case
        .frames
        .iter()
        .position(|f| f["id"] == 4)
        .ok_or("no answer")?;
    case.frames.insert(
        ack,
        json!({"hoh_wait": {"method": "turn/steer", "count": 2}}),
    );
    case.frames.insert(ack + 1, message);
    case.frames.insert(ack + 3, refused);
    let fake = world.fake("aimed", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/A1")?;
    let (tx, steers) = mpsc::channel(4);
    let latch = CancelLatch::new();
    let running = codex::run(&world.store, &attempt, &turn, &latch, steers);
    let steering = async {
        active_turn(&world, "A1").await?;
        let first = steer(&world, &tx, "I1", text, Some("hoh-stale-turn-id")).await?;
        let second = steer(&world, &tx, "I2", text, None).await?;
        Ok::<(SteerReceipt, SteerReceipt), Box<dyn std::error::Error>>((
            first.await?,
            second.await?,
        ))
    };
    let (outcome, receipts) = tokio::join!(running, steering);
    let (outcome, (first, second)) = (outcome?, receipts?);
    assert!(matches!(first, SteerReceipt::Rejected { .. }), "{first:?}");
    assert!(
        matches!(second, SteerReceipt::Delivered { .. }),
        "{second:?}"
    );
    assert_eq!(
        [input_state(&world, "I1")?, input_state(&world, "I2")?],
        [InputState::Rejected, InputState::Applied]
    );
    assert_eq!(outcome.state, AttemptState::Settled);
    Ok(())
}

/// A steer whose input someone else has already moved on (here REJECTED) is recorded and does not stop the turn.
#[tokio::test]
async fn an_input_moved_elsewhere_does_not_stop_the_turn() -> R {
    let world = World::new()?;
    let case = Case::load("app-server-steer")?;
    let fake = world.fake("moved-elsewhere", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/A1")?;
    let (tx, steers) = mpsc::channel(4);
    let latch = CancelLatch::new();
    let running = codex::run(&world.store, &attempt, &turn, &latch, steers);
    let steering = async {
        active_turn(&world, "A1").await?;
        world
            .store
            .enqueue_input(NewInput {
                input_id: "I1".to_owned(),
                run_id: "R".to_owned(),
                kind: "STEER".to_owned(),
                target_role: Some("developer".to_owned()),
                payload: json!({"text": "x"}),
            })
            .await?;
        world
            .store
            .move_input(hoh_core::persistence::InputMove {
                input_id: "I1".to_owned(),
                from: InputState::Queued,
                to: InputState::Rejected,
                reason: Some("withdrawn".to_owned()),
            })
            .await?;
        let (reply, receipt) = oneshot::channel();
        tx.send(Steer {
            input_id: "I1".to_owned(),
            text: "Stop running the command now and reply exactly STEERED.".to_owned(),
            expected_turn: Some("hoh-stale-turn-id".to_owned()),
            reply,
        })
        .await
        .map_err(|_| "no steer channel")?;
        let first = receipt.await?;
        let (reply, receipt) = oneshot::channel();
        tx.send(Steer {
            input_id: "I1".to_owned(),
            text: "Stop running the command now and reply exactly STEERED.".to_owned(),
            expected_turn: None,
            reply,
        })
        .await
        .map_err(|_| "no steer channel")?;
        Ok::<(SteerReceipt, SteerReceipt), Box<dyn std::error::Error>>((first, receipt.await?))
    };
    let (outcome, receipts) = tokio::join!(running, steering);
    let (outcome, _) = (outcome?, receipts?);
    assert_eq!(outcome.state, AttemptState::Settled);
    assert_eq!(
        input_state(&world, "I1")?,
        InputState::Rejected,
        "the other move stands"
    );
    let notes = events(&world, "adapter.diagnostic")?
        .into_iter()
        .filter(|d| d["reason"] == "input not moved")
        .count();
    assert!(notes >= 1, "the refused moves are recorded");
    Ok(())
}

/// TEST-027-02 (codex): a recorded app-server turn with one notification whose method is not in 0.166.0's known-frame
/// registry (`tests/fixtures/protocol/codex/0.166.0/known-frames.json`, the schema's methods) and an `x_future` field
/// in a known notification, against the same turn without that notification. Expected: no panic; exactly one
/// `adapter.unknown_event`, with the frame as received and the sha256 of its bytes; the raw log is the bytes the fake
/// wrote; `x_future` comes back from the raw log as sent; both turns end alike, as returned and as stored.
#[tokio::test]
async fn test_027_02_codex_keeps_an_unregistered_notification_once_and_ends_as_without_it() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    let at = case.position("item/completed")?;
    case.frames[at]["params"]["x_future"] = json!({"nested": [1, "二"], "flag": true});
    let x_future = case.frames[at]["params"]["x_future"].clone();
    let control = Case {
        frames: case.frames.clone(),
        ..Case::load("app-server-turn")?
    };
    let unknown = json!({"method": "hoh/futureNotification", "params": {"n": 1}});
    let at = case.position("turn/completed")?;
    case.frames.insert(at, unknown.clone());
    let mut ends = Vec::new();
    for (id, name, case) in [("A1", "with", &case), ("A2", "control", &control)] {
        let fake = world.fake(name, Some(case), "exec", false)?;
        let attempt = world.attempt(id).await?;
        let outcome = run(&world, &attempt, &world.turn(&fake, &format!("logs/{id}"))?).await?;
        let row = world.store.reader()?.attempt(id)?.ok_or("no attempt")?;
        ends.push((
            (outcome.state, outcome.failure.clone()),
            (row.state, row.failure_reason),
        ));
        let sent = std::fs::read(fake.parent().ok_or("no dir")?.join("log/sent.jsonl"))?;
        let raw = std::fs::read(&outcome.stdout)?;
        assert_eq!(
            hex(&raw),
            hex(&sent),
            "{name}: the raw log is what the fake wrote"
        );
        let kept = raw
            .split(|b| *b == b'\n')
            .filter_map(|l| serde_json::from_slice::<Value>(l).ok())
            .find_map(|f| f["params"].get("x_future").cloned());
        assert_eq!(
            kept.as_ref(),
            Some(&x_future),
            "{name}: x_future in the raw log"
        );
        if name == "with" {
            let line = sent
                .split(|b| *b == b'\n')
                .find(|l| serde_json::from_slice::<Value>(l).is_ok_and(|f| f == unknown))
                .ok_or("the unknown notification was not sent")?;
            let unknowns = events(&world, "adapter.unknown_event")?;
            assert_eq!(unknowns.len(), 1, "{unknowns:?}");
            assert_eq!(unknowns[0]["attempt_id"], id);
            assert_eq!(unknowns[0]["frame_sha256"], hex(line));
            assert_eq!(unknowns[0]["frame"], unknown);
        }
    }
    assert_eq!(
        events(&world, "adapter.unknown_event")?.len(),
        1,
        "none for the control"
    );
    assert_eq!(
        ends[0], ends[1],
        "the same end with and without the unknown notification"
    );
    Ok(())
}

/// The adapter's known methods for 0.166.0 (its schema's server notifications) and exec event types are the
/// version's known-frame registry, so "unknown" means the same in both (docs/13).
#[test]
fn the_known_methods_are_the_registry() -> R {
    let registry: Value = serde_json::from_str(&std::fs::read_to_string(format!(
        "{FIXTURES}/known-frames.json"
    ))?)?;
    let keys = |path: &[&str]| -> Vec<String> {
        let mut v = &registry;
        for p in path {
            v = &v[*p];
        }
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e["key"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(format!(
        "{FIXTURES}/app-server-schema/ServerNotification.json"
    ))?)?;
    let known: Vec<String> = codex::known_notifications(&schema).into_iter().collect();
    assert_eq!(keys(&["app_server", "notifications"]), known);
    assert_eq!(keys(&["exec"]).len(), 8);
    let mut exec = keys(&["exec"]);
    exec.sort();
    let mut ours: Vec<String> = hoh_agents::known::codex_exec().keys().cloned().collect();
    ours.sort();
    assert_eq!(exec, ours);
    Ok(())
}

/// The session row records what the probe found, not what the brand suggests: an App Server whose schema has no
/// `thread/resume`, `turn/steer` or `thread/tokenUsage/updated` makes a session without resume, steer or token usage,
/// so a dispatch that needs resume finds none (M1R-01, REQ-027). Probe, adapter and dispatch rule in one chain.
#[tokio::test]
async fn the_session_has_the_capabilities_the_probe_found() -> R {
    use hoh_agents::compat::Agent;
    use hoh_agents::probe::{ProbeEnv, probe};
    let world = World::new()?;
    let fake = world.dir.path().join("codex");
    std::fs::write(
        &fake,
        r#"#!/usr/bin/python3
import sys, json, pathlib
args = sys.argv[1:]
if args == ['--version']: print('codex-cli 0.166.0'); sys.exit(0)
if args == ['exec', '--help']: print('--json'); sys.exit(0)
if args == ['app-server', '--help']: print('--listen'); sys.exit(0)
if args[:2] == ['app-server', 'generate-json-schema']:
    out = pathlib.Path(args[args.index('--out') + 1]); out.mkdir(parents=True, exist_ok=True)
    for name, methods in [('ClientRequest', ['initialize', 'thread/start', 'turn/start']), ('ServerNotification', ['turn/completed'])]:
        schema = {'oneOf': [{'type': 'object', 'properties': {'method': {'type': 'string', 'enum': [m]}}} for m in methods]}
        (out / (name + '.json')).write_text(json.dumps(schema))
    sys.exit(0)
for line in sys.stdin:
    q = json.loads(line); m = q.get('method')
    if m == 'initialize': r = {'userAgent': 'fake'}
    elif m == 'thread/start': r = {'thread': {'id': 'thread-fake'}, 'model': 'fake'}
    elif m == 'turn/start': r = {'turn': {'id': 'turn-fake'}}
    else: continue
    print(json.dumps({'id': q['id'], 'result': r}), flush=True)
    if m == 'turn/start':
        print(json.dumps({'method': 'turn/completed', 'params': {'threadId': 'thread-fake', 'turn': {'id': 'turn-fake', 'status': 'completed'}}}), flush=True)
"#,
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    let env = ProbeEnv {
        path: world.dir.path().as_os_str().to_owned(),
        fixture_root: None,
        timeout: Duration::from_secs(10),
        log: None,
        deadline: None,
    };
    let entry = probe(Agent::Codex, &env)?;
    for absent in ["resume", "mid_turn_steer", "token_usage"] {
        assert_eq!(
            entry.capabilities.get(absent),
            Some(&Some(false)),
            "{absent}"
        );
    }
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.launch.app_server_help = Some("--listen".to_owned());
    turn.launch.exec_help = "--json".to_owned();
    turn.launch.yolo = false;
    turn.launch.known = ["turn/completed".to_owned()].into_iter().collect();
    turn.launch.capabilities = entry.capabilities.clone();
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.mode),
        (AttemptState::Settled, Mode::AppServer)
    );
    let session = world.store.reader()?.session("S1")?.ok_or("no session")?;
    for absent in ["resume", "mid_turn_steer", "token_usage"] {
        assert_eq!(session.capabilities[absent], false, "{absent}: {session:?}");
    }
    assert_eq!(session.capabilities["structured_events"], true);
    // The rule dispatch reads the row with: no resume, so a task that needs it is ROLE_CAPABILITY_MISSING.
    let granted = hoh_core::decision::granted_capabilities(&session.capabilities);
    assert!(
        !granted.contains("resume") && !granted.contains("mid_turn_steer"),
        "{granted:?}"
    );
    assert!(granted.contains("structured_events"));
    Ok(())
}

/// A frame too large while the handshake is refused fails the attempt: in ADR-012 §3 an oversized frame comes before
/// a refused protocol request, so codex is not tried again on exec and the attempt is not settled (M1R-02).
#[tokio::test]
async fn a_frame_too_large_during_a_refused_handshake_fails_the_attempt() -> R {
    let world = World::new()?;
    let mut case = Case {
        frames: Vec::new(),
        requests: BTreeMap::new(),
    };
    case.requests.insert(0, "initialize".to_owned());
    case.frames.push(json!({"hoh_batch": [
        {"id": 0, "error": {"code": -32601, "message": "Method not found: initialize"}},
        {"method": "item/agentMessage/delta", "params": {"delta": "x".repeat(1 << 20)}},
    ]}));
    let fake = world.fake("refused-huge", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref(), outcome.mode),
        (
            AttemptState::Failed,
            Some("PROTOCOL_FRAME_TOO_LARGE"),
            Mode::AppServer
        )
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.failure_reason.as_deref()),
        (AttemptState::Failed, Some("PROTOCOL_FRAME_TOO_LARGE"))
    );
    assert!(
        argvs(&fake)?
            .iter()
            .all(|a| a.first().map(String::as_str) != Some("exec")),
        "exec was not started"
    );
    assert!(events(&world, "adapter.degraded")?.is_empty());
    Ok(())
}

/// A frame longer than the 1 MiB limit, on app-server or on exec, fails the turn as PROTOCOL_FRAME_TOO_LARGE with its
/// diagnosis (length, prefix sha256, first 4 KiB); nothing more is read as frames and codex's group is stopped
/// (M1-T07 done_when; the host-level AC-027-03 is M2-T05's).
#[tokio::test]
async fn a_frame_too_large_fails_the_turn_with_its_diagnosis() -> R {
    let world = World::new()?;
    let mut case = Case::load("app-server-turn")?;
    let at = case.position("turn/completed")?;
    case.frames.insert(
        at,
        json!({"method": "item/agentMessage/delta", "params": {"delta": "x".repeat(1 << 20)}}),
    );
    let fake = world.fake("huge", Some(&case), "exec", false)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("PROTOCOL_FRAME_TOO_LARGE"))
    );
    let diagnosis = events(&world, "adapter.diagnostic")?
        .into_iter()
        .find(|d| d["reason"] == "PROTOCOL_FRAME_TOO_LARGE")
        .ok_or("no diagnosis")?;
    assert!(diagnosis["length"].as_u64().is_some_and(|n| n > 1 << 20));
    assert_eq!(diagnosis["prefix_sha256"].as_str().map(str::len), Some(64));
    assert_eq!(diagnosis["head"].as_str().map(str::len), Some(4 << 10));
    assert!(
        group_members(outcome.pid.ok_or("no pid")?).is_empty(),
        "codex's group is stopped"
    );

    // exec: one event line over the limit.
    let fake = world.fake("huge-exec", None, "exec", false)?;
    let line = format!(
        "{}\n",
        json!({"type": "item.completed", "item": {"type": "agent_message", "text": "x".repeat(1 << 20)}})
    );
    std::fs::write(fake.parent().ok_or("no dir")?.join("exec.0001"), line)?;
    let attempt = world.attempt("A2").await?;
    let mut turn = world.turn(&fake, "logs/A2")?;
    turn.launch.app_server_help = None;
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("PROTOCOL_FRAME_TOO_LARGE"))
    );
    let exec_diagnosis = events(&world, "adapter.diagnostic")?
        .into_iter()
        .filter(|d| d["reason"] == "PROTOCOL_FRAME_TOO_LARGE")
        .nth(1)
        .ok_or("no exec diagnosis")?;
    assert_eq!(exec_diagnosis["head"].as_str().map(str::len), Some(4 << 10));
    Ok(())
}
