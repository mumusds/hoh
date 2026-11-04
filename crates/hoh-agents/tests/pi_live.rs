//! TEST-026-04 against the installed `pi` (live; model calls). Run only by scripts/product/adapter-live.sh, which sets
//! HOH_LIVE_OUT and turns the verdict written there into the check's evidence. Three turns in a temporary Git
//! repository on one session (`--session-dir` under the store's `.hoh/`, a session id HoH chose): a reply; after that
//! process has ended, a new one on the same session repeating the token; then `sleep 120`, cancelled once it runs.
//! A refused prompt for want of a provider key (BLOCKED_AUTH) is BLOCKED, never PASS. The installed CLI (version,
//! launcher and package hashes, help hash, from `probe::probe`) and the Node version are recorded in pi-install.json.
//!
//! The cancel is sent only once this turn's bash tool has started and a `sleep 120` descending from this pi is alive;
//! the turn fails if that never happens, or unless pi's group, the tool's process and group (zombies included) and
//! every process working in the repository are all seen gone within 16 s of the cancel.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;
mod live;

use common::{R, World};
use hoh_agents::clock::SystemClock;
use hoh_agents::compat::Agent;
use hoh_agents::pi::{self, Launch, Outcome, Turn};
use hoh_agents::probe::{self, ProbeEnv};
use hoh_agents::process::group_members;
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::CancelLatch;
use live::{TOKEN, clear_by, hex, left_of, processes_in, stat_of, tool_sleeps, write_verdict};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[tokio::test]
#[ignore = "live: runs the installed pi through scripts/product/adapter-live.sh"]
async fn pi_live_turns_resume_and_cancel() -> R {
    let out = PathBuf::from(std::env::var_os("HOH_LIVE_OUT").ok_or("HOH_LIVE_OUT is not set")?);
    let program = PathBuf::from(std::env::var_os("HOH_LIVE_PI").ok_or("HOH_LIVE_PI is not set")?);
    let none = json!({});
    let entry = probe::probe(
        Agent::Pi,
        &ProbeEnv {
            path: std::env::var_os("PATH").ok_or("no PATH")?,
            fixture_root: None,
            timeout: Duration::from_secs(120),
        },
    )?;
    let version = entry
        .cli_version
        .split_whitespace()
        .last()
        .ok_or("no version")?
        .to_owned();
    let node = String::from_utf8(Command::new("node").arg("--version").output()?.stdout)?;
    let help = String::from_utf8(Command::new(&program).arg("--help").output()?.stdout)?;
    std::fs::write(
        out.join("pi-install.json"),
        serde_json::to_vec_pretty(&json!({
            "probe": entry, "version": version, "node": node.trim(), "help_sha256": hex(help.as_bytes()),
        }))?,
    )?;
    let world = World::new()?;
    let repo = world.dir.path().join("repo");
    std::fs::create_dir_all(&repo)?;
    if !Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()?
        .success()
    {
        return Err("git init failed".into());
    }
    let repo = repo.canonicalize()?;
    let session_id = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_owned();
    let launch = Launch {
        program: program.clone(),
        help,
        cwd: repo.clone(),
        session_dir: world.dir.path().join(".hoh/pi-sessions/S1"),
        session_id: session_id.clone(),
        resume: false,
        provider: None,
        model: None,
        settled_event: true,
    };
    let turn = |prompt: &str, logs: &str, resume: bool| {
        let mut launch = launch.clone();
        launch.resume = resume;
        Turn {
            launch,
            prompt: prompt.to_owned(),
            input_id: None,
            logs: out.join(logs),
            escalation: Turn::default_escalation(),
            deadline: Some(Duration::from_secs(600)),
            clock: Arc::new(SystemClock),
            events: None,
        }
    };
    let session_of = |o: &Outcome| {
        json!({
            "agent": "pi",
            "cli_version": version,
            "session_id": o.native_session.clone().unwrap_or_default(),
            "resolved_model": o.model.clone().unwrap_or_else(|| "unknown".to_owned()),
        })
    };
    let mut sessions = Vec::new();
    let mut problems = Vec::new();

    // 1. A reply.
    let a1 = world.attempt_in("pi", "A1", "S1", &version).await?;
    let first = pi::run(
        &world.store,
        &a1,
        &turn(
            &format!("Reply with exactly {TOKEN} and nothing else. Do not use any tool."),
            "live/pi/A1",
            false,
        ),
        &CancelLatch::new(),
    )
    .await?;
    if first.state != AttemptState::Settled {
        let (status, why) = if first.failure.as_deref() == Some("BLOCKED_AUTH") {
            ("BLOCKED", "BLOCKED_AUTH: pi has no provider key".to_owned())
        } else {
            (
                "FAIL",
                format!("turn 1 ended {:?} ({:?})", first.state, first.failure),
            )
        };
        write_verdict(&out, "pi", status, &why, &sessions, &none)?;
        return if status == "BLOCKED" {
            Ok(())
        } else {
            Err(why.into())
        };
    }
    if !first.reply.as_deref().unwrap_or_default().contains(TOKEN) {
        problems.push(format!("turn 1: reply {:?}", first.reply));
    }
    sessions.push(session_of(&first));

    // 2. That process has ended; a new one on the same session repeats the token.
    let a2 = world.attempt_in("pi", "A2", "S1", &version).await?;
    let second = pi::run(
        &world.store,
        &a2,
        &turn(
            "Repeat the exact token you replied with in the previous turn, and nothing else.",
            "live/pi/A2",
            true,
        ),
        &CancelLatch::new(),
    )
    .await?;
    if second.state != AttemptState::Settled
        || second.native_session != first.native_session
        || !second.reply.as_deref().unwrap_or_default().contains(TOKEN)
    {
        problems.push(format!(
            "turn 2: {:?} {:?} session {:?} (first {:?}) reply {:?}",
            second.state, second.failure, second.native_session, first.native_session, second.reply
        ));
    }
    let row = world.store.reader()?.session("S1")?.ok_or("no session")?;
    if row.native_path.as_deref() != Some(launch.session_dir.to_string_lossy().as_ref()) {
        problems.push(format!("session row native_path {:?}", row.native_path));
    }

    // 3. `sleep 120`, cancelled once its bash tool runs it.
    let a3 = world.attempt_in("pi", "A3", "S3", &version).await?;
    let mut third_turn = turn(
        "Use the bash tool to run `sleep 120` and wait for it to finish, then reply DONE.",
        "live/pi/A3",
        false,
    );
    third_turn.launch.session_dir = world.dir.path().join(".hoh/pi-sessions/S3");
    third_turn.launch.session_id = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_owned();
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    third_turn.events = Some(tx);
    let cancel = CancelLatch::new();
    let running = pi::run(&world.store, &a3, &third_turn, &cancel);
    let store = &world.store;
    let trigger = async {
        let started = Instant::now();
        let mut bash = false;
        let mut tools = Vec::new();
        while started.elapsed() < Duration::from_secs(240) && tools.is_empty() {
            while let Ok(record) = rx.try_recv() {
                bash |= record["type"] == "tool_execution_start" && record["toolName"] == "bash";
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            let pid = store
                .reader()
                .ok()
                .and_then(|r| r.attempt("A3").ok().flatten())
                .and_then(|a| a.pid)
                .and_then(|p| u32::try_from(p).ok());
            if let (true, Some(pid)) = (bash, pid) {
                tools = tool_sleeps(&repo, pid);
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let alive: Vec<u32> = tools
            .iter()
            .map(|(pid, _)| *pid)
            .filter(|pid| stat_of(*pid).is_some_and(|(state, _, _)| state != 'Z'))
            .collect();
        let at = Instant::now();
        let receipt = cancel.request();
        (at, tools, alive, receipt)
    };
    let (third, (at, tools, alive, receipt)) = tokio::join!(running, trigger);
    let third = third?;
    let took = at.elapsed();
    let leader = third.pid.unwrap_or_default();
    let mut left = Vec::new();
    let cleared = clear_by(at, Duration::from_secs(16), || {
        left = group_members(leader);
        left.extend(left_of(&tools));
        left.extend(processes_in(&repo));
        left.is_empty()
    })
    .await;
    if alive.is_empty() {
        problems.push(format!("turn 3: no `sleep 120` of this turn's bash tool was alive at the cancel (seen {tools:?})"));
    }
    if third.state != AttemptState::Cancelled || cleared.is_none() {
        problems.push(format!(
            "turn 3: {:?} ({:?}) after {took:?}; still there 16 s after the cancel: {left:?}",
            third.state, third.failure
        ));
    }
    for (i, o) in [&first, &second, &third].iter().enumerate() {
        if o.args.iter().any(|a| a == "--no-session") {
            problems.push(format!("turn {}: --no-session", i + 1));
        }
    }
    sessions.push(session_of(&third));
    let observations = json!({
        "turn1": {"session": first.native_session, "model": first.model, "settled_by": format!("{:?}", first.settled_by),
                  "reply": first.reply, "args": first.args},
        "turn2": {"session": second.native_session, "settled_by": format!("{:?}", second.settled_by), "reply": second.reply,
                  "args": second.args},
        "turn3": {"pi_pid": third.pid, "tool_processes": tools.iter().map(|(p, g)| json!({"pid": p, "pgid": g})).collect::<Vec<_>>(),
                  "alive_at_cancel": alive, "cancel_receipt": format!("{receipt:?}"), "cancel_to_end_ms": took.as_millis(),
                  "cleared_after_ms": cleared.map(|d| d.as_millis()), "left": left},
    });
    if problems.is_empty() {
        write_verdict(
            &out,
            "pi",
            "PASS",
            &format!(
                "pi {version}: reply, resumed session reply, `sleep 120` cancelled in {took:?}"
            ),
            &sessions,
            &observations,
        )?;
        Ok(())
    } else {
        write_verdict(
            &out,
            "pi",
            "FAIL",
            &problems.join("; "),
            &sessions,
            &observations,
        )?;
        Err(problems.join("; ").into())
    }
}
