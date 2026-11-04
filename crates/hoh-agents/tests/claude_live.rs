//! TEST-025-04 against the installed `claude` (live; model calls). Run only by scripts/product/adapter-live.sh, which
//! sets HOH_LIVE_OUT and turns the verdict written there into the check's evidence: three turns in a temporary Git
//! repository with the default graces — a reply, a resumed reply, a cancelled tool run. Not logged in (the adapter's
//! BLOCKED_AUTH, from Claude's recorded `authentication_failed` marker) is BLOCKED, never PASS. The installed CLI
//! (path, version, sha256, help hash) is recorded in claude-install.json.
//!
//! claude 2.1.318 refuses a foreground `sleep 120` ("Blocked: standalone sleep"), so the third turn runs it as a
//! background task, which Claude starts in a process group of its own. The cancel is sent only once this turn's Bash
//! `tool_use` for `sleep 120` stands unrefused and a `sleep 120` descending from this claude is alive; the turn fails
//! if that never happens, or unless claude's group, the tool's process and group (zombies included) and every process
//! working in the repository are all seen gone within 16 s of the cancel. The observer's checks have tests of their
//! own below.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;
mod live;

use common::{R, World};
use hoh_agents::claude::{self, Launch, Outcome, Turn};
use hoh_agents::process::group_members;
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::CancelLatch;
use live::{
    TOKEN, clear_by, descends_from, hex, left_of, processes_in, stat_of, tool_sleeps, write_verdict,
};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Whether `stdout` (stream-json) shows a Bash `tool_use` running `command` that no error `tool_result` answered.
fn tool_started(stdout: &[u8], command: &str) -> bool {
    let frames: Vec<serde_json::Value> = stdout
        .split(|b| *b == b'\n')
        .filter_map(|l| serde_json::from_slice(l).ok())
        .collect();
    let content = |f: &serde_json::Value| {
        f["message"]["content"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    let uses: Vec<String> = frames
        .iter()
        .filter(|f| f["type"] == "assistant")
        .flat_map(content)
        .filter(|c| {
            c["type"] == "tool_use" && c["name"] == "Bash" && c["input"]["command"] == command
        })
        .filter_map(|c| c["id"].as_str().map(str::to_owned))
        .collect();
    let refused = |id: &str| {
        frames
            .iter()
            .filter(|f| f["type"] == "user")
            .flat_map(content)
            .any(|c| c["type"] == "tool_result" && c["tool_use_id"] == id && c["is_error"] == true)
    };
    uses.iter().any(|id| !refused(id))
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
    let a1 = world.attempt_in("claude", "A1", "S1", &version).await?;
    let first = claude::run(
        &world.store,
        &a1,
        &turn(
            &format!("Reply with exactly {TOKEN} and nothing else."),
            "live/A1",
            None,
        ),
        &CancelLatch::new(),
    )
    .await?;
    let none = json!({});
    if first.state != AttemptState::Settled {
        if first.failure.as_deref() == Some("BLOCKED_AUTH") {
            write_verdict(
                &out,
                "claude",
                "BLOCKED",
                "BLOCKED_AUTH: claude is not logged in",
                &sessions,
                &none,
            )?;
            return Ok(());
        }
        write_verdict(
            &out,
            "claude",
            "FAIL",
            &format!("turn 1 ended {:?} ({:?})", first.state, first.failure),
            &sessions,
            &none,
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
    let a2 = world.attempt_in("claude", "A2", "S1", &version).await?;
    let second = claude::run(
        &world.store,
        &a2,
        &turn(
            "Repeat the exact token you replied with in the previous turn, and nothing else.",
            "live/A2",
            first.native_session.clone(),
        ),
        &CancelLatch::new(),
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

    // 3. A long tool run, cancelled once its `sleep 120` is seen alive: CANCELLED, the group gone within 16 s, and no
    //    process left in the repository (the sleep runs in a group of its own).
    let a3 = world.attempt_in("claude", "A3", "S2", &version).await?;
    let third_turn = turn(
        "Use the Bash tool with run_in_background: true to run `sleep 120`. Then wait until that background command \
         has finished before you reply; when it has finished, reply DONE.",
        "live/A3",
        None,
    );
    let stdout = third_turn.logs.join("stdout.jsonl");
    let cancel = CancelLatch::new();
    let running = claude::run(&world.store, &a3, &third_turn, &cancel);
    let store = &world.store;
    let trigger = async {
        // The tool is this turn's: its Bash tool_use, not refused, and a `sleep 120` that descends from this claude.
        let started = Instant::now();
        let mut claude_pid = None;
        let mut tools = Vec::new();
        while started.elapsed() < Duration::from_secs(180) && tools.is_empty() {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if claude_pid.is_none() {
                claude_pid = store
                    .reader()
                    .ok()
                    .and_then(|r| r.attempt("A3").ok().flatten())
                    .and_then(|a| a.pid)
                    .and_then(|p| u32::try_from(p).ok());
            }
            let Some(claude_pid) = claude_pid else {
                continue;
            };
            if tool_started(&std::fs::read(&stdout).unwrap_or_default(), "sleep 120") {
                tools = tool_sleeps(&repo, claude_pid);
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
    let claude_pid = third.pid.unwrap_or_default();
    // Everything is gone within 16 s of the cancel: claude's group, the tool's process and group (zombies included),
    // and any process working in the repository.
    let mut left = Vec::new();
    let cleared = clear_by(at, Duration::from_secs(16), || {
        left = group_members(claude_pid);
        left.extend(left_of(&tools));
        left.extend(processes_in(&repo));
        left.is_empty()
    })
    .await;
    let observations = json!({
        "turn3": {
            "claude_pid": third.pid,
            "tool_processes": tools.iter().map(|(pid, pgid)| json!({"pid": pid, "pgid": pgid})).collect::<Vec<_>>(),
            "alive_at_cancel": alive,
            "cancel_receipt": format!("{receipt:?}"),
            "cancel_to_end_ms": took.as_millis(),
            "cleared_after_ms": cleared.map(|d| d.as_millis()),
            "left": left,
        }
    });
    if alive.is_empty() {
        problems.push(format!(
            "turn 3: no `sleep 120` of this turn's Bash tool was alive when the cancel was sent (seen {tools:?})"
        ));
    }
    if third.state != AttemptState::Cancelled || cleared.is_none() {
        problems.push(format!(
            "turn 3: {:?} after {took:?}; still there 16 s after the cancel: {left:?}",
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
            "claude",
            "PASS",
            &format!(
                "claude {version}: reply, resumed reply, `sleep 120` tool run cancelled in {took:?}"
            ),
            &sessions,
            &observations,
        )?;
        Ok(())
    } else {
        write_verdict(
            &out,
            "claude",
            "FAIL",
            &problems.join("; "),
            &sessions,
            &observations,
        )?;
        Err(problems.join("; ").into())
    }
}

// The observer's own checks (not live).

#[test]
fn a_tool_counts_only_with_its_unrefused_bash_tool_use() {
    let frame = |v: serde_json::Value| format!("{v}\n");
    let use_ = frame(json!({"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "sleep 120", "run_in_background": true}}
    ]}}));
    let ok = frame(json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "t1", "content": "Command running in background"}
    ]}}));
    let refused = frame(json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "t1", "is_error": true, "content": "Blocked: standalone sleep 120"}
    ]}}));
    let init = frame(json!({"type": "system", "subtype": "init"}));
    assert!(tool_started(
        format!("{init}{use_}{ok}").as_bytes(),
        "sleep 120"
    ));
    assert!(tool_started(
        format!("{init}{use_}").as_bytes(),
        "sleep 120"
    ));
    assert!(
        !tool_started(init.as_bytes(), "sleep 120"),
        "no tool frames"
    );
    assert!(!tool_started(
        format!("{init}{use_}{refused}").as_bytes(),
        "sleep 120"
    ));
    assert!(!tool_started(
        format!("{init}{use_}").as_bytes(),
        "sleep 60"
    ));
}

#[tokio::test]
async fn the_observer_needs_descent_counts_zombies_and_keeps_its_deadline() -> R {
    use std::os::unix::process::CommandExt;
    let me = std::process::id();
    // A child in a group of its own descends from this process; pid 1 does not.
    let mut child = Command::new("sleep").arg("30").process_group(0).spawn()?;
    let pid = child.id();
    assert!(descends_from(pid, me));
    assert!(!descends_from(1, me));
    let group = stat_of(pid).ok_or("no stat")?.2;
    assert_eq!(group, pid);
    // Killed but not reaped, it is a zombie: still left.
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(pid)?).ok_or("pid 0")?,
        rustix::process::Signal::KILL,
    )?;
    let zombie = clear_by(Instant::now(), Duration::from_secs(5), || {
        stat_of(pid).is_some_and(|(state, _, _)| state == 'Z')
    })
    .await;
    assert!(zombie.is_some(), "never a zombie");
    assert_eq!(left_of(&[(pid, group)]), [pid]);
    // The pid itself counts, not only its group (no group has this id: it is above any pid_max).
    assert_eq!(left_of(&[(pid, 4_000_000_000)]), [pid]);
    child.wait()?;
    assert!(left_of(&[(pid, group)]).is_empty());
    // Cleared only if seen clear by the bound; a check that clears later is too late.
    let at = Instant::now();
    assert_eq!(
        clear_by(at, Duration::from_millis(300), || false).await,
        None
    );
    let at = Instant::now();
    let late = clear_by(at, Duration::from_millis(300), || {
        at.elapsed() > Duration::from_millis(500)
    })
    .await;
    assert_eq!(late, None);
    // A scan that starts before the bound but ends after it is too late as well (M1T03R-15).
    let at = Instant::now();
    let slow = clear_by(at, Duration::from_millis(300), || {
        std::thread::sleep(Duration::from_millis(400));
        true
    })
    .await;
    assert_eq!(slow, None);
    let cleared = clear_by(Instant::now(), Duration::from_millis(300), || true).await;
    assert!(cleared.is_some_and(|d| d <= Duration::from_millis(300)));
    Ok(())
}
