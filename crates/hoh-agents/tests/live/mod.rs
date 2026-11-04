//! Observers shared by the live adapter tests (claude_live.rs, codex_live.rs): processes by working directory and
//! ancestry, a tool's process and group after a cancel (zombies included), a deadline judged after each scan, and
//! the verdict file scripts/product/adapter-live.sh reads.

use super::common::R;
use hoh_agents::process::group_members;
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

/// Processes whose working directory is inside `dir` (a tool run that outlived its turn, in any process group).
pub fn processes_in(dir: &Path) -> Vec<u32> {
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

/// `(state, ppid, pgrp)` of `pid` from /proc, zombies included.
pub fn stat_of(pid: u32) -> Option<(char, u32, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut rest = stat.rsplit_once(')')?.1.split_whitespace();
    let state = rest.next()?.chars().next()?;
    Some((
        state,
        rest.next()?.parse().ok()?,
        rest.next()?.parse().ok()?,
    ))
}

/// Whether `pid` is a descendant of `ancestor`.
pub fn descends_from(pid: u32, ancestor: u32) -> bool {
    let mut at = pid;
    for _ in 0..64 {
        match stat_of(at) {
            Some((_, ppid, _)) if ppid == ancestor => return true,
            Some((_, ppid, _)) if ppid > 1 => at = ppid,
            _ => return false,
        }
    }
    false
}

/// `sleep 120` processes started by `agent` (its descendants) working in `dir`: (pid, process group).
pub fn tool_sleeps(dir: &Path, agent: u32) -> Vec<(u32, u32)> {
    processes_in(dir)
        .into_iter()
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c == b"sleep\x00120\x00")
        })
        .filter(|pid| descends_from(*pid, agent))
        .filter_map(|pid| Some((pid, stat_of(pid)?.2)))
        .collect()
}

/// Processes of `tools` (pid, group) still there, zombies included.
pub fn left_of(tools: &[(u32, u32)]) -> Vec<u32> {
    let mut left: Vec<u32> = tools
        .iter()
        .flat_map(|(pid, pgid)| {
            let mut v = group_members(*pgid);
            v.extend(stat_of(*pid).map(|_| *pid));
            v
        })
        .collect();
    left.sort_unstable();
    left.dedup();
    left
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
