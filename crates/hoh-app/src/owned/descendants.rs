//! doctor's descendants (docs/adr/ADR-031.md, 设计修正 2, B): doctor is its children's subreaper, so a process one of
//! its helpers set apart (left its process group, `setsid`) and then orphaned is re-parented to doctor, and every
//! process doctor's helpers started is found by /proc's parent links from doctor down. The stop kills each one found
//! alive, by pidfd once its start time showed the pidfd holds the process seen, and scans again until one whole scan
//! finds none alive: a zombie, or a process whose pidfd is readable, has ended. Nothing is reaped here (no `P_ALL`, no
//! ECHILD taken for proof): a registered helper is reaped through its handle, an adopted orphan by init once doctor
//! has ended.

use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

/// A process as /proc showed it.
struct Seen {
    pid: u32,
    parent: u32,
    /// Its state letter: `Z` (zombie) or `X` (being released) has ended.
    state: char,
    start: String,
}

/// Every process /proc shows that can be read.
fn table() -> Vec<Seen> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
            // After the parenthesised command name: state (field 3), parent (4), ..., start time (22).
            let fields: Vec<&str> = hoh_agents::identity::after_command_name(&stat)?
                .split_whitespace()
                .collect();
            Some(Seen {
                pid,
                state: fields.first()?.chars().next()?,
                parent: fields.get(1)?.parse().ok()?,
                start: (*fields.get(19)?).to_owned(),
            })
        })
        .collect()
}

/// The descendants of `root` in `table`: its children, theirs, and so on.
fn under(root: u32, table: &[Seen]) -> Vec<&Seen> {
    let mut found: Vec<&Seen> = Vec::new();
    let mut parents = vec![root];
    while let Some(parent) = parents.pop() {
        for seen in table.iter().filter(|s| s.parent == parent && s.pid != root) {
            if !found.iter().any(|f| f.pid == seen.pid) {
                found.push(seen);
                parents.push(seen.pid);
            }
        }
    }
    found
}

/// Whether the process `pidfd` holds has exited (its pidfd readable).
fn exited(pidfd: &OwnedFd) -> bool {
    hoh_agents::identity::exited(pidfd).unwrap_or(false)
}

/// Kill every descendant of `root` found alive, scanning again until one whole scan finds none alive or `end` has
/// come: the pids of those still alive at the last scan (none when it ended clean).
pub fn sweep(root: u32, end: Instant) -> Vec<u32> {
    // Those killed, by the pidfd the kill went through: one whose pidfd is readable has ended, and its number seen
    // again belongs to another process.
    let mut killed: Vec<(u32, String, OwnedFd)> = Vec::new();
    loop {
        let table = table();
        let mut alive = Vec::new();
        for seen in under(root, &table) {
            if matches!(seen.state, 'Z' | 'X') {
                continue;
            }
            if let Some((_, _, pidfd)) = killed
                .iter()
                .find(|(pid, start, _)| *pid == seen.pid && *start == seen.start)
            {
                if !exited(pidfd) {
                    alive.push(seen.pid);
                }
                continue;
            }
            let Some(pid) = i32::try_from(seen.pid).ok().and_then(Pid::from_raw) else {
                continue;
            };
            // Gone since the scan: the next scan tells.
            let Ok(pidfd) = pidfd_open(pid, PidfdFlags::empty()) else {
                continue;
            };
            // Its start time read once the pidfd is held: the same one names the process seen (a number goes to
            // another process only once its holder is reaped). Another one, or none, is left to the next scan.
            if !matches!(hoh_agents::identity::start_id(seen.pid), Ok(Some(start)) if start == seen.start)
            {
                continue;
            }
            let _ = pidfd_send_signal(&pidfd, Signal::KILL);
            alive.push(seen.pid);
            killed.push((seen.pid, seen.start.clone(), pidfd));
        }
        if alive.is_empty() || Instant::now() >= end {
            return alive;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
