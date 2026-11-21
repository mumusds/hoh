//! doctor's descendants (docs/adr/ADR-031.md, 设计修正 2, B): doctor is its children's subreaper, so a process one of
//! its helpers set apart (left its process group, `setsid`) and then orphaned is re-parented to doctor, and every
//! process doctor's helpers started is found by /proc's children links from doctor down (another user's process is
//! never read: CLT-1353). The stop kills each one found alive, by pidfd once its start time showed the pidfd holds the
//! process seen, and scans again until one whole and complete scan finds none alive (a /proc read or a pidfd that
//! fails leaves a scan incomplete, never clean): a process has ended only when its pidfd is readable, its whole thread
//! group gone (a Z in /proc is no proof: CLT-1300), and only on a scan after the one that first saw it, whose table
//! shows what it forked as it exited (CLT-1451). Nothing is reaped here (no `P_ALL`, no ECHILD taken for proof): a
//! registered helper is reaped through its handle, an adopted orphan by init once doctor has ended.

use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

/// A process as /proc showed it.
struct Seen {
    pid: u32,
    start: String,
}

/// What the sweep reads of /proc (a test's seam): a process's children, a process's stat.
trait Proc {
    fn children(&self, pid: u32) -> std::io::Result<Vec<u32>>;
    fn stat(&self, pid: u32) -> std::io::Result<Vec<u8>>;
}

/// The real /proc.
struct RealProc;

impl Proc for RealProc {
    fn children(&self, pid: u32) -> std::io::Result<Vec<u32>> {
        let mut children = Vec::new();
        for task in std::fs::read_dir(format!("/proc/{pid}/task"))? {
            let text = std::fs::read_to_string(task?.path().join("children"))?;
            children.extend(
                text.split_whitespace()
                    .filter_map(|c| c.parse::<u32>().ok()),
            );
        }
        Ok(children)
    }

    fn stat(&self, pid: u32) -> std::io::Result<Vec<u8>> {
        std::fs::read(format!("/proc/{pid}/stat"))
    }
}

/// Whether `e` says the process is gone (no entry any more).
fn gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3)
}

/// A process's [`Seen`] from its stat, if it can be parsed.
fn parse(pid: u32, stat: &[u8]) -> Option<Seen> {
    // After the parenthesised command name: state (field 3), ..., start time (22).
    let fields: Vec<&str> = hoh_agents::identity::after_command_name(stat)?
        .split_whitespace()
        .collect();
    Some(Seen {
        pid,
        start: (*fields.get(19)?).to_owned(),
    })
}

/// The descendants of `root` as `proc` shows them, and whether they were read whole: walked down the children links
/// from `root` (`/proc/<pid>/task/*/children`), each one's stat read, so another user's process is never read (CLT-1353:
/// on a /proc mounted with hidepid its stat is refused). A descendant whose children or stat cannot be read for another
/// reason than its being gone leaves the table incomplete (CLT-1200); one seen twice is taken once.
fn descendants_in(root: u32, proc: &dyn Proc) -> (Vec<Seen>, bool) {
    let mut complete = true;
    let mut seen: Vec<Seen> = Vec::new();
    let mut parents = vec![root];
    while let Some(parent) = parents.pop() {
        let children = match proc.children(parent) {
            Ok(children) => children,
            Err(e) if gone(&e) => continue,
            Err(_) => {
                complete = false;
                continue;
            }
        };
        for pid in children {
            if pid == root || seen.iter().any(|s| s.pid == pid) {
                continue;
            }
            match proc.stat(pid) {
                Ok(stat) => match parse(pid, &stat) {
                    Some(one) => {
                        seen.push(one);
                        parents.push(pid);
                    }
                    None => complete = false,
                },
                Err(e) if gone(&e) => {}
                Err(_) => complete = false,
            }
        }
    }
    (seen, complete)
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
        let (table, mut complete) = descendants_in(root, &RealProc);
        // Where a test holds a scan between its table and its looks at the processes in it (debug builds only).
        crate::debug_hold::at("sweep-scanned");
        let mut alive = Vec::new();
        let mut opened = 0;
        for seen in &table {
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

    /// A /proc of a few processes: `(pid, parent)` each, with children by parent; the stat of each pid in `denied` is
    /// refused (EPERM, as /proc mounted with hidepid answers for another user's process).
    struct Fake {
        processes: Vec<(u32, u32)>,
        denied: Vec<u32>,
    }

    impl Proc for Fake {
        fn children(&self, pid: u32) -> std::io::Result<Vec<u32>> {
            Ok(self
                .processes
                .iter()
                .filter(|(_, parent)| *parent == pid)
                .map(|(child, _)| *child)
                .collect())
        }

        fn stat(&self, pid: u32) -> std::io::Result<Vec<u8>> {
            if self.denied.contains(&pid) {
                return Err(std::io::Error::from_raw_os_error(1));
            }
            let parent = self
                .processes
                .iter()
                .find(|(p, _)| *p == pid)
                .map(|(_, parent)| *parent)
                .ok_or_else(|| std::io::Error::from_raw_os_error(2))?;
            Ok(format!("{pid} (x) S {parent} {}{pid}0", "0 ".repeat(17)).into_bytes())
        }
    }

    #[test]
    fn another_users_unreadable_process_leaves_the_scan_complete() {
        // ADR-031, 设计修正 2, corrections after its second implementation review, 9 (CLT-1353): on a /proc mounted with
        // hidepid another user's stat is refused (EPERM). The sweep reads only doctor's descendants, down their children
        // links, so such a process is never read and the scan stays complete; a descendant's own unreadable stat still
        // leaves it incomplete. Before, every pid in /proc was read and one refusal made every scan incomplete.
        let fake = Fake {
            processes: vec![(10, 1), (11, 10), (99, 500)],
            denied: vec![99],
        };
        let (seen, complete) = descendants_in(1, &fake);
        let pids: Vec<u32> = seen.iter().map(|s| s.pid).collect();
        assert_eq!(pids.len(), 2, "{pids:?}");
        assert!(pids.contains(&10) && pids.contains(&11), "{pids:?}");
        assert!(
            complete,
            "an unrelated process's refused stat made the scan incomplete"
        );
        let own = Fake {
            processes: vec![(10, 1), (11, 10)],
            denied: vec![11],
        };
        assert!(
            !descendants_in(1, &own).1,
            "a descendant's refused stat left the scan complete"
        );
    }
}
