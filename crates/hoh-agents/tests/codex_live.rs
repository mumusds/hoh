//! TEST-024-04 against the installed `codex` (live; model calls). Run only by scripts/product/adapter-live.sh, which
//! sets HOH_LIVE_OUT and turns the verdict written there into the check's evidence. Two turns over the App Server in a
//! temporary Git repository with the default graces: a reply with no tool, then `sleep 120` cancelled while it runs.
//! Not logged in (`codex login status` fails, or the first turn is BLOCKED_AUTH) is BLOCKED, never PASS. The
//! installed CLI (version, launcher and binary sha256, help and schema hashes, from `probe::probe`) is recorded in
//! codex-install.json.
//!
//! The cancel is sent only once this turn's commandExecution item for `sleep 120` has started and a `sleep 120`
//! descending from this app-server is alive; the turn fails if that never happens, or unless the app-server's group,
//! the tool's process and group (zombies included) and every process working in the repository are all seen gone
//! within 16 s of the cancel.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;
mod live;

use common::{R, World};
use hoh_agents::codex::{self, Launch, Mode, Outcome, Turn};
use hoh_agents::compat::Agent;
use hoh_agents::probe::{self, ProbeEnv};
use hoh_agents::process::group_members;
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::CancelLatch;
use live::{TOKEN, clear_by, hex, left_of, processes_in, stat_of, tool_sleeps, write_verdict};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn stdout_of(program: &Path, args: &[&str]) -> R<String> {
    Ok(String::from_utf8(
        Command::new(program).args(args).output()?.stdout,
    )?)
}

/// Whether the App Server stream shows a commandExecution item started whose command runs `needle`.
fn command_started(stdout: &[u8], needle: &str) -> bool {
    stdout
        .split(|b| *b == b'\n')
        .filter_map(|l| serde_json::from_slice::<Value>(l).ok())
        .any(|f| {
            f["method"] == "item/started"
                && f["params"]["item"]["type"] == "commandExecution"
                && f["params"]["item"]["command"]
                    .as_str()
                    .is_some_and(|c| c.contains(needle))
        })
}

#[tokio::test]
#[ignore = "live: runs the installed codex through scripts/product/adapter-live.sh"]
async fn codex_live_turn_and_cancel() -> R {
    let out = PathBuf::from(std::env::var_os("HOH_LIVE_OUT").ok_or("HOH_LIVE_OUT is not set")?);
    let program =
        PathBuf::from(std::env::var_os("HOH_LIVE_CODEX").ok_or("HOH_LIVE_CODEX is not set")?);
    let none = json!({});
    // Not logged in: BLOCKED before any model call.
    let login = Command::new(&program).args(["login", "status"]).output()?;
    if !login.status.success() {
        let said = String::from_utf8_lossy(&login.stdout).trim().to_owned();
        write_verdict(
            &out,
            "codex",
            "BLOCKED",
            &format!("BLOCKED_AUTH: codex login status: {said}"),
            &[],
            &none,
        )?;
        return Ok(());
    }
    let entry = probe::probe(
        Agent::Codex,
        &ProbeEnv {
            path: std::env::var_os("PATH").ok_or("no PATH")?,
            fixture_root: None,
            timeout: Duration::from_secs(120),
            log: None,
            deadline: None,
            cancel: None,
        },
    )?;
    let version = entry
        .cli_version
        .split_whitespace()
        .last()
        .ok_or("no version")?
        .to_owned();
    let world = World::new()?;
    let schema_dir = world.dir.path().join("schema");
    if !Command::new(&program)
        .args(["app-server", "generate-json-schema", "--out"])
        .arg(&schema_dir)
        .status()?
        .success()
    {
        return Err("codex app-server generate-json-schema failed".into());
    }
    let notifications = std::fs::read(schema_dir.join("ServerNotification.json"))?;
    let known = codex::known_notifications(&serde_json::from_slice(&notifications)?);
    let requests = codex::listed_methods(&serde_json::from_slice(&std::fs::read(
        schema_dir.join("ClientRequest.json"),
    )?)?);
    let client_notifications = codex::listed_methods(&serde_json::from_slice(&std::fs::read(
        schema_dir.join("ClientNotification.json"),
    )?)?);
    std::fs::write(
        out.join("codex-install.json"),
        serde_json::to_vec_pretty(&json!({
            "probe": entry,
            "version": version,
            "server_notification_schema_sha256": hex(&notifications),
            "known_notifications": known.len(),
        }))?,
    )?;
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
    let launch = Launch {
        program: program.clone(),
        app_server_help: Some(stdout_of(&program, &["app-server", "--help"])?),
        exec_help: stdout_of(&program, &["exec", "--help"])?,
        exec_resume_help: Some(stdout_of(&program, &["exec", "resume", "--help"])?),
        cwd: repo.clone(),
        yolo: true,
        resume: None,
        model: None,
        known,
        requests,
        client_notifications,
        capabilities: entry.capabilities.clone(),
    };
    let turn = |prompt: &str, logs: &str| Turn {
        launch: launch.clone(),
        prompt: prompt.to_owned(),
        logs: out.join(logs),
        escalation: Turn::default_escalation(),
        handshake_timeout: Duration::from_secs(60),
        deadline: Some(Duration::from_secs(600)),
    };
    let session_of = |o: &Outcome| {
        json!({
            "agent": "codex",
            "cli_version": version,
            "session_id": o.native_session.clone().unwrap_or_default(),
            "resolved_model": o.model.clone().unwrap_or_else(|| "unknown".to_owned()),
        })
    };
    let mut sessions = Vec::new();
    let mut problems = Vec::new();

    // 1. A reply, no tool.
    let a1 = world.attempt_in("codex", "A1", "S1", &version).await?;
    let (_steer, steers) = mpsc::channel(1);
    let first = codex::run(
        &world.store,
        &a1,
        &turn(
            &format!(
                "Reply with exactly {TOKEN} and nothing else. Do not run any command or tool."
            ),
            "live/codex/A1",
        ),
        &CancelLatch::new(),
        steers,
    )
    .await?;
    if first.state != AttemptState::Settled {
        let (status, why) = if first.failure.as_deref() == Some("BLOCKED_AUTH") {
            ("BLOCKED", "BLOCKED_AUTH: codex is not logged in".to_owned())
        } else {
            (
                "FAIL",
                format!("turn 1 ended {:?} ({:?})", first.state, first.failure),
            )
        };
        write_verdict(&out, "codex", status, &why, &sessions, &none)?;
        return if status == "BLOCKED" {
            Ok(())
        } else {
            Err(why.into())
        };
    }
    if first.mode != Mode::AppServer
        || first.status.as_deref() != Some("completed")
        || !first.reply.as_deref().unwrap_or_default().contains(TOKEN)
    {
        problems.push(format!(
            "turn 1: {:?} {:?} {:?}",
            first.mode, first.status, first.reply
        ));
    }
    sessions.push(session_of(&first));

    // 2. `sleep 120`, cancelled once it runs.
    let a2 = world.attempt_in("codex", "A2", "S2", &version).await?;
    let second_turn = turn(
        "Run the shell command `sleep 120` in the foreground and wait for it to finish, then reply DONE.",
        "live/codex/A2",
    );
    let stdout = second_turn.logs.join("app-server.stdout.jsonl");
    let cancel = CancelLatch::new();
    let (_steer, steers) = mpsc::channel(1);
    let running = codex::run(&world.store, &a2, &second_turn, &cancel, steers);
    let store = &world.store;
    let trigger = async {
        let started = Instant::now();
        let mut server = None;
        let mut tools = Vec::new();
        while started.elapsed() < Duration::from_secs(240) && tools.is_empty() {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if server.is_none() {
                server = store
                    .reader()
                    .ok()
                    .and_then(|r| r.attempt("A2").ok().flatten())
                    .and_then(|a| a.pid)
                    .and_then(|p| u32::try_from(p).ok());
            }
            let Some(server) = server else { continue };
            if command_started(&std::fs::read(&stdout).unwrap_or_default(), "sleep 120") {
                tools = tool_sleeps(&repo, server);
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
    let (second, (at, tools, alive, receipt)) = tokio::join!(running, trigger);
    let second = second?;
    let took = at.elapsed();
    let server = second.pid.unwrap_or_default();
    let mut left = Vec::new();
    let cleared = clear_by(at, Duration::from_secs(16), || {
        left = group_members(server);
        left.extend(left_of(&tools));
        left.extend(processes_in(&repo));
        left.is_empty()
    })
    .await;
    if alive.is_empty() {
        problems.push(format!(
            "turn 2: no `sleep 120` of this turn's command was alive when the cancel was sent (seen {tools:?})"
        ));
    }
    if second.state != AttemptState::Cancelled
        || second.status.as_deref() != Some("interrupted")
        || cleared.is_none()
    {
        problems.push(format!(
            "turn 2: {:?} status {:?} after {took:?}; still there 16 s after the cancel: {left:?}",
            second.state, second.status
        ));
    }
    sessions.push(session_of(&second));
    let observations = json!({
        "turn1": {"threadId": first.native_session, "turnId": first.turn_id, "model": first.model,
                  "status": first.status, "reply": first.reply},
        "turn2": {"threadId": second.native_session, "turnId": second.turn_id, "model": second.model,
                  "status": second.status, "app_server_pid": second.pid,
                  "tool_processes": tools.iter().map(|(pid, pgid)| json!({"pid": pid, "pgid": pgid})).collect::<Vec<_>>(),
                  "alive_at_cancel": alive, "cancel_receipt": format!("{receipt:?}"),
                  "cancel_to_end_ms": took.as_millis(), "cleared_after_ms": cleared.map(|d| d.as_millis()),
                  "left": left},
    });
    let row = world
        .store
        .reader()?
        .attempt("A2")?
        .ok_or("no attempt A2")?;
    if (row.state, row.cancel_requested) != (AttemptState::Cancelled, true) {
        problems.push(format!(
            "A2 in the store: {:?} cancel_requested={}",
            row.state, row.cancel_requested
        ));
    }
    if problems.is_empty() {
        write_verdict(
            &out,
            "codex",
            "PASS",
            &format!("codex {version}: reply, `sleep 120` cancelled (interrupted) in {took:?}"),
            &sessions,
            &observations,
        )?;
        Ok(())
    } else {
        write_verdict(
            &out,
            "codex",
            "FAIL",
            &problems.join("; "),
            &sessions,
            &observations,
        )?;
        Err(problems.join("; ").into())
    }
}
