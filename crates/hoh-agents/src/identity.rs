//! Process identity (REQ-041 b3, docs/adr/ADR-019.md decision 4). A recorded pid names an attempt's process only
//! together with the boot it ran in (`/proc/sys/kernel/random/boot_id`) and its start time (field 22 of
//! `/proc/<pid>/stat`, clock ticks since boot): a reused pid, or one from before a reboot, is another process and gets
//! no signal. A process found to be ours is held by a pidfd opened before its start time is read again, so the check
//! and every later signal to it name the same process.
//!
//! Its process group (id = its pid) is the attempt's for as long as that number stays in use (decision 8, design
//! correction 4): the kernel frees a number only when no process uses it as a pid, process group or session id, so
//! while the leader is unreaped it holds the number itself and after its reap the members left in its group hold it.
//! Whether the group is empty is asked of the kernel (`kill(-pgid, 0)` gives ESRCH), never read from a scan of
//! `/proc`, which is no atomic snapshot (M2T05R-300).
//!
//! This is for processes this host did not start (an old host's agents after a kill -9); [`crate::process`]
//! supervises the ones it starts.

use rustix::process::{
    Pid, PidfdFlags, Signal, kill_process_group, pidfd_open, pidfd_send_signal,
    test_kill_process_group,
};
use std::cell::Cell;
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

/// The fields of a `/proc/<pid>/stat` after its parenthesised command name, from field 3 (state) on. The command name
/// is any bytes but NUL (not necessarily UTF-8, and it may hold `)`), so the file is read as bytes and only what follows
/// its last `)` is taken, which is ASCII (M2T06R-500).
pub fn after_command_name(stat: &[u8]) -> Option<&str> {
    let at = stat.iter().rposition(|&b| b == b')')?;
    std::str::from_utf8(&stat[at + 1..]).ok()
}

/// The start time of process `pid`, `None` when /proc has no entry for it (which alone proves nothing: /proc can hide
/// a live process, M2T06R-800).
pub fn start_id(pid: u32) -> io::Result<Option<String>> {
    let stat = match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    // After the parenthesised command name the fields start at field 3 (state); the start time is field 22.
    let rest = after_command_name(&stat)
        .ok_or_else(|| io::Error::other(format!("/proc/{pid}/stat has no command name")))?;
    rest.split_whitespace()
        .nth(19)
        .map(|field| Some(field.to_owned()))
        .ok_or_else(|| io::Error::other(format!("/proc/{pid}/stat has no start time")))
}

/// The identity of process `pid` now, `None` when /proc has no entry for it (see [`start_id`]).
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

/// The facts of an attempt's move to RUNNING for its process: the whole identity that names it (REQ-041 b3). Only a
/// complete identity is recorded; one that cannot be read leaves the process at its gate (M2T05R-100).
pub fn running(identity: &Identity) -> hoh_core::persistence::AttemptFacts {
    hoh_core::persistence::AttemptFacts {
        pid: Some(i64::from(identity.pid)),
        boot_id: Some(identity.boot_id.clone()),
        process_start_id: Some(identity.start_id.clone()),
        ..hoh_core::persistence::AttemptFacts::default()
    }
}

/// The facts of a move to RUNNING by an adapter that starts its process itself (no start gate before M2-T07): the pid
/// and, when it can be read, the rest of its identity; a record left incomplete is uncertain to every reconcile.
pub fn running_pid(pid: u32) -> hoh_core::persistence::AttemptFacts {
    match of(pid).ok().flatten() {
        Some(identity) => running(&identity),
        None => hoh_core::persistence::AttemptFacts {
            pid: Some(i64::from(pid)),
            ..hoh_core::persistence::AttemptFacts::default()
        },
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
}

/// What a record names now.
#[derive(Debug)]
pub enum Presence {
    /// Its process, held by its pidfd.
    Ours(Found),
    /// Its leader gone, its number still in use (members left in its group, or the number taken as a pid since).
    Leaderless(Leaderless),
    /// A pid recorded without the boot or the start time that would tell it apart: nothing can be confirmed
    /// (M2T05R-100).
    Unverifiable,
    /// Nothing of it is left (or never was): why.
    Gone(Gone),
}

/// What a record names now: [`find`], and for a leader that is gone, whether its number is free ([`number_free`]: no
/// process uses it as its pid or as its group, by the kernel's answers, M2T06R-700, -800).
pub fn presence(pid: Option<i64>, boot: Option<&str>, start: Option<&str>) -> io::Result<Presence> {
    if pid.is_some() && (boot.is_none() || start.is_none()) {
        return Ok(Presence::Unverifiable);
    }
    Ok(match find(pid, boot, start)? {
        Ok(found) => Presence::Ours(found),
        Err(Gone::NoProcess) => {
            let pgid = pid.and_then(|p| u32::try_from(p).ok()).unwrap_or(0);
            // The number unused as pid and as group, by the kernel's answers (M2T06R-700, -800).
            if number_free(pgid) {
                Presence::Gone(Gone::NoProcess)
            } else {
                Presence::Leaderless(Leaderless { pgid })
            }
        }
        Err(gone) => Presence::Gone(gone),
    })
}

/// Whether no process (zombies included) is in process group `pgid`: the kernel's own look-up of the group
/// (`kill(-pgid, 0)` gives ESRCH), not a scan (M2T05R-300). Any other answer, EPERM included, means something is
/// still in it.
pub fn group_empty(pgid: u32) -> bool {
    match i32::try_from(pgid).ok().and_then(Pid::from_raw) {
        Some(pgid) => test_kill_process_group(pgid) == Err(rustix::io::Errno::SRCH),
        None => false,
    }
}

/// Whether no process uses `n` at all, neither as its pid nor as its process group: `pidfd_open(n)` finds no process
/// of that number ([`no_process`]; a thread's id is no process's, and a thread cannot make a group of its id) and
/// `kill(-n, 0)` gives ESRCH. Only then can no process of a group numbered `n` come back: while a process `n` lives,
/// or is a zombie, in whatever group, it can make group `n` again (`setpgid(0, 0)`), so an empty group alone proves
/// nothing (docs/adr/ADR-021.md decision 8d v7, M2T06R-700). The kernel's answers only, never /proc's view, which
/// can hide a live process (M2T06R-800). Any other answer means the number is in use.
pub fn number_free(n: u32) -> bool {
    let pid_gone = match i32::try_from(n).ok().and_then(Pid::from_raw) {
        Some(pid) => matches!(pidfd_open(pid, PidfdFlags::empty()), Err(e) if no_process(e)),
        None => false,
    };
    pid_gone && group_empty(n)
}

/// The processes in process group `pgid` other than `leader`, zombies included, from a whole scan of `/proc`: for
/// diagnostics and test evidence only, since a scan is no atomic snapshot ([`group_empty`] tells whether a group is
/// empty). A scan that fails anywhere (an unreadable `/proc`, a stat that cannot be read for another reason than the
/// process having gone) is an error, never an empty group (M2T05R-200).
pub fn group_left(pgid: u32, leader: u32) -> io::Result<Vec<u32>> {
    group_left_in(std::path::Path::new("/proc"), pgid, leader)
}

fn group_left_in(proc: &std::path::Path, pgid: u32, leader: u32) -> io::Result<Vec<u32>> {
    let mut left = Vec::new();
    for entry in std::fs::read_dir(proc)? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            // It ended while the scan ran.
            Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
                continue;
            }
            Err(e) => return Err(e),
        };
        // Fields after the parenthesised command name: state, ppid, pgrp.
        let group = after_command_name(&stat)
            .and_then(|rest| rest.split_whitespace().nth(2)?.parse::<u32>().ok())
            .ok_or_else(|| io::Error::other(format!("/proc/{pid}/stat has no process group")))?;
        if group == pgid && pid != leader {
            left.push(pid);
        }
    }
    Ok(left)
}

/// A process found to be the recorded one, held by its pidfd.
#[derive(Debug)]
pub struct Found {
    pub identity: Identity,
    pidfd: OwnedFd,
    /// When its group was last seen to be the attempt's ([`Group::Held`]).
    held: Cell<Instant>,
}

/// How long a sight of a group as the attempt's lasts once its leader is reaped (ADR-019 decision 8, design
/// correction 4): its members hold its number, but only a group watched this closely can be told not to have emptied
/// and had its number handed out again in between.
pub const HELD_FOR: Duration = Duration::from_millis(100);

/// How often a stop looks at the group.
const LOOK_EVERY: Duration = Duration::from_millis(10);

/// What one look at a found process's group shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Still the attempt's: its leader unreaped (it holds the number itself), or its leader reaped, processes left in
    /// it and the previous `Held` no longer than [`HELD_FOR`] ago. Only now may it be signalled.
    Held,
    /// Nothing is left in it: its leader was reaped and no process has its group id ([`group_empty`]).
    Empty,
    /// Whether it is still the attempt's cannot be told: its leader's state could not be read (M2T05R-400), or
    /// processes are left in it but it was not watched closely enough since its leader's reap to know they are the
    /// attempt's. No signal, and its stop is not reaped.
    Lost,
}

/// What a process `pid` whose pidfd opened but whose start time cannot be read is: gone only when its pidfd says it was
/// reaped; alive, a zombie, or nothing told (an fdinfo that cannot be read, another pid namespace's view), it cannot be
/// confirmed (M2T06R-800, -951).
fn unread_start(leader: Leader, pid: u32) -> io::Result<Gone> {
    match leader {
        Leader::Reaped => Ok(Gone::NoProcess),
        Leader::Unreaped | Leader::Unknown => Err(io::Error::other(format!(
            "process {pid} is not known to be gone but /proc/{pid}/stat cannot be read"
        ))),
    }
}

/// What `pidfd`'s fdinfo says of its process `pid` (this process's own /proc, never hidden from it).
fn pidfd_leader(pidfd: &OwnedFd, pid: u32) -> Leader {
    use std::os::fd::AsRawFd as _;
    let shown = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd()))
        .ok()
        .and_then(|info| {
            info.lines()
                .find_map(|l| l.strip_prefix("Pid:").map(|v| v.trim().to_owned()))
        });
    leader_shown(shown.as_deref(), pid)
}

/// What a pidfd's fdinfo `Pid:` field (`None`: unreadable or missing) says of leader `pid`.
fn leader_shown(shown: Option<&str>, pid: u32) -> Leader {
    match shown {
        Some(shown) if shown == pid.to_string() => Leader::Unreaped,
        Some("-1") => Leader::Reaped,
        _ => Leader::Unknown,
    }
}

/// What its pidfd's fdinfo says of a found leader (-1 once it is reaped: seen on Linux 7.0, and pinned on every
/// kernel the tests run on by `a_zombie_leader_holds_its_group_until_it_is_reaped`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leader {
    /// Alive or a zombie: it holds its pid, so its group's number is in use.
    Unreaped,
    /// Reaped.
    Reaped,
    /// Not readable, or not one of those (another pid namespace's view, a missing field): nothing is told by it.
    Unknown,
}

/// The process a record names, if it is still there, without signalling anything (REQ-041 b3).
pub fn find(
    pid: Option<i64>,
    boot: Option<&str>,
    start: Option<&str>,
) -> io::Result<Result<Found, Gone>> {
    find_with(pid, boot, start, start_id)
}

/// [`find`] with the read of the start time given (a test seam for a pause inside that read, M2T05R-401, -551).
fn find_with(
    pid: Option<i64>,
    boot: Option<&str>,
    start: Option<&str>,
    read_start: impl FnOnce(u32) -> io::Result<Option<String>>,
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
        Err(e) if no_process(e) => return Ok(Err(Gone::NoProcess)),
        Err(e) => return Err(e.into()),
    };
    // Its start time, read next, shows it holding its pid at some moment after this one: the moment its group is
    // known to be the attempt's is taken before the read, never after it (M2T05R-401).
    let seen = Instant::now();
    let Some(now) = read_start(pid)? else {
        // No /proc entry is no proof: /proc can hide a live process (hidepid, a mount over it). Only the kernel's
        // answer through the pidfd tells it is gone (reaped); alive or a zombie, it cannot be confirmed (M2T06R-800).
        return unread_start(pidfd_leader(&pidfd, pid), pid).map(Err);
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
        held: Cell::new(seen),
    }))
}

/// Whether `pidfd_open` (no flags, a pid above 0) failing with `e` means no process has that pid as its own: ESRCH
/// (no such number); for a number with a task but no thread-group leader of that number, Linux 6.8 to 6.15 give EINVAL
/// both for one held only as a process group id by the members left after its leader's reap and for a live thread's
/// id, while 6.16 and later give ESRCH for the former and ENOENT for the latter (M2T05R-553, -600, -650). The
/// recorded process is gone either way; its group is then asked about by number.
fn no_process(e: rustix::io::Errno) -> bool {
    e == rustix::io::Errno::SRCH || e == rustix::io::Errno::INVAL || e == rustix::io::Errno::NOENT
}

/// One step of a stop, for the events (AC-041-02's order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopStep {
    /// SIGTERM was sent to the group.
    Term,
    /// SIGKILL was sent to the group and its leader.
    Kill,
    /// The leader was reaped and no process is left in its group.
    Reaped,
    /// The group was lost track of ([`Group::Lost`]): nothing more was sent.
    Lost,
}

/// How a stop went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub steps: Vec<StopStep>,
    /// Whether its group was seen empty within the graces.
    pub reaped: bool,
}

impl Found {
    /// Its leader's state by its pidfd's fdinfo: its pid while it is unreaped, -1 once it is reaped (a pidfd keeps
    /// the process, not its number). A read that fails or shows anything else tells nothing (M2T05R-400).
    fn leader(&self) -> Leader {
        pidfd_leader(&self.pidfd, self.identity.pid)
    }

    /// Whether its leader is unreaped (alive or a zombie).
    #[cfg(test)]
    fn unreaped(&self) -> bool {
        self.leader() == Leader::Unreaped
    }

    /// Look at its group once (ADR-019 decision 8, design corrections 4 and 5). Empty only when its leader is known
    /// reaped and nothing is left in its group; the moment kept for the next look is taken before this one's reads.
    pub fn look(&self) -> Group {
        let at = Instant::now();
        match self.leader() {
            Leader::Unreaped => {
                self.held.set(at);
                Group::Held
            }
            Leader::Unknown => Group::Lost,
            Leader::Reaped if group_empty(self.identity.pid) => Group::Empty,
            Leader::Reaped => {
                if Instant::now().saturating_duration_since(self.held.get()) <= HELD_FOR {
                    self.held.set(at);
                    Group::Held
                } else {
                    Group::Lost
                }
            }
        }
    }

    /// Look at its group every 10 ms until it is not [`Group::Held`] or `grace` is over; what was seen last.
    pub fn wait(&self, grace: Duration) -> Group {
        let end = Instant::now() + grace;
        loop {
            let group = self.look();
            if group != Group::Held || Instant::now() >= end {
                return group;
            }
            std::thread::sleep(LOOK_EVERY);
        }
    }

    /// Send `signal` to its group (and a SIGKILL to its leader through its pidfd too), only right after a look that
    /// shows the group still the attempt's; whether the group signal went.
    fn signal(&self, signal: Signal) -> bool {
        if self.look() != Group::Held {
            return false;
        }
        let Some(pgid) = i32::try_from(self.identity.pid)
            .ok()
            .and_then(Pid::from_raw)
        else {
            return false;
        };
        let sent = kill_process_group(pgid, signal).is_ok();
        if signal == Signal::KILL {
            let _ = pidfd_send_signal(self.pidfd.as_fd(), signal);
        }
        sent
    }

    /// SIGTERM to its group, only right after a look that shows the group still the attempt's (a cancel's step,
    /// docs/adr/ADR-021.md decision 8c); whether it went.
    pub fn terminate(&self) -> bool {
        self.signal(Signal::TERM)
    }

    /// SIGKILL to its group and leader, under the same rule as [`Found::terminate`]; whether the group signal went.
    pub fn kill(&self) -> bool {
        self.signal(Signal::KILL)
    }

    /// SIGTERM to its group, SIGKILL to its group and leader after `term_grace`, then up to `kill_grace` for the
    /// group to empty (an orphan is reaped by its new parent). A group already empty gets no signal (M2T05R-259); a
    /// step is recorded only for a signal that went (M2T05R-355); a group lost track of gets nothing more.
    pub fn stop(&self, term_grace: Duration, kill_grace: Duration) -> Stopped {
        let mut steps = Vec::new();
        if self.signal(Signal::TERM) {
            steps.push(StopStep::Term);
        }
        let mut group = self.wait(term_grace);
        if group == Group::Held {
            if self.signal(Signal::KILL) {
                steps.push(StopStep::Kill);
            }
            group = self.wait(kill_grace);
        }
        match group {
            Group::Empty => steps.push(StopStep::Reaped),
            Group::Lost => steps.push(StopStep::Lost),
            Group::Held => {}
        }
        Stopped {
            steps,
            reaped: group == Group::Empty,
        }
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

    #[test]
    fn a_number_is_free_only_when_no_process_has_it_as_its_pid_or_its_group() -> io::Result<()> {
        // M2T06R-700: a live process N that moved into another group leaves group N empty, yet it can make group N
        // again (setpgid(0, 0)) at any time: the number is free only when neither a pid nor a group uses it.
        use std::os::unix::process::CommandExt as _;
        let mine = rustix::process::getpgrp();
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r x"])
            .stdin(std::process::Stdio::piped())
            .process_group(mine.as_raw_pid())
            .spawn()?;
        let n = child.id();
        let alive = (group_empty(n), number_free(n));
        drop(child.stdin.take());
        child.wait()?;
        assert_eq!(alive, (true, false), "its group is empty, its pid is not");
        assert!(number_free(n), "both free once it is reaped");
        assert!(
            !number_free(mine.as_raw_pid().unsigned_abs()),
            "this test's own group"
        );
        Ok(())
    }

    #[test]
    fn the_fields_after_a_command_name_start_after_its_last_parenthesis() {
        // M2T06R-650: a command name may hold ") " and bytes that are not UTF-8; the fields are what follows its last
        // `)` (with the first one, the start time would be read from the wrong field: another process, never ours).
        let stat = b"42 (x) a b\xff) S 7 42 42 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 12345 0";
        let rest = after_command_name(stat).map(str::split_whitespace);
        assert_eq!(rest.and_then(|mut f| f.nth(19)), Some("12345"));
        assert_eq!(
            after_command_name(stat).and_then(|r| r.split_whitespace().nth(2)),
            Some("42")
        );
        assert_eq!(after_command_name(b"44 no name"), None);
    }

    #[test]
    fn a_process_whose_command_name_is_not_utf8_has_an_identity_and_a_group() -> io::Result<()> {
        // M2T06R-500: a command name is any bytes but NUL; reading /proc/<pid>/stat as text failed on one that is not
        // UTF-8, and the cancel took the failure for a group it could not inspect.
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::process::CommandExt as _;
        let dir = tempfile::tempdir()?;
        let link = dir.path().join(std::ffi::OsStr::from_bytes(b"w\xffr"));
        // dash, not sleep: this platform's sleep is a multi-call program that picks its tool by its name.
        std::os::unix::fs::symlink("/usr/bin/dash", &link)?;
        let mut child = std::process::Command::new(&link)
            .args(["-c", "read -r x"])
            .stdin(std::process::Stdio::piped())
            .process_group(0)
            .spawn()?;
        let pid = child.id();
        // The kernel names it after its exec only after the spawn has returned (seen in CI): wait for the name.
        let comm = format!("/proc/{pid}/comm");
        let end = Instant::now() + Duration::from_secs(5);
        while std::fs::read(&comm)? != b"w\xffr\n" && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        let seen = (
            std::fs::read(&comm)?,
            of(pid),
            group_left(pid, 0),
            crate::process::group_members(pid),
        );
        drop(child.stdin.take());
        child.wait()?;
        let (comm, me, left, members) = seen;
        assert_eq!(comm, b"w\xffr\n", "the precondition: its name is not UTF-8");
        let me = me?.ok_or_else(|| io::Error::other("no identity"))?;
        assert_eq!(me.pid, pid);
        assert_eq!(left?, vec![pid]);
        assert_eq!(members, vec![pid]);
        Ok(())
    }

    /// An orphan leading its own session and group that ignores SIGTERM: the test's shell starts it and exits, so
    /// it is reparented and reaped by another process, as an old host's agent is after a kill -9.
    fn orphan_ignoring_term() -> io::Result<u32> {
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                "setsid sh -c 'trap \"\" TERM; sleep 1000 & while :; do sleep 0.05; done' </dev/null >/dev/null 2>&1 & echo $!",
            ])
            .output()?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .map_err(io::Error::other)
    }

    /// Kills process group `.0` however a test ends (M2T05R-252).
    struct Guard(u32);

    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(pgid) = i32::try_from(self.0).ok().and_then(Pid::from_raw) {
                let _ = kill_process_group(pgid, Signal::KILL);
            }
        }
    }

    /// Wait up to 5 s for `until`.
    fn wait_until(mut until: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while !until() {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// Whether process `pid` ignores SIGTERM (bit 15 of its `SigIgn` mask).
    fn ignores_term(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|status| {
                status.lines().find_map(|l| {
                    l.strip_prefix("SigIgn:")
                        .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
                })
            })
            .is_some_and(|mask| mask & (1 << (15 - 1)) != 0)
    }

    /// A leader leading its own session whose one member ignores SIGTERM while the leader itself does not; started
    /// by a shell that exits at once, so the leader is reaped by another process. Its pid, once the member ignores
    /// SIGTERM (M2T05R-353).
    fn leader_with_a_member_ignoring_term() -> io::Result<u32> {
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                "setsid sh -c '(trap \"\" TERM; exec sleep 1000) </dev/null >/dev/null 2>&1 & wait' </dev/null >/dev/null 2>&1 & echo $!",
            ])
            .output()?;
        let leader: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .map_err(io::Error::other)?;
        let ready = wait_until(|| {
            group_left(leader, leader)
                .is_ok_and(|left| left.len() == 1 && left.iter().all(|&m| ignores_term(m)))
        });
        if !ready {
            let _ = Guard(leader);
            return Err(io::Error::other("its member never came to ignore SIGTERM"));
        }
        Ok(leader)
    }

    fn found(pid: u32) -> io::Result<Found> {
        let me = of(pid)?.ok_or_else(|| io::Error::other("it is not running"))?;
        find(Some(i64::from(pid)), Some(&me.boot_id), Some(&me.start_id))?
            .map_err(|g| io::Error::other(format!("{g:?}")))
    }

    #[test]
    fn a_found_orphan_that_ignores_sigterm_is_killed_with_its_group_and_reaped() -> io::Result<()> {
        let pid = orphan_ignoring_term()?;
        let _guard = Guard(pid);
        let end = Instant::now() + Duration::from_secs(5);
        // Until its shell has set its trap and started its member (the member comes after the trap).
        while group_left(pid, pid).map_or(0, |l| l.len()) < 1 && Instant::now() < end {
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
        assert!(group_empty(pid));
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
        let _guard = Guard(leader);
        let end = Instant::now() + Duration::from_secs(5);
        while start_id(leader)?.is_some() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        let boot = boot_id()?;
        let found = presence(Some(i64::from(leader)), Some(&boot), Some("1"))?;
        assert!(
            matches!(&found, Presence::Leaderless(l) if l.pgid == leader),
            "{found:?}"
        );
        assert!(!group_left(leader, leader)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_group_whose_leader_was_reaped_is_still_killed_while_it_is_watched() -> io::Result<()> {
        // M2T05R-351: the leader dies of SIGTERM and is reaped by its new parent while its member, which ignores
        // SIGTERM, stays in its group and so keeps its number in use: the stop, watching the group all along, still
        // SIGKILLs it and sees the group empty.
        let leader = leader_with_a_member_ignoring_term()?;
        let _guard = Guard(leader);
        let found = found(leader)?;
        let stopped = found.stop(Duration::from_millis(500), Duration::from_secs(5));
        assert!(!found.unreaped());
        assert_eq!(
            stopped,
            Stopped {
                steps: vec![StopStep::Term, StopStep::Kill, StopStep::Reaped],
                reaped: true
            }
        );
        assert!(group_empty(leader));
        Ok(())
    }

    #[test]
    fn a_group_not_watched_since_its_leader_was_reaped_gets_no_signal() -> io::Result<()> {
        // ADR-019 decision 8, design correction 4: once the leader is reaped, a group not looked at within HELD_FOR
        // cannot be told from one that emptied and had its number handed out again; it gets nothing and its stop is
        // not reaped. The member lives on (killed by the guard).
        let leader = leader_with_a_member_ignoring_term()?;
        let _guard = Guard(leader);
        let found = found(leader)?;
        if let Some(pid) = i32::try_from(leader).ok().and_then(Pid::from_raw) {
            rustix::process::kill_process(pid, Signal::TERM)?;
        }
        assert!(wait_until(|| !found.unreaped()));
        std::thread::sleep(HELD_FOR + Duration::from_millis(50));
        let stopped = found.stop(Duration::from_millis(100), Duration::from_millis(100));
        assert_eq!(
            stopped,
            Stopped {
                steps: vec![StopStep::Lost],
                reaped: false
            }
        );
        assert!(!group_empty(leader));
        let left = group_left(leader, leader)?;
        assert!(left.len() == 1 && ignores_term(left[0]), "{left:?}");
        Ok(())
    }

    #[test]
    fn a_group_that_churns_is_never_seen_empty_until_it_is() -> io::Result<()> {
        // M2T05R-300: its members relay (each starts the next, then exits), so at every instant one is in the group
        // while no pid lasts: a scan of /proc can miss them all, the kernel's look-up of the group cannot.
        use std::io::Write as _;
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "read -r go; relay() { (sleep 0.002; relay) & }; relay; exit 0",
            ])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let leader = child.id();
        let _guard = Guard(leader);
        let found = found(leader)?;
        child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?
            .write_all(b"go\n")?;
        child.wait()?;
        assert!(!found.unreaped());
        let (mut looks, mut scans_empty) = (0, 0);
        let end = Instant::now() + Duration::from_secs(1);
        while Instant::now() < end {
            assert_eq!(found.look(), Group::Held);
            looks += 1;
            if group_left(leader, leader).is_ok_and(|left| left.is_empty()) {
                scans_empty += 1;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // How often a whole scan read it as empty meanwhile (shown, not asserted: it depends on the scheduling).
        eprintln!("{looks} looks, all held; {scans_empty} scans read the group as empty");
        let stopped = found.stop(Duration::from_secs(2), Duration::from_secs(5));
        assert!(stopped.reaped, "{stopped:?}");
        assert_eq!(found.look(), Group::Empty);
        Ok(())
    }

    #[test]
    fn a_leader_whose_state_cannot_be_read_is_not_taken_for_reaped() {
        // M2T05R-400: only its own pid (unreaped) and -1 (reaped) tell anything; an unreadable fdinfo, a missing
        // field, another namespace's 0 or any other number leave the group lost track of, never empty.
        assert_eq!(leader_shown(Some("42"), 42), Leader::Unreaped);
        assert_eq!(leader_shown(Some("-1"), 42), Leader::Reaped);
        for shown in [None, Some("0"), Some("43"), Some(""), Some("-2")] {
            assert_eq!(leader_shown(shown, 42), Leader::Unknown, "{shown:?}");
        }
        // A look whose fdinfo tells nothing (here no pidfd at all, so no Pid: field) for a group that is empty.
        let found = Found {
            identity: Identity {
                pid: 1 << 23,
                boot_id: String::new(),
                start_id: String::new(),
            },
            pidfd: OwnedFd::from(std::fs::File::open("/dev/null").expect("/dev/null")),
            held: Cell::new(Instant::now()),
        };
        assert!(group_empty(found.identity.pid));
        assert_eq!(found.look(), Group::Lost);
        assert_eq!(
            found.stop(Duration::from_millis(10), Duration::from_millis(10)),
            Stopped {
                steps: vec![StopStep::Lost],
                reaped: false
            }
        );
    }

    #[test]
    fn a_zombie_leader_holds_its_group_until_it_is_reaped() -> io::Result<()> {
        // M2T05R-451: the first leg of Held. A leader that exited but is not reaped holds its number, however long
        // ago it was last looked at; once reaped, with nothing left in its group, the group is empty. This pins the
        // fdinfo reading on whatever kernel runs the tests.
        assert_eq!(HELD_FOR, Duration::from_millis(100));
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r go; exit 0"])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        let leader = child.id();
        let _guard = Guard(leader);
        let found = found(leader)?;
        drop(child.stdin.take());
        assert!(wait_until(|| found.leader() == Leader::Unreaped
            && std::fs::read_to_string(format!("/proc/{leader}/stat"))
                .is_ok_and(|stat| stat.rsplit(')').next().is_some_and(
                    |rest| rest.trim_start().starts_with('Z')
                ))));
        std::thread::sleep(HELD_FOR + Duration::from_millis(50));
        assert_eq!(found.look(), Group::Held);
        child.wait()?;
        assert_eq!(found.leader(), Leader::Reaped);
        assert_eq!(found.look(), Group::Empty);
        Ok(())
    }

    #[test]
    fn a_pause_inside_the_identity_read_is_not_taken_for_a_fresh_look() -> io::Result<()> {
        // M2T05R-401, -551: the moment its group is known to be the attempt's is taken before the start time is read.
        // Here that read returns its (matching) value only after the leader has died and been reaped and HELD_FOR
        // has passed: its member, left in the group, gets nothing.
        let leader = leader_with_a_member_ignoring_term()?;
        let _guard = Guard(leader);
        let me = of(leader)?.ok_or_else(|| io::Error::other("it is not running"))?;
        let found = find_with(
            Some(i64::from(leader)),
            Some(&me.boot_id),
            Some(&me.start_id),
            |pid| {
                let read = start_id(pid);
                if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
                    let _ = rustix::process::kill_process(pid, Signal::TERM);
                }
                wait_until(|| start_id(leader).is_ok_and(|s| s.is_none()));
                std::thread::sleep(HELD_FOR + Duration::from_millis(50));
                read
            },
        )?
        .map_err(|g| io::Error::other(format!("{g:?}")))?;
        assert_eq!(found.leader(), Leader::Reaped);
        assert_eq!(found.look(), Group::Lost);
        assert!(!group_empty(leader));
        Ok(())
    }

    #[test]
    fn a_number_used_only_as_another_users_pid_is_not_free() -> io::Result<()> {
        // M2T06R-850: the pid half of number_free on its own: a process of another user that leads no group (a kernel
        // thread, or a daemon in its parent's group) has its pid in use while no group has that number.
        let mine = rustix::process::getuid().as_raw();
        let other = std::fs::read_dir("/proc")?
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| pid > 1)
            .find(|&pid| {
                let group = std::fs::read(format!("/proc/{pid}/stat"))
                    .ok()
                    .and_then(|stat| {
                        after_command_name(&stat)?
                            .split_whitespace()
                            .nth(2)?
                            .parse::<u32>()
                            .ok()
                    });
                let owner = std::fs::read_to_string(format!("/proc/{pid}/status"))
                    .ok()
                    .and_then(|s| {
                        s.lines()
                            .find_map(|l| l.strip_prefix("Uid:"))?
                            .split_whitespace()
                            .next()?
                            .parse::<u32>()
                            .ok()
                    });
                group.is_some_and(|g| g != pid)
                    && owner.is_some_and(|uid| uid != mine)
                    && group_empty(pid)
            });
        let Some(other) = other else {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "SKIP: no other user's process outside a group of its pid"
            );
            return Ok(());
        };
        assert!(!number_free(other), "{other}");
        Ok(())
    }

    #[test]
    fn a_start_time_that_cannot_be_read_is_gone_only_when_the_pidfd_says_reaped() {
        // M2T06R-951: the mapping on its own: a pidfd whose fdinfo cannot be read (Unknown) proves nothing.
        assert_eq!(unread_start(Leader::Reaped, 7).ok(), Some(Gone::NoProcess));
        assert!(unread_start(Leader::Unreaped, 7).is_err());
        assert!(unread_start(Leader::Unknown, 7).is_err());
    }

    #[test]
    fn a_live_process_whose_proc_entry_cannot_be_seen_is_not_gone() -> io::Result<()> {
        // M2T06R-800: /proc can hide a live process (hidepid, a mount over it); only the kernel's answer for the
        // pid tells it is gone. The pidfd held a live process when its start time could not be read: an error (the
        // record cannot be confirmed), never NoProcess. Once the pidfd's process is reaped, NoProcess.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r x"])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        let pid = child.id();
        let me = of(pid)?.ok_or_else(|| io::Error::other("it is not running"))?;
        let hidden = find_with(
            Some(i64::from(pid)),
            Some(&me.boot_id),
            Some(&me.start_id),
            |_| Ok(None),
        );
        let mut stdin = child.stdin.take();
        let ended = find_with(
            Some(i64::from(pid)),
            Some(&me.boot_id),
            Some(&me.start_id),
            |_| {
                drop(stdin.take());
                let _ = child.wait();
                Ok(None)
            },
        );
        assert!(
            hidden.is_err(),
            "{:?}",
            hidden.map(|f| f.map(|f| f.identity))
        );
        assert_eq!(ended?.err(), Some(Gone::NoProcess));
        Ok(())
    }

    #[test]
    fn a_pid_with_no_process_by_either_answer_is_gone() {
        // M2T05R-553, -650: the kernels' answers for a number that is no process's own (see `no_process`).
        assert!(no_process(rustix::io::Errno::SRCH));
        assert!(no_process(rustix::io::Errno::INVAL));
        assert!(no_process(rustix::io::Errno::NOENT));
        assert!(!no_process(rustix::io::Errno::PERM));
        assert!(!no_process(rustix::io::Errno::MFILE));
    }

    #[test]
    fn a_pid_that_is_now_a_thread_of_another_process_is_gone() -> io::Result<()> {
        // M2T05R-650 (REQ-041 failure mode 2): a recorded pid that is now a live thread's id (here one of this test's
        // own) names no process of the attempt; pidfd_open refuses it (EINVAL up to 6.15, ENOENT from 6.16) and no
        // process group has that number, so the record is Gone, not uncertain.
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            // `/proc/thread-self` links to `<pid>/task/<tid>`.
            let tid = std::fs::read_link("/proc/thread-self")
                .ok()
                .and_then(|link| {
                    link.file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.parse::<u32>().ok())
                });
            let _ = tid_tx.send(tid);
            let _ = stop_rx.recv();
        });
        let tid = tid_rx
            .recv()
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("no thread id"))?;
        assert_ne!(tid, std::process::id());
        let boot = boot_id()?;
        let seen = presence(Some(i64::from(tid)), Some(&boot), Some("1"));
        let _ = stop_tx.send(());
        let _ = thread.join();
        assert!(
            matches!(&seen, Ok(Presence::Gone(Gone::NoProcess))),
            "{seen:?}"
        );
        Ok(())
    }

    #[test]
    fn a_churning_group_whose_leader_is_gone_is_never_read_as_gone() -> io::Result<()> {
        // M2T05R-450: the reconcile's view of a record whose leader is gone (no pidfd of it): its relaying members
        // keep the group, so every inspection is Leaderless (no signal, slot kept), never Gone.
        use std::io::Write as _;
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "read -r go; relay() { (sleep 0.002; relay) & }; relay; exit 0",
            ])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let leader = child.id();
        let _guard = Guard(leader);
        let me = of(leader)?.ok_or_else(|| io::Error::other("it is not running"))?;
        child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?
            .write_all(b"go\n")?;
        child.wait()?;
        let mut looks = 0;
        let end = Instant::now() + Duration::from_secs(1);
        while Instant::now() < end {
            let seen = presence(
                Some(i64::from(leader)),
                Some(&me.boot_id),
                Some(&me.start_id),
            )?;
            assert!(
                matches!(&seen, Presence::Leaderless(l) if l.pgid == leader),
                "after {looks}: {seen:?}"
            );
            looks += 1;
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(looks > 100);
        Ok(())
    }

    #[test]
    fn a_group_scan_that_fails_is_an_error_not_an_empty_group() -> io::Result<()> {
        // M2T05R-200: an unreadable /proc, or a stat that cannot be read for another reason than the process having
        // gone, never reads as an empty group.
        assert!(group_left_in(std::path::Path::new("/nonexistent-proc"), 1, 1).is_err());
        let fake = tempfile::tempdir()?;
        std::fs::create_dir(fake.path().join("42"))?;
        std::fs::create_dir(fake.path().join("42/stat"))?;
        assert!(group_left_in(fake.path(), 7, 7).is_err());
        // A process that ended while the scan ran (its directory without a stat) is skipped, not an error
        // (M2T05R-356).
        let only = tempfile::tempdir()?;
        std::fs::create_dir(only.path().join("43"))?;
        std::fs::create_dir(only.path().join("9"))?;
        std::fs::write(only.path().join("9/stat"), "9 (sh) S 1 7 7 0")?;
        std::fs::create_dir(only.path().join("7"))?;
        std::fs::write(only.path().join("7/stat"), "7 (sh) S 1 7 7 0")?;
        std::fs::write(only.path().join("self"), "")?;
        assert_eq!(group_left_in(only.path(), 7, 7)?, [9]);
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
