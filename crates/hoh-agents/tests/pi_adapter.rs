//! M1-T05 Pi adapter against a fake `pi --mode rpc` replaying recorded pi 1.5.0 traffic (TEST-026-01..03,
//! tests/fakes/pi_rpc.py). The adapter's results are checked in the store it wrote and in the records it hands to its
//! subscriber.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use common::{R, World};
use hoh_agents::adapter::AttemptRef;
use hoh_agents::clock::{Clock, SystemClock, Timer};
use hoh_agents::pi::{self, Launch, Outcome, SettledBy, Turn};
use hoh_agents::process::{Escalation, group_members};
use hoh_core::lifecycle::{AttemptState, InputState};
use hoh_core::persistence::{CancelLatch, CancelReceipt, NewInput};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/pi/1.5.0"
);
const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/pi_rpc.py");

fn read(path: impl AsRef<Path>) -> R<String> {
    Ok(std::fs::read_to_string(path)?)
}

/// A clock that never moves; it counts the timers armed on it.
#[derive(Debug, Default)]
struct FrozenClock {
    timers: AtomicUsize,
}

impl Clock for FrozenClock {
    fn sleep(&self, _: Duration) -> Timer {
        self.timers.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

/// A recorded case: the records pi wrote and the type of each command, in order.
#[derive(Clone)]
struct Case {
    frames: Vec<Value>,
    commands: Vec<(String, String)>,
}

impl Case {
    fn load(name: &str) -> R<Case> {
        let frames = read(format!("{FIXTURES}/{name}/stdout.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Value>, _>>()?;
        let mut commands = Vec::new();
        for line in read(format!("{FIXTURES}/{name}/stdin.txt"))?.lines() {
            let Ok(c) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let (Some(id), Some(kind)) = (c["id"].as_str(), c["type"].as_str()) {
                commands.push((id.to_owned(), kind.to_owned()));
            }
        }
        Ok(Case { frames, commands })
    }

    fn position(&self, kind: &str) -> R<usize> {
        Ok(self
            .frames
            .iter()
            .position(|f| f["type"] == kind)
            .ok_or(format!("no {kind}"))?)
    }

    fn response(&self, id: &str) -> R<usize> {
        Ok(self
            .frames
            .iter()
            .position(|f| f["type"] == "response" && f["id"] == id)
            .ok_or(format!("no response {id}"))?)
    }

    /// The case with the recorded get_state answer of rpc-prompt in front (cases recorded without one).
    fn with_state(mut self) -> R<Case> {
        let prompt = Case::load("rpc-prompt")?;
        let state = prompt.frames[prompt.response("s1")?].clone();
        self.frames.insert(0, state);
        self.commands
            .insert(0, ("s1".to_owned(), "get_state".to_owned()));
        Ok(self)
    }
}

trait Fakes {
    fn fake(&self, name: &str, case: &Case) -> R<PathBuf>;
    fn launch(&self, program: &Path, session: &str) -> R<Launch>;
    fn turn(&self, program: &Path, logs: &str) -> R<Turn>;
    async fn attempt(&self, id: &str) -> R<AttemptRef>;
}

impl Fakes for World {
    /// A fake `pi` replaying `case`; every invocation records its argv.
    fn fake(&self, name: &str, case: &Case) -> R<PathBuf> {
        let d = self.dir.path().join(name);
        std::fs::create_dir_all(&d)?;
        let frames: String = case.frames.iter().map(|f| format!("{f}\n")).collect();
        std::fs::write(d.join("frames.jsonl"), frames)?;
        let commands: serde_json::Map<String, Value> = case
            .commands
            .iter()
            .map(|(id, kind)| (id.clone(), json!(kind)))
            .collect();
        std::fs::write(d.join("commands.json"), serde_json::to_vec(&commands)?)?;
        let script = d.join("pi");
        std::fs::write(
            &script,
            format!(
                "#!/bin/bash\nd='{d}'\nprintf '%s\\n' \"$@\" > \"$d/argv.$$\"\necho $$ > \"$d/pid\"\n\
                 exec python3 '{FAKE}' \"$d/frames.jsonl\" \"$d/commands.json\" \"$d/log\" -- \"$@\"\n",
                d = d.display()
            ),
        )?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        Ok(script)
    }

    fn launch(&self, program: &Path, session: &str) -> R<Launch> {
        Ok(Launch {
            program: program.to_path_buf(),
            help: read(format!("{FIXTURES}/help.txt"))?,
            cwd: self.dir.path().to_path_buf(),
            session_dir: self.dir.path().join(".hoh/pi-sessions").join(session),
            session_id: format!("01a1-hoh-{session}"),
            resume: false,
            provider: None,
            model: None,
            settled_event: true,
        })
    }

    fn turn(&self, program: &Path, logs: &str) -> R<Turn> {
        Ok(Turn {
            launch: self.launch(program, "S1")?,
            prompt: "Reply with exactly HOH_SAMPLE_OK and nothing else.".to_owned(),
            input_id: None,
            logs: self.dir.path().join(logs),
            escalation: Escalation {
                native_grace: Duration::from_secs(2),
                term_grace: Duration::from_secs(1),
                ..Turn::default_escalation()
            },
            deadline: None,
            clock: Arc::new(SystemClock),
            events: None,
        })
    }

    async fn attempt(&self, id: &str) -> R<AttemptRef> {
        self.attempt_in("pi", id, "S1", "1.5.0").await
    }
}

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

fn received(fake: &Path) -> R<Vec<Value>> {
    Ok(
        read(fake.parent().ok_or("no dir")?.join("log/received.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
    )
}

fn sent(fake: &Path) -> R<Vec<Value>> {
    Ok(read(fake.parent().ok_or("no dir")?.join("log/sent.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
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

async fn run(world: &World, attempt: &AttemptRef, turn: &Turn) -> R<Outcome> {
    Ok(pi::run(&world.store, attempt, turn, &CancelLatch::new()).await?)
}

fn value_of(value: Value, path: &[&str]) -> Value {
    path.iter().fold(value, |v, k| v[*k].clone())
}

#[tokio::test]
async fn test_026_01_framing_is_exact_and_agent_start_comes_first() -> R {
    let world = World::new()?;
    let mut case = Case::load("rpc-prompt")?;
    // Separators, Chinese and an emoji inside JSON strings; agent_start before the prompt's response; one record in
    // three writes, two in one write; 10 MiB of stderr.
    let tricky = |text: &str| json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": text}});
    let prompt_answer = case.response("p1")?;
    let agent_start = case.frames.remove(case.position("agent_start")?);
    case.frames.insert(
        prompt_answer,
        json!({"hoh_wait": {"command": "prompt", "count": 1}}),
    );
    case.frames.insert(prompt_answer + 1, agent_start);
    case.frames
        .insert(prompt_answer + 1, json!({"hoh_stderr": 10 << 20}));
    let turn_start = case.position("turn_start")?;
    let pair = [
        case.frames.remove(turn_start),
        case.frames.remove(turn_start),
    ];
    case.frames.insert(turn_start, json!({"hoh_batch": pair}));
    let end = case.position("agent_end")?;
    case.frames
        .insert(end, tricky("line\u{2028}separator 中文 😀"));
    case.frames
        .insert(end + 1, tricky("paragraph\u{2029}separator"));
    let end = case.position("agent_end")?;
    let agent_end = case.frames.remove(end);
    case.frames
        .insert(end, json!({"hoh_split": [agent_end, 3]}));
    let fake = world.fake("framing", &case)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    turn.events = Some(tx);
    let outcome =
        tokio::time::timeout(Duration::from_secs(30), run(&world, &attempt, &turn)).await??;
    assert_eq!(
        (outcome.state, outcome.settled_by),
        (AttemptState::Settled, Some(SettledBy::AgentSettled))
    );
    let mut parsed = Vec::new();
    while let Ok(record) = rx.try_recv() {
        parsed.push(record);
    }
    // Frame for frame what the fake wrote, separators kept inside their frames.
    assert_eq!(parsed, sent(&fake)?);
    assert_eq!(outcome.parse_errors, 0);
    let deltas: Vec<String> = parsed
        .iter()
        .filter_map(|r| {
            r["assistantMessageEvent"]["delta"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert!(
        deltas.contains(&"line\u{2028}separator 中文 😀".to_owned()),
        "{deltas:?}"
    );
    assert!(deltas.contains(&"paragraph\u{2029}separator".to_owned()));
    // The prompt's own records start with agent_start, which came before its response.
    let after_state = parsed
        .iter()
        .position(|r| r["type"] == "response" && r["command"] == "get_state")
        .ok_or("no get_state answer")?;
    let first = parsed[after_state + 1..]
        .iter()
        .find(|r| r["type"] != "response")
        .ok_or("no event")?;
    assert_eq!(first["type"], "agent_start");
    assert_eq!(events(&world, "adapter.agent_start")?.len(), 1);
    assert_eq!(std::fs::metadata(&outcome.stderr)?.len(), 10 << 20);
    assert_eq!(outcome.reply.as_deref(), Some("HOH_SAMPLE_OK"));
    Ok(())
}

/// Run `case` with a frozen clock; when the fake holds at `hold`, read the attempt's state, then let it go on.
async fn settle(
    world: &World,
    name: &str,
    case: &Case,
    settled_event: bool,
    hold: Option<&Path>,
) -> R<(Outcome, Option<AttemptState>, usize)> {
    let fake = world.fake(name, case)?;
    let attempt = world.attempt_in("pi", name, name, "1.5.0").await?;
    let mut turn = world.turn(&fake, &format!("logs/{name}"))?;
    turn.launch.settled_event = settled_event;
    let clock = Arc::new(FrozenClock::default());
    turn.clock = clock.clone();
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    turn.events = Some(tx);
    let latch = CancelLatch::new();
    let running = pi::run(&world.store, &attempt, &turn, &latch);
    let watch = async {
        let mut seen = None;
        if let Some(hold) = hold {
            while let Some(record) = rx.recv().await {
                if record["type"] == "agent_end" {
                    // Read the state while the fake waits right after this first agent_end.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    seen = world.store.reader()?.attempt(name)?.map(|a| a.state);
                    std::fs::write(hold, b"")?;
                    break;
                }
            }
        }
        Ok::<Option<AttemptState>, Box<dyn std::error::Error>>(seen)
    };
    let (outcome, seen) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(running, watch)
    })
    .await?;
    Ok((outcome?, seen?, clock.timers.load(Ordering::SeqCst)))
}

#[tokio::test]
async fn test_026_02_the_end_is_the_disposition_agent_settled_or_the_state_never_a_timer() -> R {
    let world = World::new()?;
    // (a) handled: no run started, so nothing to wait for.
    let mut a = Case::load("rpc-prompt")?;
    let answer = a.response("p1")?;
    a.frames[answer]["data"]["disposition"] = json!("handled");
    a.frames.truncate(answer + 1);
    let (outcome, _, timers) = settle(&world, "a", &a, true, None).await?;
    assert_eq!(
        (outcome.state, outcome.settled_by, timers),
        (AttemptState::Settled, Some(SettledBy::Handled), 0)
    );

    // (b) a retry and (c) a compaction after the first agent_end: still RUNNING then, SETTLED on agent_settled.
    for (name, between) in [
        (
            "b",
            vec![
                json!({"type": "auto_retry_start", "attempt": 1, "maxAttempts": 3, "delayMs": 2000, "errorMessage": "529"}),
            ],
        ),
        (
            "c",
            vec![
                json!({"type": "compaction_start", "reason": "overflow"}),
                json!({"type": "compaction_end", "reason": "overflow"}),
            ],
        ),
    ] {
        let hold = world.dir.path().join(format!("go-{name}"));
        let mut case = Case::load("rpc-prompt")?;
        let end = case.position("agent_end")?;
        case.frames[end]["willRetry"] = json!(name == "b");
        let settled = case.frames.remove(case.position("agent_settled")?);
        let mut tail = vec![json!({"hoh_hold": hold.display().to_string()})];
        tail.extend(between);
        tail.push(json!({"type": "agent_start"}));
        tail.push(json!({"type": "agent_end", "messages": [], "willRetry": false}));
        tail.push(settled);
        case.frames.splice(end + 1..end + 1, tail);
        let (outcome, seen, timers) = settle(&world, name, &case, true, Some(&hold)).await?;
        assert_eq!(
            seen,
            Some(AttemptState::Running),
            "{name}: after the first agent_end"
        );
        assert_eq!(
            (outcome.state, outcome.settled_by, timers),
            (AttemptState::Settled, Some(SettledBy::AgentSettled), 0),
            "{name}"
        );
    }

    // (d) a version without agent_settled: SETTLED only once a state query says idle; each sign of work counts.
    for (name, busy_field, busy_value) in [
        ("d-streaming", "isStreaming", json!(true)),
        ("d-compacting", "isCompacting", json!(true)),
        ("d-pending", "pendingMessageCount", json!(1)),
    ] {
        let hold = world.dir.path().join(format!("go-{name}"));
        let mut d = Case::load("rpc-prompt")?;
        let state = d.frames[d.response("s1")?].clone();
        let busy = {
            let mut s = state.clone();
            s["id"] = json!("s2");
            s["data"][busy_field] = busy_value;
            s
        };
        let idle = {
            let mut s = state.clone();
            s["id"] = json!("s3");
            s
        };
        d.frames.retain(|f| f["type"] != "agent_settled");
        let end = d.position("agent_end")?;
        d.frames.splice(
            end + 1..end + 1,
            [
                json!({"hoh_hold": hold.display().to_string()}),
                busy,
                json!({"type": "agent_start"}),
                json!({"type": "agent_end", "messages": [], "willRetry": false}),
                idle,
            ],
        );
        d.commands.push(("s2".to_owned(), "get_state".to_owned()));
        d.commands.push(("s3".to_owned(), "get_state".to_owned()));
        let (outcome, seen, timers) = settle(&world, name, &d, false, Some(&hold)).await?;
        assert_eq!(seen, Some(AttemptState::Running), "{name}");
        assert_eq!(
            (
                outcome.state,
                outcome.settled_by,
                outcome.state_queries,
                timers
            ),
            (AttemptState::Settled, Some(SettledBy::State), 2, 0),
            "{name}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_026_03_the_session_survives_a_kill_and_dialogs_are_cancelled() -> R {
    let world = World::new()?;
    // A turn to agent_settled; then kill -9 the fake while it waits to be closed.
    let mut first = Case::load("rpc-prompt")?;
    let hold = world.dir.path().join("never");
    first
        .frames
        .push(json!({"hoh_hold": hold.display().to_string()}));
    let fake = world.fake("first", &first)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    turn.events = Some(tx);
    let latch = CancelLatch::new();
    let running = pi::run(&world.store, &attempt, &turn, &latch);
    let killer = async {
        while let Some(record) = rx.recv().await {
            if record["type"] == "agent_settled" {
                let pid = read(fake.parent().ok_or("no dir")?.join("pid"))?;
                std::process::Command::new("kill")
                    .args(["-9", pid.trim()])
                    .status()?;
                break;
            }
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (outcome, killed) = tokio::join!(running, killer);
    killed?;
    assert_eq!(outcome?.state, AttemptState::Settled);
    let before = world.store.reader()?.session("S1")?.ok_or("no session")?;
    assert_eq!(count(&world, "SELECT count(*) FROM sessions")?, 1);
    assert_eq!(
        world.store.reader()?.attempt("A1")?.map(|a| a.state),
        Some(AttemptState::Settled)
    );

    // resume(): the same session dir and id; the dialogs are cancelled, the status request is only data.
    let mut second = Case::load("rpc-resume")?;
    let start = second.position("agent_start")?;
    second.frames.splice(
        start + 1..start + 1,
        [
            json!({"type": "extension_ui_request", "id": "ui-confirm", "method": "confirm", "title": "Run it?", "message": "x"}),
            json!({"type": "extension_ui_request", "id": "ui-select", "method": "select", "title": "Pick", "options": ["Allow", "Block"]}),
            json!({"type": "extension_ui_request", "id": "ui-status", "method": "setStatus", "statusKey": "ext", "statusText": "busy"}),
        ],
    );
    let fake2 = world.fake("second", &second)?;
    let attempt = world.attempt("A2").await?;
    let mut turn = world.turn(&fake2, "logs/A2")?;
    turn.launch.resume = true;
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(outcome.state, AttemptState::Settled);
    let both: Vec<Vec<String>> = argvs(&fake)?.into_iter().chain(argvs(&fake2)?).collect();
    assert_eq!(both.len(), 2);
    let flag = |a: &[String], f: &str| {
        a.iter()
            .position(|x| x == f)
            .and_then(|i| a.get(i + 1))
            .cloned()
    };
    for a in &both {
        assert!(a.windows(2).any(|w| w == ["--mode", "rpc"]), "{a:?}");
        assert!(!a.iter().any(|x| x == "--no-session"), "{a:?}");
    }
    assert_eq!(
        flag(&both[0], "--session-dir"),
        flag(&both[1], "--session-dir")
    );
    assert_eq!(
        flag(&both[0], "--session-id"),
        flag(&both[1], "--session-id")
    );
    // CR5R-01: the resumed session is the same row, unchanged, and no other row was made.
    let after = world.store.reader()?.session("S1")?.ok_or("no session")?;
    assert_eq!(after, before);
    assert_eq!(count(&world, "SELECT count(*) FROM sessions")?, 1);
    assert_eq!(
        world.store.reader()?.attempt("A2")?.map(|a| a.state),
        Some(AttemptState::Settled)
    );
    assert_eq!(outcome.native_session, before.native_id);
    let answers: Vec<Value> = received(&fake2)?
        .into_iter()
        .filter(|m| m["type"] == "extension_ui_response")
        .collect();
    assert_eq!(answers.len(), 2, "{answers:?}");
    for (answer, id) in answers.iter().zip(["ui-confirm", "ui-select"]) {
        assert_eq!(
            answer,
            &json!({"type": "extension_ui_response", "id": id, "cancelled": true})
        );
    }
    let dialogs = events(&world, "adapter.ui_request")?;
    assert_eq!(
        dialogs
            .iter()
            .map(|d| (
                value_of(d.clone(), &["method"]),
                value_of(d.clone(), &["decision"])
            ))
            .collect::<Vec<_>>(),
        [
            (json!("confirm"), json!("cancelled")),
            (json!("select"), json!("cancelled"))
        ]
    );
    let data = events(&world, "adapter.ui_data")?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["method"], "setStatus");
    Ok(())
}

/// A resumed session whose file is gone is not silently replaced by a new one: FAILED, nothing started.
#[tokio::test]
async fn a_missing_session_is_not_replaced_by_a_new_one() -> R {
    let world = World::new()?;
    let fake = world.fake("gone", &Case::load("rpc-resume")?)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.launch.resume = true;
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("SESSION_NOT_FOUND"))
    );
    assert!(argvs(&fake)?.is_empty(), "no process may start");
    Ok(())
}

/// A refused prompt is classified and its input REJECTED without waiting for anything; an accepted one is
/// DELIVERED, then APPLIED when its run starts.
#[tokio::test]
async fn a_refused_prompt_fails_at_once_and_rejects_its_input() -> R {
    let world = World::new()?;
    let enqueue = |id: &str| NewInput {
        input_id: id.to_owned(),
        run_id: "R".to_owned(),
        kind: "ASK".to_owned(),
        target_role: Some("developer".to_owned()),
        payload: json!({"text": "Reply"}),
    };
    let no_key = Case::load("rpc-no-provider")?;
    let mut no_model = no_key.clone();
    let at = no_model.response("p1")?;
    no_model.frames[at]["error"] = json!("Model not found: x/y");
    let mut other = no_key.clone();
    other.frames[at]["error"] = json!("Something else broke");
    for (i, (case, reason)) in [
        (no_key, "BLOCKED_AUTH"),
        (no_model, "MODEL_UNAVAILABLE"),
        (other, "PERMANENT_EXTERNAL"),
    ]
    .into_iter()
    .enumerate()
    {
        let fake = world.fake(&format!("refused-{i}"), &case)?;
        let attempt = world
            .attempt_in("pi", &format!("A{i}"), &format!("S{i}"), "1.5.0")
            .await?;
        world.store.enqueue_input(enqueue(&format!("I{i}"))).await?;
        let mut turn = world.turn(&fake, &format!("logs/A{i}"))?;
        turn.input_id = Some(format!("I{i}"));
        let clock = Arc::new(FrozenClock::default());
        turn.clock = clock.clone();
        let outcome = run(&world, &attempt, &turn).await?;
        assert_eq!(
            (outcome.state, outcome.failure.as_deref()),
            (AttemptState::Failed, Some(reason))
        );
        assert_eq!(
            world
                .store
                .reader()?
                .input(&format!("I{i}"))?
                .map(|r| r.state),
            Some(InputState::Rejected)
        );
        assert_eq!(clock.timers.load(Ordering::SeqCst), 0);
    }
    let fake = world.fake("accepted", &Case::load("rpc-prompt")?)?;
    let attempt = world.attempt_in("pi", "A9", "S9", "1.5.0").await?;
    world.store.enqueue_input(enqueue("I9")).await?;
    let mut turn = world.turn(&fake, "logs/A9")?;
    turn.input_id = Some("I9".to_owned());
    assert_eq!(
        run(&world, &attempt, &turn).await?.state,
        AttemptState::Settled
    );
    assert_eq!(
        world.store.reader()?.input("I9")?.map(|r| r.state),
        Some(InputState::Applied)
    );
    Ok(())
}

/// A cancel sends abort; Pi's aborted agent_settled ends the attempt CANCELLED. An abort Pi never answers is followed
/// by SIGTERM and SIGKILL to the group after the grace.
#[tokio::test]
async fn a_cancel_aborts_and_an_ignored_abort_stops_the_group() -> R {
    let world = World::new()?;
    for (name, answered) in [("abort", true), ("deaf", false)] {
        let mut case = Case::load("rpc-abort")?.with_state()?;
        let start = case.position("agent_start")?;
        case.frames.insert(
            start + 1,
            json!({"hoh_wait": {"command": "abort", "count": 1}}),
        );
        if !answered {
            case.frames.truncate(start + 2);
            case.frames
                .push(json!({"hoh_hold": "/nonexistent/hoh-never"}));
        }
        let fake = world.fake(name, &case)?;
        let attempt = world.attempt_in("pi", name, name, "1.5.0").await?;
        let mut turn = world.turn(&fake, &format!("logs/{name}"))?;
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        turn.events = Some(tx);
        let latch = CancelLatch::new();
        let running = pi::run(&world.store, &attempt, &turn, &latch);
        let trigger = async {
            while let Some(record) = rx.recv().await {
                if record["type"] == "agent_start" {
                    return Ok::<CancelReceipt, Box<dyn std::error::Error>>(latch.request());
                }
            }
            Err("no agent_start".into())
        };
        let (outcome, receipt) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(running, trigger)
        })
        .await?;
        let (outcome, receipt) = (outcome?, receipt?);
        assert_eq!(receipt, CancelReceipt::Accepted);
        assert_eq!(outcome.state, AttemptState::Cancelled, "{name}");
        assert!(
            received(&fake)?.iter().any(|m| m["type"] == "abort"),
            "{name}"
        );
        assert!(
            group_members(outcome.pid.ok_or("no pid")?).is_empty(),
            "{name}"
        );
        if !answered {
            assert!(!outcome.steps.is_empty(), "the group was stopped");
        }
    }
    Ok(())
}

/// The stream ends before Pi settled: FAILED, or UNKNOWN_EFFECT once a tool had started.
#[tokio::test]
async fn a_stream_cut_short_is_not_settled() -> R {
    let world = World::new()?;
    for (name, tool) in [("cut", false), ("cut-tool", true)] {
        let mut case = Case::load("rpc-prompt")?;
        let start = case.position("agent_start")?;
        case.frames.truncate(start + 1);
        if tool {
            case.frames.push(json!({"type": "tool_execution_start", "toolCallId": "t1", "toolName": "bash", "args": {"command": "touch x"}}));
        }
        case.frames.push(json!({"hoh_exit": 0}));
        let fake = world.fake(name, &case)?;
        let attempt = world.attempt_in("pi", name, name, "1.5.0").await?;
        let outcome = run(
            &world,
            &attempt,
            &world.turn(&fake, &format!("logs/{name}"))?,
        )
        .await?;
        let expected = if tool {
            AttemptState::UnknownEffect
        } else {
            AttemptState::Failed
        };
        assert_eq!(
            (outcome.state, outcome.failure.as_deref()),
            (expected, Some("PROTOCOL_INCOMPLETE")),
            "{name}"
        );
    }
    Ok(())
}

/// A frame over 1 MiB without LF fails the attempt with its diagnosis (length, prefix sha256, first 4 KiB).
#[tokio::test]
async fn a_frame_too_large_fails_with_its_diagnosis() -> R {
    let world = World::new()?;
    let mut case = Case::load("rpc-prompt")?;
    let start = case.position("agent_start")?;
    case.frames.insert(
        start + 1,
        json!({"type": "message_update", "pad": "x".repeat(1 << 20)}),
    );
    let fake = world.fake("huge", &case)?;
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
    Ok(())
}

/// The deadline aborts like a cancel and fails the turn as DEADLINE_EXCEEDED.
#[tokio::test]
async fn a_deadline_aborts_and_fails_the_turn() -> R {
    let world = World::new()?;
    let mut case = Case::load("rpc-abort")?.with_state()?;
    let start = case.position("agent_start")?;
    case.frames.insert(
        start + 1,
        json!({"hoh_wait": {"command": "abort", "count": 1}}),
    );
    let fake = world.fake("slow", &case)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.deadline = Some(Duration::from_millis(500));
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("DEADLINE_EXCEEDED"))
    );
    assert!(received(&fake)?.iter().any(|m| m["type"] == "abort"));
    Ok(())
}

/// A pi that reports another session than the one asked for is not taken at its word: FAILED, no prompt sent.
#[tokio::test]
async fn another_session_than_the_one_asked_for_fails() -> R {
    let world = World::new()?;
    let mut case = Case::load("rpc-prompt")?;
    case.frames.insert(0, json!({"hoh_keep_session": true}));
    let fake = world.fake("other-session", &case)?;
    let attempt = world.attempt("A1").await?;
    let outcome = run(&world, &attempt, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("SESSION_MISMATCH"))
    );
    assert!(
        !received(&fake)?.iter().any(|m| m["type"] == "prompt"),
        "no prompt may be sent"
    );
    Ok(())
}

fn ask(id: &str) -> NewInput {
    NewInput {
        input_id: id.to_owned(),
        run_id: "R".to_owned(),
        kind: "ASK".to_owned(),
        target_role: Some("developer".to_owned()),
        payload: json!({"text": "Reply"}),
    }
}

/// agent_start before the prompt's response (AC-026-01's order) with a real input: DELIVERED then APPLIED, in that
/// order, and the attempt ends (M1T05R-01).
#[tokio::test]
async fn a_run_that_starts_before_its_answer_still_applies_its_input() -> R {
    let world = World::new()?;
    let mut case = Case::load("rpc-prompt")?;
    let answer = case.response("p1")?;
    let start = case.frames.remove(case.position("agent_start")?);
    case.frames.insert(
        answer,
        json!({"hoh_wait": {"command": "prompt", "count": 1}}),
    );
    case.frames.insert(answer + 1, start);
    let fake = world.fake("early-start", &case)?;
    let attempt = world.attempt("A1").await?;
    world.store.enqueue_input(ask("I1")).await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.input_id = Some("I1".to_owned());
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(outcome.state, AttemptState::Settled);
    assert_eq!(
        world.store.reader()?.input("I1")?.map(|r| r.state),
        Some(InputState::Applied)
    );
    assert_eq!(
        world.store.reader()?.attempt("A1")?.map(|a| a.state),
        Some(AttemptState::Settled)
    );
    let moves: Vec<String> = events(&world, "input.moved")?
        .iter()
        .map(|e| e["to"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(moves, ["DELIVERED", "APPLIED"]);
    Ok(())
}

/// Without agent_settled, a busy answer after the last agent_end is checked again when that work ends, e.g. a
/// compaction, with no further agent_end (M1T05R-02).
#[tokio::test]
async fn a_busy_answer_is_checked_again_when_the_work_ends() -> R {
    let world = World::new()?;
    let mut case = Case::load("rpc-prompt")?;
    let state = case.frames[case.response("s1")?].clone();
    let mut busy = state.clone();
    busy["id"] = json!("s2");
    busy["data"]["isCompacting"] = json!(true);
    let mut idle = state;
    idle["id"] = json!("s3");
    case.frames.retain(|f| f["type"] != "agent_settled");
    let end = case.position("agent_end")?;
    case.frames.splice(
        end + 1..end + 1,
        [
            busy,
            json!({"type": "compaction_start", "reason": "threshold"}),
            json!({"type": "compaction_end", "reason": "threshold", "willRetry": false}),
            idle,
        ],
    );
    case.commands
        .push(("s2".to_owned(), "get_state".to_owned()));
    case.commands
        .push(("s3".to_owned(), "get_state".to_owned()));
    let fake = world.fake("busy-then-done", &case)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.launch.settled_event = false;
    let clock = Arc::new(FrozenClock::default());
    turn.clock = clock.clone();
    let outcome =
        tokio::time::timeout(Duration::from_secs(20), run(&world, &attempt, &turn)).await??;
    assert_eq!(
        (outcome.state, outcome.settled_by, outcome.state_queries),
        (AttemptState::Settled, Some(SettledBy::State), 2)
    );
    assert_eq!(clock.timers.load(Ordering::SeqCst), 0);
    Ok(())
}

/// A session file pi does not take (empty, or of another cwd), and a pi that opens another file for the id: the
/// session was not resumed, so no prompt goes out (M1T05R-03, M1T05R-10/11: pi chooses, HoH checks the choice).
#[tokio::test]
async fn only_the_sessions_own_file_is_resumed() -> R {
    let world = World::new()?;
    let resume = Case::load("rpc-resume")?;
    let header = |cwd: &Path, id: &str| json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-11-04T00:00:00.000Z", "cwd": cwd});
    for (name, content) in [
        ("empty", None),
        ("other-cwd", Some(Path::new("/elsewhere"))),
    ] {
        let fake = world.fake(name, &resume)?;
        let attempt = world.attempt_in("pi", name, name, "1.5.0").await?;
        let mut turn = world.turn(&fake, &format!("logs/{name}"))?;
        turn.launch.resume = true;
        turn.launch.session_dir = world.dir.path().join(".hoh/pi-sessions").join(name);
        std::fs::create_dir_all(&turn.launch.session_dir)?;
        let file = turn.launch.session_dir.join(format!(
            "2026-11-04T00-00-00-000Z_{}.jsonl",
            turn.launch.session_id
        ));
        let body = content.map_or(String::new(), |cwd| {
            format!("{}\n", header(cwd, &turn.launch.session_id))
        });
        std::fs::write(&file, body)?;
        // pi decides which file is the session; it does not take these, so it did not resume the session.
        let outcome = run(&world, &attempt, &turn).await?;
        assert_eq!(
            (outcome.state, outcome.failure.as_deref()),
            (AttemptState::Failed, Some("SESSION_MISMATCH")),
            "{name}"
        );
        assert!(
            !received(&fake)?.iter().any(|m| m["type"] == "prompt"),
            "{name}: no prompt may be sent"
        );
    }
    // The session's own file exists, but this pi opens a new one for the id.
    let mut fresh = resume.clone();
    fresh.frames.insert(0, json!({"hoh_new_file": true}));
    let fake = world.fake("fresh", &fresh)?;
    let attempt = world.attempt_in("pi", "fresh", "fresh", "1.5.0").await?;
    let mut turn = world.turn(&fake, "logs/fresh")?;
    turn.launch.resume = true;
    std::fs::create_dir_all(&turn.launch.session_dir)?;
    std::fs::write(
        turn.launch.session_dir.join(format!(
            "2026-11-03T00-00-00-000Z_{}.jsonl",
            turn.launch.session_id
        )),
        format!(
            "{}\n",
            header(&world.dir.path().canonicalize()?, &turn.launch.session_id)
        ),
    )?;
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("SESSION_MISMATCH"))
    );
    assert!(
        !received(&fake)?.iter().any(|m| m["type"] == "prompt"),
        "no prompt may be sent"
    );
    Ok(())
}

/// A provider error after the prompt was taken fails the turn, classified as a refused prompt is (REQ-026, CR-003):
/// only the recorded `No API key found` / `Model not found` texts are BLOCKED_AUTH / MODEL_UNAVAILABLE; any other text,
/// pi's own formatted ones included, and one pi gave up retrying, is PERMANENT_EXTERNAL with the text kept in
/// `adapter.turn_failed` (M1T05R-04, M1T05R-17..20). A retry that worked settles the turn.
#[tokio::test]
async fn a_provider_failure_after_acceptance_fails_the_turn() -> R {
    let world = World::new()?;
    let failed_message = |text: &str| {
        json!({"type": "message_end", "message": {"role": "assistant", "content": [], "stopReason": "error",
               "errorMessage": text}})
    };
    let retry = |error: &str| json!({"type": "auto_retry_start", "attempt": 1, "maxAttempts": 1, "delayMs": 1, "errorMessage": error});
    let gave_up = |error: &str| json!({"type": "auto_retry_end", "success": false, "attempt": 1, "finalError": error});
    let failed = |class: &'static str| (AttemptState::Failed, Some(class));
    for (name, error, retried, expect) in [
        (
            "key",
            "No API key found for the selected model.",
            None,
            failed("BLOCKED_AUTH"),
        ),
        (
            "model",
            "Model not found: vendor/model-503",
            None,
            failed("MODEL_UNAVAILABLE"),
        ),
        (
            "other",
            "Request was malformed",
            None,
            failed("PERMANENT_EXTERNAL"),
        ),
        (
            "limit",
            "429 rate limit exceeded",
            None,
            failed("PERMANENT_EXTERNAL"),
        ),
        (
            "formatted",
            r#"OpenAI API error (429): {"message":"Too Many Requests"}"#,
            None,
            failed("PERMANENT_EXTERNAL"),
        ),
        (
            "formatted-auth",
            r#"Azure OpenAI API error (401): {"message":"Your token has expired"}"#,
            None,
            failed("PERMANENT_EXTERNAL"),
        ),
        (
            "gave-up",
            "503 Service Unavailable",
            Some(false),
            failed("PERMANENT_EXTERNAL"),
        ),
        (
            "retried",
            "529 overloaded",
            Some(true),
            (AttemptState::Settled, None),
        ),
    ] {
        let mut case = Case::load("rpc-prompt")?;
        let last = case
            .frames
            .iter()
            .rposition(|f| f["type"] == "message_end" && f["message"]["role"] == "assistant")
            .ok_or("no assistant message")?;
        let good = case.frames[last].clone();
        case.frames[last] = failed_message(error);
        match retried {
            // pi retried and the retry worked.
            Some(true) => {
                case.frames.splice(last + 1..last + 1, [retry(error), good]);
            }
            // pi retried, failed again and gave up.
            Some(false) => {
                case.frames.splice(
                    last + 1..last + 1,
                    [retry(error), failed_message(error), gave_up(error)],
                );
            }
            None => {}
        }
        let fake = world.fake(name, &case)?;
        let attempt = world.attempt_in("pi", name, name, "1.5.0").await?;
        let outcome = run(
            &world,
            &attempt,
            &world.turn(&fake, &format!("logs/{name}"))?,
        )
        .await?;
        assert_eq!(
            (outcome.state, outcome.failure.as_deref()),
            expect,
            "{name}"
        );
        let row = world.store.reader()?.attempt(name)?.ok_or("no attempt")?;
        assert_eq!(
            (row.state, row.failure_reason.as_deref()),
            expect,
            "{name} in the store"
        );
        let kept: Vec<Value> = world
            .store
            .reader()?
            .events("R")?
            .into_iter()
            .filter(|(k, p)| k == "adapter.turn_failed" && p["attempt_id"] == name)
            .map(|(_, p)| p)
            .collect();
        let want: Vec<Value> = match expect {
            (AttemptState::Failed, Some(class)) => {
                vec![json!({"attempt_id": name, "reason": class, "errorMessage": error})]
            }
            _ => Vec::new(),
        };
        assert_eq!(kept, want, "{name}: the text is kept");
    }
    Ok(())
}

/// An input someone else has moved on before Pi took the prompt is recorded and does not stop the turn.
#[tokio::test]
async fn an_input_moved_elsewhere_does_not_stop_the_pi_turn() -> R {
    let world = World::new()?;
    let fake = world.fake("moved", &Case::load("rpc-prompt")?)?;
    let attempt = world.attempt("A1").await?;
    world.store.enqueue_input(ask("I1")).await?;
    world
        .store
        .move_input(hoh_core::persistence::InputMove {
            input_id: "I1".to_owned(),
            from: InputState::Queued,
            to: InputState::Rejected,
            reason: Some("withdrawn".to_owned()),
        })
        .await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.input_id = Some("I1".to_owned());
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(outcome.state, AttemptState::Settled);
    assert_eq!(
        world.store.reader()?.input("I1")?.map(|r| r.state),
        Some(InputState::Rejected)
    );
    assert!(
        events(&world, "adapter.diagnostic")?
            .iter()
            .any(|d| d["reason"] == "input not moved")
    );
    Ok(())
}

/// The session file found before the start is gone and pi names a new path it has not written yet: both sides do
/// not resolve to the same file, so no prompt goes out (M1T05R-06).
#[tokio::test]
async fn a_session_file_that_vanished_is_not_the_session() -> R {
    let world = World::new()?;
    let mut case = Case::load("rpc-resume")?;
    case.frames.insert(0, json!({"hoh_lose_session": true}));
    let fake = world.fake("lost", &case)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.launch.resume = true;
    std::fs::create_dir_all(&turn.launch.session_dir)?;
    let header = json!({"type": "session", "version": 3, "id": turn.launch.session_id,
                        "timestamp": "2026-11-04T00:00:00.000Z", "cwd": world.dir.path().canonicalize()?});
    std::fs::write(
        turn.launch.session_dir.join(format!(
            "2026-11-03T00-00-00-000Z_{}.jsonl",
            turn.launch.session_id
        )),
        format!("{header}\n"),
    )?;
    let outcome = run(&world, &attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("SESSION_MISMATCH"))
    );
    assert!(
        !received(&fake)?.iter().any(|m| m["type"] == "prompt"),
        "no prompt may be sent"
    );
    Ok(())
}

/// A `*.jsonl` entry that is not a regular file would block pi's own session lookup: nothing starts (M1T05R-12).
#[tokio::test]
async fn a_session_directory_that_would_block_pi_is_refused() -> R {
    let world = World::new()?;
    let fake = world.fake("fifo", &Case::load("rpc-resume")?)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.launch.resume = true;
    std::fs::create_dir_all(&turn.launch.session_dir)?;
    let header = json!({"type": "session", "version": 3, "id": turn.launch.session_id,
                        "timestamp": "2026-11-04T00:00:00.000Z", "cwd": world.dir.path().canonicalize()?});
    std::fs::write(
        turn.launch.session_dir.join(format!(
            "2026-11-05T00-00-00-000Z_{}.jsonl",
            turn.launch.session_id
        )),
        format!("{header}\n"),
    )?;
    let fifo = turn
        .launch
        .session_dir
        .join("2026-11-04T00-00-00-000Z_fifo.jsonl");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()?
            .success()
    );
    let outcome =
        tokio::time::timeout(Duration::from_secs(10), run(&world, &attempt, &turn)).await??;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("SESSION_DIR_UNSAFE"))
    );
    assert!(argvs(&fake)?.is_empty(), "no process may start");
    Ok(())
}
