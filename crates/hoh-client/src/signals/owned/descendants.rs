//! doctor's descendants (docs/adr/ADR-031.md, 设计修正 2, B): doctor is its children's subreaper, so a process one of
//! its helpers set apart (left its process group, `setsid`) and then orphaned is re-parented to doctor, and every
//! process doctor's helpers started is found by /proc's parent links from doctor down. The stop kills each one found
//! alive, by pidfd once its start time showed the pidfd holds the process seen, and scans again until one whole and
//! complete scan finds none alive (a /proc read or a pidfd that fails leaves a scan incomplete, never clean): a zombie,
//! or a process whose pidfd is readable, has ended. Nothing is reaped here (no `P_ALL`, no ECHILD taken for proof): a
//! registered helper is reaped through its handle, an adopted orphan by init once doctor has ended.

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

/// Every process /proc shows, and whether it was read whole: a /proc that cannot be listed, or a process's stat that
/// cannot be read for another reason than its being gone, leaves the table incomplete (CLT-1200).
fn table() -> (Vec<Seen>, bool) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return (Vec::new(), false);
    };
    let mut complete = true;
    let mut seen = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
                continue;
            }
            Err(_) => {
                complete = false;
                continue;
            }
        };
        // After the parenthesised command name: state (field 3), parent (4), ..., start time (22).
        let fields: Option<Vec<&str>> = hoh_agents::identity::after_command_name(&stat)
            .map(|rest| rest.split_whitespace().collect());
        let parsed = fields.and_then(|fields| {
            Some(Seen {
                pid,
                state: fields.first()?.chars().next()?,
                parent: fields.get(1)?.parse().ok()?,
                start: (*fields.get(19)?).to_owned(),
            })
        });
        match parsed {
            Some(one) => seen.push(one),
            None => complete = false,
        }
    }
    (seen, complete)
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

/// How many new pidfds one scan opens at most: the rest wait for the next scan, when those of the processes seen
/// ended are let go (CLT-1200).
const BATCH: usize = 32;

/// What a sweep left: the pids still alive at its last scan, and whether that scan was incomplete (a table not read
/// whole, a pidfd or a start time that could not be had, or more descendants than one scan takes on).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    pub alive: Vec<u32>,
    pub incomplete: bool,
}

/// Kill every descendant of `root` found alive, scanning again until one whole, complete scan finds none alive or `end`
/// has come.
pub fn sweep(root: u32, end: Instant) -> Swept {
    // Those killed, by the pidfd the kill went through while it has not ended: one whose pidfd is readable has ended,
    // its pidfd is let go at once, and its number seen again belongs to another process.
    let mut killed: Vec<(u32, String, Option<OwnedFd>)> = Vec::new();
    loop {
        // Those killed that have ended let their pidfds go before the scan opens any (a killed orphan is a zombie the
        // scan skips, so it would never be looked at again).
        for (_, _, pidfd) in &mut killed {
            if pidfd.as_ref().is_some_and(exited) {
                *pidfd = None;
            }
        }
        let (table, mut complete) = table();
        let mut alive = Vec::new();
        let mut opened = 0;
        for seen in under(root, &table) {
            if matches!(seen.state, 'Z' | 'X') {
                continue;
            }
            if let Some((_, _, pidfd)) = killed
                .iter_mut()
                .find(|(pid, start, _)| *pid == seen.pid && *start == seen.start)
            {
                match pidfd {
                    Some(fd) if exited(fd) => *pidfd = None,
                    Some(_) => alive.push(seen.pid),
                    None => {}
                }
                continue;
            }
            if opened >= BATCH {
                complete = false;
                alive.push(seen.pid);
                continue;
            }
            let Some(pid) = i32::try_from(seen.pid).ok().and_then(Pid::from_raw) else {
                continue;
            };
            let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
                Ok(pidfd) => pidfd,
                // Gone since the scan: the next scan tells.
                Err(rustix::io::Errno::SRCH) => continue,
                // No descriptor for it (EMFILE, ENOMEM, ...): not confirmed, tried again.
                Err(_) => {
                    complete = false;
                    alive.push(seen.pid);
                    continue;
                }
            };
            opened += 1;
            // Its start time read once the pidfd is held: the same one names the process seen (a number goes to
            // another process only once its holder is reaped). Another one, or none, is left to the next scan; one that
            // cannot be read leaves this scan incomplete.
            match hoh_agents::identity::start_id(seen.pid) {
                Ok(Some(start)) if start == seen.start => {}
                Ok(_) => continue,
                Err(_) => {
                    complete = false;
                    alive.push(seen.pid);
                    continue;
                }
            }
            let _ = pidfd_send_signal(&pidfd, Signal::KILL);
            alive.push(seen.pid);
            killed.push((seen.pid, seen.start.clone(), Some(pidfd)));
        }
        if (alive.is_empty() && complete) || Instant::now() >= end {
            return Swept {
                alive,
                incomplete: !complete,
            };
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
