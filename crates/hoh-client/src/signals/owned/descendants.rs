//! doctor's descendants (docs/adr/ADR-031.md, 设计修正 2, B): doctor is its children's subreaper, so a process one of
//! its helpers set apart (left its process group, `setsid`) and then orphaned is re-parented to doctor, and every
//! process doctor's helpers started is found by /proc's parent links from doctor down. The stop kills each one found
//! alive, by pidfd once its start time showed the pidfd holds the process seen, and scans again until one whole and
//! complete scan finds none alive (a /proc read or a pidfd that fails leaves a scan incomplete, never clean): a process
//! has ended only when its pidfd is readable, its whole thread group gone (a Z in /proc is no proof: CLT-1300), and only
//! on a scan after the one that first saw it, whose table shows what it forked as it exited (CLT-1451). Nothing is
//! reaped here (no `P_ALL`, no ECHILD taken for proof): a registered helper is reaped through its handle, an adopted
//! orphan by init once doctor has ended.

use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

/// A process as /proc showed it.
struct Seen {
    pid: u32,
    parent: u32,
    start: String,
}

/// What the sweep reads of /proc (a test's seam): the pids it lists, a process's stat, and the uid that owns a
/// process's directory.
trait Proc {
    fn pids(&self) -> std::io::Result<Vec<std::io::Result<u32>>>;
    fn stat(&self, pid: u32) -> std::io::Result<Vec<u8>>;
    fn owner(&self, pid: u32) -> std::io::Result<u32>;
}

/// The real /proc.
struct RealProc;

impl Proc for RealProc {
    fn pids(&self) -> std::io::Result<Vec<std::io::Result<u32>>> {
        Ok(std::fs::read_dir("/proc")?
            .filter_map(|entry| match entry {
                Err(e) => Some(Err(e)),
                Ok(entry) => entry.file_name().to_str()?.parse::<u32>().ok().map(Ok),
            })
            .collect())
    }

    fn stat(&self, pid: u32) -> std::io::Result<Vec<u8>> {
        std::fs::read(format!("/proc/{pid}/stat"))
    }

    fn owner(&self, pid: u32) -> std::io::Result<u32> {
        use std::os::unix::fs::MetadataExt as _;
        Ok(std::fs::metadata(format!("/proc/{pid}"))?.uid())
    }
}

/// Every process /proc shows, and whether it was read whole: a /proc that cannot be listed, or a process's stat that
/// cannot be read for another reason than its being gone or its being another user's (refused, its directory another
/// uid's: CLT-1353), leaves the table incomplete (CLT-1200).
fn table() -> (Vec<Seen>, bool) {
    table_with(&RealProc, rustix::process::getuid().as_raw())
}

/// [`table`] from `proc`, for this process's user `uid`.
fn table_with(proc: &dyn Proc, uid: u32) -> (Vec<Seen>, bool) {
    let Ok(pids) = proc.pids() else {
        return (Vec::new(), false);
    };
    let mut complete = true;
    let mut seen = Vec::new();
    for pid in pids {
        let Ok(pid) = pid else {
            complete = false;
            continue;
        };
        let stat = match proc.stat(pid) {
            Ok(stat) => stat,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
                continue;
            }
            // Refused, and another user's (its directory is another uid's): no descendant doctor could signal anyway
            // (one that changed its uid is the stated residual of ADR-031 B), so skipped, not taken for an incomplete
            // scan (CLT-1353: /proc mounted with hidepid refuses every other user's stat).
            Err(e)
                if matches!(e.raw_os_error(), Some(1 | 13))
                    && proc.owner(pid).is_ok_and(|owner| owner != uid) =>
            {
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
        // Where a test holds a scan between its table and its looks at the processes in it (debug builds only).
        crate::debug_hold::at("sweep-scanned");
        let mut alive = Vec::new();
        let mut opened = 0;
        for seen in under(root, &table) {
            // A Z or X in /proc is no proof: a thread-group leader that called pthread_exit shows Z while its other
            // threads run (CLT-1300). Only its pidfd, readable once the whole thread group has exited, is.
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
            // Never confirmed on the scan that first saw it, even when its pidfd is readable already: it may have
            // forked as it exited (a daemonizing double fork), and only a later scan's table shows that child, re-parented
            // here (CLT-1451). SIGKILL ends the whole thread group (on one that ended it does nothing).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A /proc of a few processes, `(pid, parent, owner uid)` each; the stat of each pid in `denied` is refused (EPERM,
    /// as a /proc mounted with hidepid answers for another user's process).
    struct Fake {
        processes: Vec<(u32, u32, u32)>,
        denied: Vec<u32>,
    }

    impl Proc for Fake {
        fn pids(&self) -> std::io::Result<Vec<std::io::Result<u32>>> {
            Ok(self.processes.iter().map(|(pid, _, _)| Ok(*pid)).collect())
        }

        fn stat(&self, pid: u32) -> std::io::Result<Vec<u8>> {
            if self.denied.contains(&pid) {
                return Err(std::io::Error::from_raw_os_error(1));
            }
            let (_, parent, _) = self
                .processes
                .iter()
                .find(|(p, _, _)| *p == pid)
                .ok_or_else(|| std::io::Error::from_raw_os_error(2))?;
            Ok(format!("{pid} (x) S {parent} {}{pid}0", "0 ".repeat(17)).into_bytes())
        }

        fn owner(&self, pid: u32) -> std::io::Result<u32> {
            self.processes
                .iter()
                .find(|(p, _, _)| *p == pid)
                .map(|(_, _, uid)| *uid)
                .ok_or_else(|| std::io::Error::from_raw_os_error(2))
        }
    }

    #[test]
    fn another_users_unreadable_process_leaves_the_scan_complete() {
        // ADR-031, 设计修正 2, B (CLT-1353): on a /proc mounted with hidepid another user's stat is refused (EPERM).
        // Its directory is another uid's, so it is no descendant doctor could signal: skipped, the scan stays
        // complete. A refused stat of this user's own process still leaves the scan incomplete. Before, any refusal
        // made every scan incomplete, and doctor's exit waited its whole bound to say so.
        let fake = Fake {
            processes: vec![(10, 1, 1000), (11, 10, 1000), (99, 500, 0)],
            denied: vec![99],
        };
        let (seen, complete) = table_with(&fake, 1000);
        let mut pids: Vec<u32> = seen.iter().map(|s| s.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids, [10, 11]);
        assert!(
            complete,
            "another user's refused stat made the scan incomplete"
        );
        let own = Fake {
            processes: vec![(10, 1, 1000), (11, 10, 1000)],
            denied: vec![11],
        };
        assert!(
            !table_with(&own, 1000).1,
            "this user's own refused stat left the scan complete"
        );
    }
}
