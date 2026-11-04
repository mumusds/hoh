//! TEST-025-04 against the installed `claude` (live; model calls). Run only by scripts/product/adapter-live.sh, which
//! sets HOH_LIVE_OUT and turns the verdict written there into the check's evidence: three turns in a temporary Git
//! repository with the default graces — a reply, a resumed reply, a cancelled tool run. Not logged in is BLOCKED,
//! never PASS. The installed CLI (path, version, sha256, help hash) is recorded in claude-install.json.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use common::{R, World};
use hoh_agents::claude::{self, Launch, Outcome, Turn};
use hoh_agents::process::group_members;
use hoh_core::lifecycle::AttemptState;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const TOKEN: &str = "HOH_NATIVE_SMOKE_OK";

fn write_verdict(out: &Path, status: &str, detail: &str, sessions: &[serde_json::Value]) -> R {
    std::fs::write(
        out.join("claude-live-verdict.json"),
        serde_json::to_vec_pretty(
            &json!({"status": status, "detail": detail, "sessions": sessions}),
        )?,
    )?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Processes whose working directory is inside `dir` (a tool run that outlived its turn, in any process group).
fn processes_in(dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(dir))
        })
        .collect()
}

/// A failed first turn that reads as "not logged in" (no recorded sample exists yet, so this errs on BLOCKED, never
/// on PASS).
fn looks_unauthenticated(outcome: &Outcome) -> bool {
    let text = outcome
        .result
        .as_ref()
        .and_then(|r| r.text.clone())
        .unwrap_or_default();
    let stderr = std::fs::read(&outcome.stderr).unwrap_or_default();
    let all = format!("{text}\n{}", String::from_utf8_lossy(&stderr)).to_lowercase();
    [
        "/login",
        "not logged in",
        "invalid api key",
        "authentication",
        "unauthorized",
        "401",
    ]
    .iter()
    .any(|p| all.contains(p))
}

#[tokio::test]
#[ignore = "live: runs the installed claude through scripts/product/adapter-live.sh"]
async fn claude_live_turns_resume_and_cancel() -> R {
    let out = PathBuf::from(std::env::var_os("HOH_LIVE_OUT").ok_or("HOH_LIVE_OUT is not set")?);
    let program =
        PathBuf::from(std::env::var_os("HOH_LIVE_CLAUDE").ok_or("HOH_LIVE_CLAUDE is not set")?);
    let program = program.canonicalize()?;
    let help = String::from_utf8(Command::new(&program).arg("--help").output()?.stdout)?;
    let version_line = String::from_utf8(Command::new(&program).arg("--version").output()?.stdout)?;
    let version = version_line
        .split_whitespace()
        .next()
        .ok_or("no version")?
        .to_owned();
    std::fs::write(
        out.join("claude-install.json"),
        serde_json::to_vec_pretty(&json!({
            "path": program,
            "version": version,
            "sha256": hex(&std::fs::read(&program)?),
            "help_sha256": hex(help.as_bytes()),
        }))?,
    )?;
    let world = World::new()?;
    let repo = world.dir.path().join("repo");
    std::fs::create_dir_all(&repo)?;
    let repo = repo.canonicalize()?;
    if !Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()?
        .success()
    {
        return Err("git init failed".into());
    }
    let turn = |prompt: &str, logs: &str, resume: Option<String>| Turn {
        launch: Launch {
            program: program.clone(),
            help: help.clone(),
            cwd: repo.clone(),
            yolo: true,
            partial: false,
            resume,
            root: rustix::process::getuid().is_root(),
        },
        prompt: prompt.as_bytes().to_vec(),
        logs: out.join(logs),
        escalation: Turn::default_escalation(),
        deadline: Some(Duration::from_secs(600)),
    };
    let quiet = || watch::channel(false).1;
    let mut sessions = Vec::new();
    let session_of = |o: &Outcome| {
        json!({
            "agent": "claude",
            "cli_version": version,
            "session_id": o.native_session.clone().unwrap_or_default(),
            "resolved_model": o.model.clone().unwrap_or_else(|| "unknown".to_owned()),
        })
    };

    // 1. A reply.
    let a1 = world.attempt_in("A1", "S1", &version).await?;
    let first = claude::run(
        &world.store,
        &a1,
        &turn(
            &format!("Reply with exactly {TOKEN} and nothing else."),
            "live/A1",
            None,
        ),
        quiet(),
    )
    .await?;
    if first.state != AttemptState::Settled {
        if looks_unauthenticated(&first) {
            write_verdict(
                &out,
                "BLOCKED",
                "BLOCKED_AUTH: claude is not logged in",
                &sessions,
            )?;
            return Ok(());
        }
        write_verdict(
            &out,
            "FAIL",
            &format!("turn 1 ended {:?} ({:?})", first.state, first.failure),
            &sessions,
        )?;
        return Err(format!("turn 1: {first:?}").into());
    }
    let text = |o: &Outcome| {
        o.result
            .as_ref()
            .and_then(|r| r.text.clone())
            .unwrap_or_default()
    };
    let mut problems = Vec::new();
    if first.result.as_ref().and_then(|r| r.subtype.as_deref()) != Some("success")
        || !text(&first).contains(TOKEN)
    {
        problems.push(format!("turn 1: {:?}", first.result));
    }
    sessions.push(session_of(&first));

    // 2. The same session, resumed: it remembers the token.
    let a2 = world.attempt_in("A2", "S1", &version).await?;
    let second = claude::run(
        &world.store,
        &a2,
        &turn(
            "Repeat the exact token you replied with in the previous turn, and nothing else.",
            "live/A2",
            first.native_session.clone(),
        ),
        quiet(),
    )
    .await?;
    if second.state != AttemptState::Settled
        || second.native_session != first.native_session
        || !text(&second).contains(TOKEN)
    {
        problems.push(format!(
            "turn 2: {:?} {:?} {:?}",
            second.state, second.native_session, second.result
        ));
    }

    // 3. A long tool run, cancelled once the tool is in flight: CANCELLED and the group gone within 16 s, with no
    //    process left in the repository either.
    let a3 = world.attempt_in("A3", "S2", &version).await?;
    let third_turn = turn(
        "Use the Bash tool to run `sleep 120` in the foreground (not in the background), then reply DONE.",
        "live/A3",
        None,
    );
    let stdout = third_turn.logs.join("stdout.jsonl");
    let (cancel, cancelled) = watch::channel(false);
    let running = claude::run(&world.store, &a3, &third_turn, cancelled);
    let trigger = async {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(180) {
            if std::fs::read(&stdout)
                .is_ok_and(|b| String::from_utf8_lossy(&b).contains("\"tool_use\""))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let at = Instant::now();
        let _ = cancel.send(true);
        at
    };
    let (third, at) = tokio::join!(running, trigger);
    let third = third?;
    let took = at.elapsed();
    let left = third.pid.map(group_members).unwrap_or_default();
    let mut stray = processes_in(&repo);
    while !stray.is_empty() && at.elapsed() < Duration::from_secs(16) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        stray = processes_in(&repo);
    }
    if third.state != AttemptState::Cancelled
        || took > Duration::from_secs(16)
        || !left.is_empty()
        || !stray.is_empty()
    {
        problems.push(format!(
            "turn 3: {:?} after {took:?}, group {left:?}, processes in the repository {stray:?}",
            third.state
        ));
    }
    sessions.push(session_of(&third));
    let row = world
        .store
        .reader()?
        .attempt("A3")?
        .ok_or("no attempt A3")?;
    if (row.state, row.cancel_requested) != (AttemptState::Cancelled, true) {
        problems.push(format!(
            "A3 in the store: {:?} cancel_requested={}",
            row.state, row.cancel_requested
        ));
    }
    if problems.is_empty() {
        write_verdict(
            &out,
            "PASS",
            &format!("claude {version}: reply, resumed reply, cancelled tool run in {took:?}"),
            &sessions,
        )?;
        Ok(())
    } else {
        write_verdict(&out, "FAIL", &problems.join("; "), &sessions)?;
        Err(problems.join("; ").into())
    }
}
