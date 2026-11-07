//! Process identity (REQ-041 b3, docs/adr/ADR-019.md decision 4). A recorded pid names an attempt's process only
//! together with the boot it ran in (`/proc/sys/kernel/random/boot_id`) and its start time (field 22 of
//! `/proc/<pid>/stat`, clock ticks since boot): a reused pid, or one from before a reboot, is another process and gets
//! no signal. A process found to be ours is held by a pidfd opened before its start time is read again, so the check
//! and every later signal name the same process.
//!
//! This is for processes this host did not start (an old host's agents after a kill -9); [`crate::process`]
//! supervises the ones it starts.

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, PidfdFlags, Signal, kill_process_group, pidfd_open, pidfd_send_signal};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

/// What names one process: its pid within one boot, started at one time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub pid: u32,
    pub boot_id: String,
    /// The start time in clock ticks since boot, as decimal text (`attempts.process_start_id`).
    pub start_id: String,
}

/// This machine's current boot.
pub fn boot_id() -> io::Result<String> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

/// The start time of process `pid`, `None` when there is no such process.
pub fn start_id(pid: u32) -> io::Result<Option<String>> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    // After the parenthesised command name the fields start at field 3 (state); the start time is field 22.
    let rest = stat
        .rfind(')')
        .map(|at| &stat[at + 1..])
        .ok_or_else(|| io::Error::other(format!("/proc/{pid}/stat has no command name")))?;
    rest.split_whitespace()
        .nth(19)
        .map(|field| Some(field.to_owned()))
        .ok_or_else(|| io::Error::other(format!("/proc/{pid}/stat has no start time")))
}

/// The identity of process `pid` now, `None` when there is no such process.
pub fn of(pid: u32) -> io::Result<Option<Identity>> {
    let Some(start_id) = start_id(pid)? else {
        return Ok(None);
    };
    Ok(Some(Identity {
        pid,
        boot_id: boot_id()?,
        start_id,
    }))
}

/// The facts of an attempt's move to RUNNING for its process `pid`: the pid with the boot and start time that name it
/// (REQ-041 b3). A start time that cannot be read is left out: the reconcile then treats the pid as another process's.
pub fn running(pid: u32) -> hoh_core::persistence::AttemptFacts {
    let identity = of(pid).ok().flatten();
    hoh_core::persistence::AttemptFacts {
        pid: Some(i64::from(pid)),
        boot_id: identity.as_ref().map(|i| i.boot_id.clone()),
        process_start_id: identity.map(|i| i.start_id),
        ..hoh_core::persistence::AttemptFacts::default()
    }
}

/// Why a recorded process is not ours (the reconcile event records it, AC-041-03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gone {
    /// No pid was recorded: the process never started, or its start was not recorded.
    NoPid,
    /// The pid was recorded in another boot (or with no boot id): whatever has that pid now is another process.
    OtherBoot {
        recorded: Option<String>,
        current: String,
    },
    /// No process has that pid now.
    NoProcess,
    /// The process with that pid started at another time (or no start time was recorded): a reused pid.
    OtherStart {
        recorded: Option<String>,
        current: String,
    },
}

impl Gone {
    /// The basis as a short name ("no_pid", "boot_id_differs", "no_process", "start_time_differs").
    pub fn basis(&self) -> &'static str {
        match self {
            Gone::NoPid => "no_pid",
            Gone::OtherBoot { .. } => "boot_id_differs",
            Gone::NoProcess => "no_process",
            Gone::OtherStart { .. } => "start_time_differs",
        }
    }
}

/// A recorded leader that is gone while processes are left in its process group (the group id is its pid and is not
/// reused while the group has members): whether they are the attempt's descendants or a later process's that reused
/// the pid and left its own group behind cannot be told, so they get no signal and the attempt's slot stays held
/// (REQ-041 failure mode 5; M2T05R-02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaderless {
    pub pgid: u32,
    pub members: Vec<u32>,
}

/// What a record names now.
#[derive(Debug)]
pub enum Presence {
    /// Its process, held by its pidfd.
    Ours(Found),
    /// Its leader gone, members left in its group.
    Leaderless(Leaderless),
    /// Nothing of it is left (or never was): why.
    Gone(Gone),
}

/// What a record names now: [`find`], and for a leader that is gone, whether its group still has members.
pub fn presence(pid: Option<i64>, boot: Option<&str>, start: Option<&str>) -> io::Result<Presence> {
    Ok(match find(pid, boot, start)? {
        Ok(found) => Presence::Ours(found),
        Err(Gone::NoProcess) => {
            let pgid = pid.and_then(|p| u32::try_from(p).ok()).unwrap_or(0);
            let members = crate::process::group_members(pgid);
            if members.is_empty() {
                Presence::Gone(Gone::NoProcess)
            } else {
                Presence::Leaderless(Leaderless { pgid, members })
            }
        }
        Err(gone) => Presence::Gone(gone),
    })
}

/// A process found to be the recorded one, held by its pidfd.
#[derive(Debug)]
pub struct Found {
    pub identity: Identity,
    pidfd: OwnedFd,
}

/// The process a record names, if it is still there, without signalling anything (REQ-041 b3).
pub fn find(
    pid: Option<i64>,
    boot: Option<&str>,
    start: Option<&str>,
) -> io::Result<Result<Found, Gone>> {
    let Some(pid) = pid.and_then(|p| u32::try_from(p).ok()).filter(|p| *p > 0) else {
        return Ok(Err(Gone::NoPid));
    };
    let current = boot_id()?;
    if boot != Some(current.as_str()) {
        return Ok(Err(Gone::OtherBoot {
            recorded: boot.map(str::to_owned),
            current,
        }));
    }
    let Some(raw) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return Ok(Err(Gone::NoPid));
    };
    // The pidfd first: from here on it names one process, whatever happens to the pid.
    let pidfd = match pidfd_open(raw, PidfdFlags::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::SRCH) => return Ok(Err(Gone::NoProcess)),
        Err(e) => return Err(e.into()),
    };
    let Some(now) = start_id(pid)? else {
        return Ok(Err(Gone::NoProcess));
    };
    if start != Some(now.as_str()) {
        return Ok(Err(Gone::OtherStart {
            recorded: start.map(str::to_owned),
            current: now,
        }));
    }
    Ok(Ok(Found {
        identity: Identity {
            pid,
            boot_id: current,
            start_id: now,
        },
        pidfd,
    }))
}

/// One step of a stop, for the events (AC-041-02's order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopStep {
    Term,
    Kill,
    /// The leader has exited and no process is left in its group.
    Reaped,
}

/// How a stop went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub steps: Vec<StopStep>,
    /// Whether the leader exited and its group emptied within the graces.
    pub reaped: bool,
}

impl Found {
    /// Whether the leader has exited (its pidfd is readable).
    pub fn exited(&self) -> bool {
        let mut fds = [PollFd::new(&self.pidfd, PollFlags::IN)];
        matches!(poll(&mut fds, Some(&Timespec::default())), Ok(n) if n > 0)
    }

    fn wait(&self, grace: Duration) -> bool {
        let end = Instant::now() + grace;
        loop {
            let group_left = crate::process::group_members(self.identity.pid).is_empty();
            if self.exited() && group_left {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// SIGTERM to its group, SIGKILL to its group and leader after `term_grace`, then wait `kill_grace` for the
    /// leader to exit and the group to empty (an orphan is reaped by its new parent). The group gets a signal only
    /// while its leader is alive (its pidfd unreadable, so the group id is its) or members are left in it (a group id
    /// is not reused while it has members).
    pub fn stop(&self, term_grace: Duration, kill_grace: Duration) -> Stopped {
        let mut steps = Vec::new();
        let pgid = Pid::from_raw(i32::try_from(self.identity.pid).unwrap_or(0));
        let send = |signal: Signal| {
            if let Some(pgid) = pgid
                && (!self.exited() || !crate::process::group_members(self.identity.pid).is_empty())
            {
                let _ = kill_process_group(pgid, signal);
            }
            if signal == Signal::KILL {
                let _ = pidfd_send_signal(self.pidfd.as_fd(), signal);
            }
        };
        send(Signal::TERM);
        steps.push(StopStep::Term);
        if !self.wait(term_grace) {
            send(Signal::KILL);
            steps.push(StopStep::Kill);
        }
        let reaped = self.wait(kill_grace);
        if reaped {
            steps.push(StopStep::Reaped);
        }
        Stopped { steps, reaped }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_itself_and_any_other_record_names_another() -> io::Result<()> {
        let me = of(std::process::id())?.ok_or_else(|| io::Error::other("no identity"))?;
        let pid = Some(i64::from(me.pid));
        let found = find(pid, Some(&me.boot_id), Some(&me.start_id))?;
        assert_eq!(found.map(|f| f.identity).ok(), Some(me.clone()));
        let other_start = find(pid, Some(&me.boot_id), Some("1"))?;
        assert_eq!(
            other_start.err().map(|g| g.basis()),
            Some("start_time_differs")
        );
        let other_boot = find(pid, Some("another-boot"), Some(&me.start_id))?;
        assert_eq!(other_boot.err().map(|g| g.basis()), Some("boot_id_differs"));
        assert_eq!(find(None, None, None)?.err(), Some(Gone::NoPid));
        Ok(())
    }

    /// An orphan leading its own session and group that ignores SIGTERM: the test's shell starts it and exits, so
    /// it is reparented and reaped by another process, as an old host's agent is after a kill -9.
    fn orphan_ignoring_term() -> io::Result<u32> {
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                "setsid sh -c 'trap \"\" TERM; while :; do sleep 0.05; done' </dev/null >/dev/null 2>&1 & echo $!",
            ])
            .output()?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .map_err(io::Error::other)
    }

    #[test]
    fn a_found_orphan_that_ignores_sigterm_is_killed_with_its_group_and_reaped() -> io::Result<()> {
        let pid = orphan_ignoring_term()?;
        let end = Instant::now() + Duration::from_secs(5);
        // Until its shell has made the session (the group id is its pid).
        while crate::process::group_members(pid).is_empty() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        let me = of(pid)?.ok_or_else(|| io::Error::other("the orphan is gone"))?;
        let found = find(Some(i64::from(pid)), Some(&me.boot_id), Some(&me.start_id))?
            .map_err(|g| io::Error::other(format!("{g:?}")))?;
        let stopped = found.stop(Duration::from_millis(300), Duration::from_secs(10));
        assert_eq!(
            stopped,
            Stopped {
                steps: vec![StopStep::Term, StopStep::Kill, StopStep::Reaped],
                reaped: true
            }
        );
        assert!(crate::process::group_members(pid).is_empty());
        assert_eq!(start_id(pid)?.filter(|s| *s == me.start_id), None);
        Ok(())
    }

    #[test]
    fn a_gone_leader_with_members_left_in_its_group_is_not_gone() -> io::Result<()> {
        // The leader (a shell) starts a child in its group and exits; the child stays.
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                "setsid sh -c 'sleep 30 >/dev/null 2>&1 & echo $$; exit 0' </dev/null 2>/dev/null & wait",
            ])
            .output()?;
        let leader: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .map_err(io::Error::other)?;
        let end = Instant::now() + Duration::from_secs(5);
        while start_id(leader)?.is_some() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        let boot = boot_id()?;
        let found = presence(Some(i64::from(leader)), Some(&boot), Some("1"))?;
        let members = match found {
            Presence::Leaderless(l) => l.members,
            other => return Err(io::Error::other(format!("{other:?}"))),
        };
        assert!(!members.is_empty());
        for pid in members {
            if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
                let _ = rustix::process::kill_process(pid, Signal::KILL);
            }
        }
        Ok(())
    }

    #[test]
    fn a_pid_no_process_has_is_gone() -> io::Result<()> {
        // pid_max is at most 2^22: a larger pid never names a process.
        let boot = boot_id()?;
        assert_eq!(
            find(Some(1 << 23), Some(&boot), Some("1"))?.err(),
            Some(Gone::NoProcess)
        );
        Ok(())
    }
}
