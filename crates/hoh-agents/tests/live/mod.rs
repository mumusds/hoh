//! Observers shared by the live adapter tests (claude_live.rs, codex_live.rs): processes by working directory and
//! ancestry, a tool's process and group after a cancel (zombies included), a deadline judged after each scan, and
//! the verdict file scripts/product/adapter-live.sh reads.

use super::common::R;
use hoh_agents::identity::{after_command_name, group_left};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{Duration, Instant};

pub const TOKEN: &str = "HOH_NATIVE_SMOKE_OK";

pub fn write_verdict(
    out: &Path,
    agent: &str,
    status: &str,
    detail: &str,
    sessions: &[serde_json::Value],
    observations: &serde_json::Value,
) -> R {
    std::fs::write(
        out.join(format!("{agent}-live-verdict.json")),
        serde_json::to_vec_pretty(&json!({
            "status": status,
            "detail": detail,
            "sessions": sessions,
            "observations": observations,
        }))?,
    )?;
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Processes whose working directory is inside `dir` (a tool run that outlived its turn, in any process group). The
/// listing of /proc failing is an error, never "none"; an entry whose working directory cannot be read is left out
/// (another user's, or gone; ADR-022 decision 10, convention v2).
pub fn processes_in(dir: &Path) -> R<Vec<u32>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(dir)) {
            found.push(pid);
        }
    }
    Ok(found)
}

/// `(state, ppid, pgrp)` of `pid` from /proc, zombies included; `None` only when its stat is gone (ENOENT, or ESRCH
/// while being reaped). Any other failed read, or a stat without those fields, is an error (convention v2). A
/// released task's pgrp (-1) reads as 0: it is in no group.
pub fn stat_of(pid: u32) -> R<Option<(char, u32, u32)>> {
    let stat = match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    let unreadable = || format!("/proc/{pid}/stat: {:?}", String::from_utf8_lossy(&stat));
    let mut rest = after_command_name(&stat)
        .ok_or_else(unreadable)?
        .split_whitespace();
    let state = rest
        .next()
        .and_then(|s| s.chars().next())
        .ok_or_else(unreadable)?;
    let ppid: u32 = rest.next().ok_or_else(unreadable)?.parse()?;
    let pgrp: i32 = rest.next().ok_or_else(unreadable)?.parse()?;
    Ok(Some((state, ppid, u32::try_from(pgrp).unwrap_or(0))))
}

/// Whether `pid` is a descendant of `ancestor`.
pub fn descends_from(pid: u32, ancestor: u32) -> bool {
    let mut at = pid;
    for _ in 0..64 {
        match stat_of(at) {
            Ok(Some((_, ppid, _))) if ppid == ancestor => return true,
            Ok(Some((_, ppid, _))) if ppid > 1 => at = ppid,
            _ => return false,
        }
    }
    false
}

/// `sleep 120` processes started by `agent` (its descendants) working in `dir`: (pid, process group).
/// A presence finder: a listing of /proc that fails finds none, which leaves the test with no live tool, a problem it
/// reports (never a pass).
pub fn tool_sleeps(dir: &Path, agent: u32) -> Vec<(u32, u32)> {
    processes_in(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c == b"sleep\x00120\x00")
        })
        .filter(|pid| descends_from(*pid, agent))
        .filter_map(|pid| Some((pid, stat_of(pid).ok()??.2)))
        .collect()
}

/// Processes of `tools` (pid, group) still there, zombies included: each group by the error-keeping scan, each pid by
/// its stat; a look that fails is an error, never "gone" (ADR-022 decision 10, convention v2; M2T07R-1150).
pub fn left_of(tools: &[(u32, u32)]) -> R<Vec<u32>> {
    let mut left = Vec::new();
    for (pid, pgid) in tools {
        left.extend(group_left(*pgid, 0)?);
        if stat_of(*pid)?.is_some() {
            left.push(*pid);
        }
    }
    left.sort_unstable();
    left.dedup();
    Ok(left)
}

/// What is left of a cancelled turn: the agent's group (`leader`'s, zombies included), its tools' processes and
/// groups, and any process working in `repo`. A look that fails is an error, so a turn is never taken for cleared on a
/// failed look.
pub fn left_after(leader: u32, tools: &[(u32, u32)], repo: &Path) -> R<Vec<u32>> {
    let mut left = group_left(leader, 0)?;
    left.extend(left_of(tools)?);
    left.extend(processes_in(repo)?);
    Ok(left)
}

/// When a check of `clear` that held ended, polled from `at` until `at + bound`; `None` if none held by then. The time
/// is taken after each check, so a scan that ends past the bound is too late (M1T03R-15).
pub async fn clear_by(
    at: Instant,
    bound: Duration,
    mut clear: impl FnMut() -> bool,
) -> Option<Duration> {
    loop {
        let held = clear();
        let checked = at.elapsed();
        if checked > bound {
            return None;
        }
        if held {
            return Some(checked);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
