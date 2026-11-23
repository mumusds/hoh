//! An attempt's processes outside its process group, found by the /proc scan (docs/adr/ADR-021.md decision 8d): the
//! degraded tracking when the host has no delegated cgroup, and the marker scan that is part of the proof with one.
//! Every path that ends an attempt or releases its writer lease (the cancel, the executor's report of its leader's
//! end, the settle of an expired lease or a lost executor, a host's start reconcile) first proves none of them alive.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The environment variable an executor starts an attempt's leader with, its value [`super::Work::marker`] (the
/// project's id and the attempt's). Every descendant inherits it, so the attempt's processes are found wherever they
/// went: another group, another session, another parent.
pub const ATTEMPT_ENV: &str = "HOH_ATTEMPT_ID";

/// How often a look scans for them while the group lives.
pub(super) const RESCAN: Duration = Duration::from_millis(100);

/// How often a stop looks again.
const LOOK_EVERY: Duration = Duration::from_millis(10);

/// A process as `/proc/<pid>/stat` shows it: its parent and its start time (fields 4 and 22), whether it is a zombie
/// (field 3, Z or X), whether it is exiting (PF_EXITING in its flags, field 9), and its thread count (field 20, taken
/// in the same read).
pub(super) struct Proc {
    pub(super) pid: u32,
    pub(super) ppid: u32,
    pub(super) start: u64,
    pub(super) zombie: bool,
    pub(super) exiting: bool,
    pub(super) threads: u64,
}

/// The kernel's PF_EXITING in a `stat`'s flags (include/linux/sched.h): the task is in its exit.
const PF_EXITING: u64 = 0x4;

/// What reading one process's `stat` gave.
enum Stat {
    Read(Proc),
    /// It is gone (it exited between the listing and the read).
    Gone,
    /// It could not be read or understood: whether it is the attempt's cannot be told.
    Unreadable,
}

/// `<root>/<pid>/stat` (root `/proc`) read as bytes (a process's name need not be UTF-8, M2T06R-101).
fn stat(root: &Path, pid: u32) -> Stat {
    let bytes = match std::fs::read(root.join(pid.to_string()).join("stat")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
            return Stat::Gone;
        }
        // Another user's process under a /proc mounted with hidepid: not one of the attempt's, which run as the host's
        // user (an agent's descendant that changed user keeps a readable stat on a usual /proc).
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Stat::Gone,
        Err(_) => return Stat::Unreadable,
    };
    parse_stat(pid, &bytes).map_or(Stat::Unreadable, Stat::Read)
}

/// The fields after the name, which ends at the last `") "` (the name may hold anything, a `)` too).
fn parse_stat(pid: u32, bytes: &[u8]) -> Option<Proc> {
    let end = bytes.windows(2).rposition(|w| w == b") ")?;
    let fields: Vec<&[u8]> = bytes.get(end + 2..)?.split(|b| *b == b' ').collect();
    let number = |i: usize| -> Option<u64> {
        std::str::from_utf8(fields.get(i)?)
            .ok()?
            .trim()
            .parse()
            .ok()
    };
    Some(Proc {
        pid,
        ppid: u32::try_from(number(1)?).ok()?,
        start: number(19)?,
        zombie: fields.first().is_some_and(|f| matches!(f, [b'Z' | b'X'])),
        exiting: number(6)? & PF_EXITING != 0,
        threads: number(17)?,
    })
}

/// Every process `root` (/proc) shows, and whether each was read (or had gone): a scan with one that could not be read
/// proves nothing.
fn processes(root: &Path) -> (Vec<Proc>, bool) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return (Vec::new(), false);
    };
    let mut complete = true;
    let mut all = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        match stat(root, pid) {
            Stat::Read(process) => all.push(process),
            Stat::Gone => {}
            Stat::Unreadable => complete = false,
        }
    }
    (all, complete)
}

/// The pid most recently allocated in this pid namespace (`<root>/sys/kernel/ns_last_pid`).
fn last_pid(root: &Path) -> Option<u32> {
    std::fs::read_to_string(root.join("sys/kernel/ns_last_pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// How many pids a scan looks at one by one at most for one span of allocations: more (a wrap of the pid space, a fork
/// storm) leaves the scan incomplete.
const SWEEP_SPAN: u32 = 65_536;

/// How many spans a scan sweeps before it gives up (allocations that never stop while it looks).
const SWEEPS: usize = 64;

/// The pids allocated after `from` up to `to`, in allocation order (cyclic below `pid_max`), if not too many.
fn allocated(root: &Path, from: u32, to: u32) -> Option<Vec<u32>> {
    if to > from {
        return (to - from <= SWEEP_SPAN).then(|| (from + 1..=to).collect());
    }
    let max: u32 = std::fs::read_to_string(root.join("sys/kernel/pid_max"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let span = max.checked_sub(from)?.saturating_add(to);
    (span <= SWEEP_SPAN).then(|| (from + 1..max).chain(1..=to).collect())
}

/// `pid` as a process (its thread group's leader) if it is one: a pid allocated during a scan may name a thread.
fn stat_process(root: &Path, pid: u32) -> Stat {
    let status = match std::fs::read(root.join(pid.to_string()).join("status")) {
        Ok(status) => status,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
            return Stat::Gone;
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Stat::Gone,
        Err(_) => return Stat::Unreadable,
    };
    let tgid = status
        .split(|b| *b == b'\n')
        .find_map(|line| line.strip_prefix(b"Tgid:"))
        .and_then(|v| std::str::from_utf8(v).ok()?.trim().parse::<u32>().ok());
    match tgid {
        Some(tgid) if tgid == pid => stat(root, pid),
        // A thread: its process is listed or swept on its own.
        Some(_) => Stat::Gone,
        None => Stat::Unreadable,
    }
}

/// Whether a process of an attempt other than its leader runs, a tool (docs/adr/ADR-022.md decision 19 item 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tools {
    Running,
    Absent,
    /// A process could not be read and none was found: whether one runs cannot be told.
    Unknown,
}

/// Whether a process of the attempt marked `marker` other than its leader (`leader`, pid and start time) runs, as `root`
/// (/proc) shows them now: a child of the leader, or a process with the marker in its environment or in a thread's. A
/// process runs while any of its threads does: one whose main thread ended (a zombie in its `stat`) may run on in
/// another (WD-02). Read only: nothing is pinned or signalled. What cannot be read and cannot be shown unrelated
/// leaves it unknown, which the watchdog takes as a tool (docs/adr/ADR-022.md decision 19 item 4, WD-03). Unrelated:
/// a process started before the leader (all of the attempt's descend from it), another user's (the attempt's run as
/// the host's user, and a set-user-id tool keeps its real user), or one that has ended by [`ended`]. One that cannot
/// be read and is a zombie or exiting without that proof is let end within [`SETTLE_WITHIN`] (decision 19's design
/// correction of 2026-11-17, WDF-100: an exiting process refuses its environment, and unrelated ones exit all the time).
pub(super) fn tools(root: &Path, leader: (u32, Option<u64>), marker: &str) -> Tools {
    tools_with(root, leader, marker, &mut std::thread::sleep)
}

/// How long a look lets the unreadable processes that are zombies or exiting end, how often it looks at them again,
/// and how many it waits for at most.
const SETTLE_WITHIN: Duration = Duration::from_millis(100);
const SETTLE_STEP: Duration = Duration::from_millis(10);
const SETTLE_AT_MOST: usize = 64;

/// [`tools`], `wait` its pause between looks at the processes it lets end (a test's seam).
fn tools_with(
    root: &Path,
    leader: (u32, Option<u64>),
    marker: &str,
    wait: &mut dyn FnMut(Duration),
) -> Tools {
    let (all, complete) = processes(root);
    let mut unknown = !complete;
    let is_leader = |p: &Proc| p.pid == leader.0 && leader.1.is_none_or(|start| start == p.start);
    let leads = all.iter().any(is_leader);
    let marker = format!("{ATTEMPT_ENV}={marker}").into_bytes();
    let me = rustix::process::getuid().as_raw();
    let mut ending = Vec::new();
    for process in all.iter().filter(|p| !is_leader(p)) {
        if leader.1.is_some_and(|start| process.start < start) || has_ended(process) {
            continue;
        }
        if leads && process.ppid == leader.0 {
            return Tools::Running;
        }
        match marked_tri(root, process, &marker, me) {
            Some(true) => return Tools::Running,
            Some(false) => {}
            // Refused by a process of this user: an exiting one refuses too. Let it end if it is ending, as listed
            // or read again now; one that runs stays unknown.
            None if process.zombie || process.exiting => ending.push(process),
            None => match stat(root, process.pid) {
                Stat::Gone => {}
                Stat::Read(now) if now.start != process.start || has_ended(&now) => {}
                Stat::Read(now) if now.zombie || now.exiting => ending.push(process),
                Stat::Read(_) | Stat::Unreadable => unknown = true,
            },
        }
    }
    if !unknown && !ending.is_empty() && !settled(root, ending, wait) {
        unknown = true;
    }
    if unknown {
        Tools::Unknown
    } else {
        Tools::Absent
    }
}

/// Whether `process`, as one `stat` read showed it, has ended: a zombie (Z or X) with one thread. The state and the
/// count come from the same task in the same read, the count under the sighand lock its release takes (released, it
/// reads 0); so the count of 1 is the zombie itself, no other thread of its group is left, and none can run or be
/// started again: a zombie never runs, and only a running thread starts a thread or execs (decision 19's design
/// correction of 2026-11-17; Linux fs/proc/array.c `do_task_stat`, kernel/exit.c `__exit_signal`).
fn has_ended(process: &Proc) -> bool {
    process.zombie && process.threads == 1
}

/// Whether `process` (as listed) has ended by one `stat` read now: gone, or its pid another process's (another start
/// time), so released, which happens only once every thread of it has ended (a non-leader exec hands the leader's pid
/// and start time to the execing thread and releases the old leader, so its /proc/<pid> stays); or [`has_ended`].
fn ended(root: &Path, process: &Proc) -> bool {
    match stat(root, process.pid) {
        Stat::Gone => true,
        Stat::Read(now) => now.start != process.start || has_ended(&now),
        Stat::Unreadable => false,
    }
}

/// Whether every process of `ending` ends by [`ended`] within [`SETTLE_WITHIN`], looked at every [`SETTLE_STEP`]; at most
/// [`SETTLE_AT_MOST`] of them are waited for.
fn settled(root: &Path, mut ending: Vec<&Proc>, wait: &mut dyn FnMut(Duration)) -> bool {
    if ending.len() > SETTLE_AT_MOST {
        return false;
    }
    let mut waited = Duration::ZERO;
    while waited < SETTLE_WITHIN {
        wait(SETTLE_STEP);
        waited += SETTLE_STEP;
        ending.retain(|process| !ended(root, process));
        if ending.is_empty() {
            return true;
        }
    }
    false
}

/// Whether a read failed because what it read is gone (the process or thread exited).
fn gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3)
}

/// Whether `process` has `marker` in its environment or a thread's: `None` when neither can be read and it cannot be
/// shown to be another user's (`me` is the host's user).
fn marked_tri(root: &Path, process: &Proc, marker: &[u8], me: u32) -> Option<bool> {
    let has = |env: &[u8]| env.split(|b| *b == 0).any(|entry| entry == marker);
    let dir = root.join(process.pid.to_string());
    match std::fs::read(dir.join("environ")) {
        Ok(env) if has(&env) => return Some(true),
        // Readable and without it; a zombie main thread's may say nothing of its threads.
        Ok(_) if !process.zombie => return Some(false),
        Err(e) if gone(&e) => return Some(false),
        Ok(_) | Err(_) => {}
    }
    // Its threads' environments (the main thread's may have ended, or be refused): one readable settles it.
    let mut readable = false;
    if let Ok(tasks) = std::fs::read_dir(dir.join("task")) {
        for task in tasks.flatten() {
            if let Ok(env) = std::fs::read(task.path().join("environ")) {
                if has(&env) {
                    return Some(true);
                }
                readable = true;
            }
        }
    }
    if readable {
        return Some(false);
    }
    match std::fs::read_to_string(dir.join("status")) {
        Ok(status) => {
            let uid = status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u32>().ok());
            match uid {
                Some(uid) if uid != me => Some(false),
                _ => None,
            }
        }
        Err(e) if gone(&e) => Some(false),
        Err(_) => None,
    }
}

/// Whether `pid`'s cgroup (`<root>/<pid>/cgroup`, readable even for a non-dumpable process) could hold a process of an
/// attempt tracked by a cgroup: inside the user's manager (a `user@<me>.service` component), where the attempt's
/// cgroup is and where its processes can move themselves; never a login session's scope or another system-owned cgroup,
/// which this user may not move a process into. What cannot be told counts: a cgroup that cannot be read, a path with no
/// cgroup v2 line, or a host whose own cgroup (`<root>/self/cgroup`) does not show its user's manager (another cgroup
/// namespace's view).
fn in_users_manager(root: &Path, pid: u32, me: u32) -> bool {
    let unit = format!("user@{me}.service");
    let inside = |text: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix("0::"))
            .map(|path| path.split('/').any(|part| part == unit))
    };
    if std::fs::read_to_string(root.join("self/cgroup"))
        .ok()
        .and_then(|own| inside(&own))
        != Some(true)
    {
        return true;
    }
    match std::fs::read_to_string(root.join(pid.to_string()).join("cgroup")) {
        Ok(text) => inside(&text) != Some(false),
        Err(e) if gone(&e) => false,
        Err(_) => true,
    }
}

/// A process of an attempt outside its group, pinned by a pidfd.
pub(super) struct Pinned {
    pid: u32,
    start: u64,
    pidfd: rustix::fd::OwnedFd,
}

impl Pinned {
    /// `pid` pinned if it is still the process that started at `start` once its pidfd is open.
    fn open(pid: u32, start: u64) -> Option<Pinned> {
        let raw = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)?;
        let pidfd = rustix::process::pidfd_open(raw, rustix::process::PidfdFlags::empty()).ok()?;
        matches!(stat(Path::new("/proc"), pid), Stat::Read(p) if p.start == start)
            .then_some(Pinned { pid, start, pidfd })
    }

    /// Whether it has not exited (its pidfd is not readable yet).
    fn alive(&self) -> bool {
        let mut fds = [rustix::event::PollFd::new(
            &self.pidfd,
            rustix::event::PollFlags::IN,
        )];
        let none = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        !matches!(rustix::event::poll(&mut fds, Some(&none)), Ok(n) if n > 0)
    }

    fn signal(&self, signal: rustix::process::Signal) -> bool {
        self.alive() && rustix::process::pidfd_send_signal(&self.pidfd, signal).is_ok()
    }
}

/// Two proofs together: done only when both are; incomplete when either is.
pub(super) fn both(a: Proof, b: Proof) -> Proof {
    match (a, b) {
        (Proof::Done, Proof::Done) => Proof::Done,
        (Proof::Incomplete, _) | (_, Proof::Incomplete) => Proof::Incomplete,
        _ => Proof::Pending,
    }
}

/// Where the proof stands after a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Proof {
    /// Two complete scans in a row found none alive: none is left.
    Done,
    /// One is alive, or the last scan found none but is the first in a row.
    Pending,
    /// The last scan could not read every process: nothing can be concluded from it.
    Incomplete,
}

/// An attempt's processes outside its group: found by the /proc parent chain from its leader (while it is known) and
/// from every pinned one, and by its marker in their environment; pinned, signalled through their pidfds.
pub(super) struct Outside {
    /// The leader, pinned as recorded (pid and start time): a root of the parent chain only while it lives, so a pid
    /// reused after its reap names no root (M2T06R-151).
    leader: Option<Pinned>,
    /// `ATTEMPT_ENV=<marker>` as it appears in an environment.
    marker: Vec<u8>,
    /// The leader's start time as recorded, kept when the leader is no longer pinned: every process of the attempt
    /// descends from the leader, so one started before it is not the attempt's (M2R-500, as WD-03).
    born: Option<u64>,
    /// Whether the attempt is tracked by its delegated cgroup: then an unreadable environment counts only inside the
    /// user's manager (see [`Outside::by_cgroup`]).
    by_cgroup: bool,
    pins: Vec<Pinned>,
    /// The strongest signal sent so far: one found later gets it at once.
    sent: Option<rustix::process::Signal>,
    /// Complete scans in a row that found none alive.
    quiet: u8,
    scanned: Option<Instant>,
    /// Where processes are listed: `/proc` (a test gives a tree of its own).
    root: PathBuf,
    /// Runs between a scan's listing and its first reads of environments (a test makes a process fork and exit there).
    #[cfg(test)]
    after_listing: Option<Box<dyn FnMut() + Send>>,
    /// Runs after a scan found the attempt's processes and before it pins them (a test makes one go there).
    #[cfg(test)]
    after_finding: Option<Box<dyn FnMut() + Send>>,
}

impl Outside {
    pub(super) fn new(marker: &str) -> Outside {
        Outside {
            leader: None,
            marker: format!("{ATTEMPT_ENV}={marker}").into_bytes(),
            born: None,
            by_cgroup: false,
            pins: Vec::new(),
            sent: None,
            quiet: 0,
            scanned: None,
            root: PathBuf::from("/proc"),
            #[cfg(test)]
            after_listing: None,
            #[cfg(test)]
            after_finding: None,
        }
    }

    /// Where processes are listed instead of `/proc` (a test's fault hook gives a tree of its own).
    pub(super) fn with_root(mut self, root: Option<PathBuf>) -> Outside {
        if let Some(root) = root {
            self.root = root;
        }
        self
    }

    /// The leader's start time as recorded (`attempts.process_start_id`), if known: what tells an older process whose
    /// environment cannot be read not to be the attempt's.
    pub(super) fn born(mut self, start_id: Option<&str>) -> Outside {
        self.born = start_id.and_then(|s| s.parse::<u64>().ok()).or(self.born);
        self
    }

    /// Whether the attempt is tracked by its delegated cgroup (`Tracking::Cgroup`). The rule of M2R-500 (an unreadable
    /// environment of this user's process started after the leader leaves the scan incomplete) then holds only for a
    /// process that could have moved there from the attempt's cgroup: one inside the user's manager (CTO decision,
    /// 2026-11-21; [`in_users_manager`]). A later login's sshd session (in its session scope) or another process in a
    /// system-owned cgroup cannot hold every attempt's proof, and its writer lease, until it ends. In /proc tracking
    /// every such process counts: that cost is ADR-021 8d's stated residual.
    pub(super) fn by_cgroup(mut self, by_cgroup: bool) -> Outside {
        self.by_cgroup = by_cgroup;
        self
    }

    /// The group was seen alive: the scans that count for the proof start again after it (M2T06R-252).
    pub(super) fn group_alive(&mut self) {
        self.quiet = 0;
    }

    /// Its leader as recorded (`attempts.pid` and `process_start_id`), pinned for the parent chain if it is still that
    /// process; kept if it is already pinned.
    pub(super) fn set_leader(&mut self, pid: Option<i64>, start_id: Option<&str>) {
        let wanted = pid
            .and_then(|p| u32::try_from(p).ok())
            .zip(start_id.and_then(|s| s.parse::<u64>().ok()));
        if let Some((_, start)) = wanted {
            self.born = Some(start);
        }
        if self
            .leader
            .as_ref()
            .is_some_and(|l| Some((l.pid, l.start)) == wanted)
        {
            return;
        }
        self.leader = wanted.and_then(|(pid, start)| Pinned::open(pid, start));
    }

    /// Look for them now: pin the new ones (sending them the strongest signal so far) and say where the proof stands.
    pub(super) fn scan(&mut self) -> Proof {
        self.scanned = Some(Instant::now());
        // The listing, then every pid allocated since it began, each batch's environments read before the next look
        // at the allocations, until none was allocated meanwhile (decision 8d v4, M2T06R-200): a marked process alive
        // at the end was either looked at here or forked by a marked process looked at here.
        let Some(mut last) = last_pid(&self.root) else {
            self.quiet = 0;
            return Proof::Incomplete;
        };
        let (mut processes, mut complete) = processes(&self.root);
        #[cfg(test)]
        if let Some(hook) = self.after_listing.as_mut() {
            hook();
        }
        let mut seen: BTreeSet<u32> = processes.iter().map(|p| p.pid).collect();
        let mut by_marker: BTreeSet<u32> = BTreeSet::new();
        let mut batch: Vec<usize> = (0..processes.len()).collect();
        let mut settled = false;
        let me = rustix::process::getuid().as_raw();
        for _ in 0..=SWEEPS {
            for at in batch.drain(..) {
                let process = &processes[at];
                match marked_tri(&self.root, process, &self.marker, me) {
                    Some(true) => {
                        by_marker.insert(process.pid);
                    }
                    Some(false) => {}
                    // Neither its environment nor a thread's could be read, and it is this user's (a non-dumpable or
                    // set-user-id process refuses its environment, M2R-500): unrelated only if it started before the
                    // leader or has ended; else it may be the attempt's, and the scan proves nothing (as WD-03). With
                    // no leader start recorded nothing bounds it (the user's own service manager refuses its
                    // environment too): not counted, the stated residual of such a record (a STARTING attempt with no
                    // process, held at its gate, has none outside; a pid recorded without a start is a legacy row).
                    None if self.born.is_none_or(|born| process.start < born)
                        || has_ended(process) => {}
                    // With the attempt's cgroup, only one that could have moved there from it ([`Outside::by_cgroup`]).
                    None if self.by_cgroup && !in_users_manager(&self.root, process.pid, me) => {}
                    None => complete = false,
                }
            }
            let Some(now) = last_pid(&self.root) else {
                break;
            };
            if now == last {
                settled = true;
                break;
            }
            let Some(pids) = allocated(&self.root, last, now) else {
                break;
            };
            for pid in pids {
                if !seen.insert(pid) {
                    continue;
                }
                match stat_process(&self.root, pid) {
                    Stat::Read(process) => {
                        batch.push(processes.len());
                        processes.push(process);
                    }
                    Stat::Gone => {}
                    Stat::Unreadable => complete = false,
                }
            }
            last = now;
        }
        complete = complete && settled;
        let mut found: BTreeSet<u32> = BTreeSet::new();
        let mut parents: Vec<u32> = self
            .leader
            .iter()
            .chain(self.pins.iter())
            .filter(|p| p.alive())
            .map(|p| p.pid)
            .collect();
        while let Some(parent) = parents.pop() {
            for process in processes.iter().filter(|p| p.ppid == parent) {
                if found.insert(process.pid) {
                    parents.push(process.pid);
                }
            }
        }
        found.extend(by_marker);
        #[cfg(test)]
        if let Some(hook) = self.after_finding.as_mut() {
            hook();
        }
        for process in processes.iter().filter(|p| found.contains(&p.pid)) {
            if self
                .leader
                .as_ref()
                .is_some_and(|l| l.pid == process.pid && l.start == process.start)
                || self
                    .pins
                    .iter()
                    .any(|q| q.pid == process.pid && q.start == process.start)
            {
                continue;
            }
            match Pinned::open(process.pid, process.start) {
                Some(pin) => {
                    if let Some(signal) = self.sent {
                        let _ = pin.signal(signal);
                    }
                    self.pins.push(pin);
                }
                // Gone since the listing, or another process now: looked at again next time.
                None => complete = complete && matches!(stat(&self.root, process.pid), Stat::Gone),
            }
        }
        self.pins.retain(Pinned::alive);
        if !complete {
            self.quiet = 0;
            return Proof::Incomplete;
        }
        // A scan that found any process of the attempt is not quiet, even if it is gone by now: it may have forked
        // after the last look at the allocations (M2T06R-300, -350).
        if !found.is_empty() {
            self.quiet = 0;
            return Proof::Pending;
        }
        if !self.pins.is_empty() {
            self.quiet = 0;
            return Proof::Pending;
        }
        self.quiet = self.quiet.saturating_add(1);
        if self.quiet >= 2 {
            Proof::Done
        } else {
            Proof::Pending
        }
    }

    /// Scan if the last scan is older than [`RESCAN`]; whether a pinned one lives.
    pub(super) fn keep_up(&mut self) -> bool {
        if self.scanned.is_none_or(|at| at.elapsed() >= RESCAN) {
            let _ = self.scan();
        }
        self.pins.iter().any(Pinned::alive)
    }

    /// Send `signal` to every pinned one alive; whether any got it. Later ones get it too, at once.
    pub(super) fn signal(&mut self, signal: rustix::process::Signal) -> bool {
        if self.sent != Some(rustix::process::Signal::KILL) {
            self.sent = Some(signal);
        }
        self.pins
            .iter()
            .map(|p| p.signal(signal))
            .fold(false, |a, b| a | b)
    }

    /// Stop them as a group is stopped (SIGTERM, `term_grace`, SIGKILL, `kill_grace`), the attempt's group already
    /// stopped or gone (blocking): the steps taken and where the proof ended.
    pub(super) fn stop(
        &mut self,
        term_grace: Duration,
        kill_grace: Duration,
    ) -> (Vec<String>, Proof) {
        // Only scans made after the group was stopped count for the proof (M2T06R-252).
        self.group_alive();
        let mut steps = Vec::new();
        let mut proof = self.scan();
        for (signal, name, grace) in [
            (rustix::process::Signal::TERM, "outside_sigterm", term_grace),
            (rustix::process::Signal::KILL, "outside_sigkill", kill_grace),
        ] {
            if proof == Proof::Done {
                break;
            }
            if self.signal(signal) {
                steps.push(name.to_owned());
            }
            let end = Instant::now() + grace;
            loop {
                std::thread::sleep(LOOK_EVERY);
                proof = self.scan();
                if proof == Proof::Done || Instant::now() >= end {
                    break;
                }
            }
        }
        if proof == Proof::Done && !steps.is_empty() {
            steps.push("outside_reaped".to_owned());
        }
        if proof == Proof::Incomplete {
            steps.push("outside_unreadable".to_owned());
        }
        (steps, proof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_proofs_together_are_done_only_when_both_are() {
        // M2T06R-550 (decision 8d v6): the cgroup's proof and the marker scan's, on paths 2-4.
        use Proof::*;
        for (a, b, both_) in [
            (Done, Done, Done),
            (Done, Pending, Pending),
            (Pending, Done, Pending),
            (Pending, Pending, Pending),
            (Done, Incomplete, Incomplete),
            (Incomplete, Done, Incomplete),
            (Pending, Incomplete, Incomplete),
            (Incomplete, Pending, Incomplete),
            (Incomplete, Incomplete, Incomplete),
        ] {
            assert_eq!(both(a, b), both_, "{a:?} and {b:?}");
        }
    }

    #[test]
    fn a_stat_is_read_as_bytes_up_to_the_last_parenthesis() {
        // M2T06R-101: a name that is not UTF-8, and one with ") " in it.
        let mut line =
            b"42 (w\xffr) S 7 42 42 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 12345 0".to_vec();
        let p = parse_stat(42, &line).map(|p| (p.ppid, p.start));
        assert_eq!(p, Some((7, 12345)));
        line = b"43 (a) b) S 9 43 43 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 777 0".to_vec();
        assert_eq!(
            parse_stat(43, &line).map(|p| (p.ppid, p.start)),
            Some((9, 777))
        );
        assert!(parse_stat(44, b"44 no name").is_none());
    }

    /// Kills and reaps a process this test spawned. `group` signals the process group, so a shell and the child it
    /// backgrounded both end. Drop covers every return, including `?` and an early `Err`.
    struct Reap {
        child: std::process::Child,
        group: bool,
    }

    impl Reap {
        fn process(child: std::process::Child) -> Self {
            Self {
                child,
                group: false,
            }
        }

        fn group(child: std::process::Child) -> Self {
            Self { child, group: true }
        }

        fn id(&self) -> u32 {
            self.child.id()
        }
    }

    impl Drop for Reap {
        fn drop(&mut self) {
            let pid = self.child.id();
            if let Some(raw) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let signal = rustix::process::Signal::KILL;
                if self.group {
                    let _ = rustix::process::kill_process_group(raw, signal);
                } else {
                    let _ = rustix::process::kill_process(raw, signal);
                }
            }
            let _ = self.child.wait();
        }
    }

    fn same_process(pid: u32, start: u64) -> bool {
        match stat(Path::new("/proc"), pid) {
            Stat::Read(p) => p.start == start,
            _ => false,
        }
    }

    /// The start time of `pid` once `/proc` shows it, or an error when it never does.
    fn start_of(pid: u32) -> Result<u64, String> {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Stat::Read(p) = stat(Path::new("/proc"), pid) {
                return Ok(p.start);
            }
            if Instant::now() > end {
                return Err(format!("pid {pid} had no stat"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_drop_guard_kills_and_waits_the_decoy_and_the_parent_group()
    -> Result<(), Box<dyn std::error::Error>> {
        // FLK34-52 / FLK34-01: letting the guard go, with no later kill, ends the decoy and the parent group and
        // reaps the children this process spawned. A zombie still has its start time, so a kill without a wait fails.
        use std::os::unix::process::CommandExt as _;
        let parent = Reap::group(
            std::process::Command::new("sh")
                .args(["-c", "sleep 30 & wait"])
                .process_group(0)
                .spawn()?,
        );
        let parent_pid = parent.id();
        let parent_start = start_of(parent_pid)?;
        let end = Instant::now() + Duration::from_secs(5);
        let child_pid = loop {
            let (all, _) = processes(Path::new("/proc"));
            if let Some(child) = all.iter().find(|p| p.ppid == parent_pid) {
                break child.pid;
            }
            if Instant::now() > end {
                return Err(format!("parent {parent_pid} grew no child").into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let child_start = start_of(child_pid)?;
        let decoy = Reap::process(std::process::Command::new("sleep").args(["60"]).spawn()?);
        let decoy_pid = decoy.id();
        let decoy_start = start_of(decoy_pid)?;
        drop(decoy);
        drop(parent);
        // The shell and the decoy are this process's children: `wait` in the guard has reaped them. The shell's
        // sleep is not; the group signal has it exiting, and init reaps it. A kill that missed it leaves it running.
        let parent_alive = same_process(parent_pid, parent_start);
        let decoy_alive = same_process(decoy_pid, decoy_start);
        let child_by = Instant::now() + Duration::from_secs(2);
        let mut child_alive = same_process(child_pid, child_start);
        while child_alive && Instant::now() < child_by {
            std::thread::sleep(Duration::from_millis(10));
            child_alive = same_process(child_pid, child_start);
        }
        // The assertion is about the guard. If it did not reap, kill these exact pids so the red leaves nothing.
        for (pid, start, group) in [
            (parent_pid, parent_start, true),
            (child_pid, child_start, false),
            (decoy_pid, decoy_start, false),
        ] {
            if !same_process(pid, start) {
                continue;
            }
            if let Some(raw) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                if group {
                    let _ = rustix::process::kill_process_group(raw, rustix::process::Signal::KILL);
                } else {
                    let _ = rustix::process::kill_process(raw, rustix::process::Signal::KILL);
                }
                let _ = rustix::process::waitpid(Some(raw), rustix::process::WaitOptions::empty());
            }
        }
        assert!(
            !parent_alive && !child_alive && !decoy_alive,
            "drop left parent {parent_pid} alive={parent_alive} child {child_pid} alive={child_alive} decoy {decoy_pid} alive={decoy_alive}"
        );
        Ok(())
    }

    #[test]
    fn the_leader_roots_the_parent_chain_only_as_the_process_recorded()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-151: a process with a child, unmarked. Named as the leader with another start time (a pid reused), it
        // roots nothing; named with its own, its child is found and pinned.
        // FLK-03: the marker is this test's own. A fixed `p/A1` is what the adapter gates put on live processes
        // (`HOH_ATTEMPT_ID=p/A1`); a scan of real /proc then pins those too (one such process turned (0, 1) into
        // (1, 2); two, the (2, 3) seen under a full parallel run). The decoy keeps that fixed marker and must not
        // be pinned.
        use std::os::unix::process::CommandExt as _;
        let parent = Reap::group(
            std::process::Command::new("sh")
                .args(["-c", "sleep 30 & wait"])
                .process_group(0)
                .spawn()?,
        );
        let pid = parent.id();
        let end = Instant::now() + Duration::from_secs(5);
        let has_child = || {
            let (all, _) = processes(Path::new("/proc"));
            all.iter().any(|p| p.ppid == pid)
        };
        while !has_child() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        let start = match stat(Path::new("/proc"), pid) {
            Stat::Read(p) => p.start,
            _ => 0,
        };
        // Held until its environment shows the foreign marker, then for the whole scan.
        let decoy = Reap::process(
            std::process::Command::new("sleep")
                .args(["60"])
                .env(ATTEMPT_ENV, "p/A1")
                .spawn()?,
        );
        let decoy_pid = decoy.id();
        let needle = format!("{ATTEMPT_ENV}=p/A1").into_bytes();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut visible = false;
        while Instant::now() < deadline {
            if std::fs::read(format!("/proc/{decoy_pid}/environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|e| e == needle))
            {
                visible = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !visible {
            return Err(format!("decoy {decoy_pid} never showed {ATTEMPT_ENV}=p/A1").into());
        }
        let marker = format!("flk03-{}-{pid}", std::process::id());
        let mut reused = Outside::new(&marker);
        reused.set_leader(Some(i64::from(pid)), Some(&(start + 1).to_string()));
        let _ = reused.scan();
        let mut recorded = Outside::new(&marker);
        recorded.set_leader(Some(i64::from(pid)), Some(&start.to_string()));
        let _ = recorded.scan();
        let pinned = (reused.pins.len(), recorded.pins.len());
        let reused_pids: Vec<u32> = reused.pins.iter().map(|p| p.pid).collect();
        let recorded_pids: Vec<u32> = recorded.pins.iter().map(|p| p.pid).collect();
        // `parent` and `decoy` are reaped by their drop guard, on this path and on every earlier return.
        drop(decoy);
        drop(parent);
        assert!(
            !reused_pids.contains(&decoy_pid) && !recorded_pids.contains(&decoy_pid),
            "the foreign p/A1 process {decoy_pid} was pinned: reused {reused_pids:?} recorded {recorded_pids:?}"
        );
        assert_eq!(
            pinned,
            (0, 1),
            "decoy {decoy_pid} reused {reused_pids:?} recorded {recorded_pids:?}"
        );
        Ok(())
    }

    #[test]
    fn a_marked_process_that_forks_and_exits_during_a_scan_leaves_its_child_found()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-200 (decision 8d v4): between the scan's listing and its reads of environments, the marked process
        // forks a child and exits, reaped. The child was not listed and its parent's environment is gone; the sweep of
        // the pids allocated meanwhile finds the child by its marker.
        use std::io::BufRead as _;
        use std::os::unix::process::CommandExt as _;
        let marker = format!("test-{}/A2", std::process::id());
        let mut hopper = std::process::Command::new("python3")
            .args([
                "-c",
                "import os, signal, time\n\
                 def hop(*_):\n    if os.fork() == 0:\n        time.sleep(30)\n        os._exit(0)\n    os._exit(0)\n\
                 signal.signal(signal.SIGUSR1, hop)\nprint('ready', flush=True)\ntime.sleep(30)\n",
            ])
            .env(ATTEMPT_ENV, &marker)
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .spawn()?;
        let pid = hopper.id();
        let mut ready = String::new();
        std::io::BufReader::new(hopper.stdout.take().ok_or("no stdout")?).read_line(&mut ready)?;
        let child_of_hopper = std::sync::Arc::new(std::sync::Mutex::new(None::<u32>));
        let seen = child_of_hopper.clone();
        let mut outside = Outside::new(&marker);
        outside.after_listing = Some(Box::new(move || {
            if let Some(p) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(p, rustix::process::Signal::USR1);
            }
            let _ = hopper.wait();
            // Its child, found without the scan: in the hopper's group, which outlives it.
            let end = Instant::now() + Duration::from_secs(5);
            while Instant::now() < end {
                let (all, _) = processes(Path::new("/proc"));
                let found = all.iter().find(|p| {
                    p.pid != pid
                        && std::fs::read(format!("/proc/{}/stat", p.pid))
                            .is_ok_and(|s| parse_group(&s) == Some(pid))
                });
                if let Some(child) = found {
                    if let Ok(mut c) = seen.lock() {
                        *c = Some(child.pid);
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }));
        let proof = outside.scan();
        let pinned: Vec<u32> = outside.pins.iter().map(|p| p.pid).collect();
        if let Some(group) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let child = child_of_hopper
            .lock()
            .map(|c| *c)
            .unwrap_or(None)
            .ok_or("no child")?;
        assert_eq!(pinned, [child], "the child forked during the scan");
        assert_eq!(proof, Proof::Pending);
        Ok(())
    }

    /// A process's group (`stat` field 5).
    fn parse_group(stat: &[u8]) -> Option<u32> {
        let end = stat.windows(2).rposition(|w| w == b") ")?;
        let fields: Vec<&[u8]> = stat.get(end + 2..)?.split(|b| *b == b' ').collect();
        std::str::from_utf8(fields.get(2)?).ok()?.parse().ok()
    }

    #[test]
    fn a_marked_process_that_cannot_be_pinned_makes_the_scan_incomplete()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-251: a marked process is listed but its pidfd cannot be opened (it is no real process here) while its
        // stat still reads: nothing is concluded.
        let root = tempfile::tempdir()?;
        std::fs::create_dir_all(root.path().join("sys/kernel"))?;
        std::fs::write(root.path().join("sys/kernel/ns_last_pid"), b"1000\n")?;
        let dir = root.path().join("4000000");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("stat"),
            b"4000000 (x) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 9 0",
        )?;
        std::fs::write(dir.join("environ"), format!("{ATTEMPT_ENV}=p/A1\0"))?;
        let mut outside = Outside::new("p/A1").with_root(Some(root.path().to_owned()));
        assert_eq!(outside.scan(), Proof::Incomplete);
        assert_eq!(outside.scan(), Proof::Incomplete);
        Ok(())
    }

    #[test]
    fn a_stop_counts_only_its_own_scans_for_the_proof() -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-252: a scan before the group's stop found nothing; the stop still needs two scans of its own.
        let root = tempfile::tempdir()?;
        std::fs::create_dir_all(root.path().join("sys/kernel"))?;
        std::fs::write(root.path().join("sys/kernel/ns_last_pid"), b"1000\n")?;
        let scans = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = scans.clone();
        let mut outside = Outside::new("p/A1").with_root(Some(root.path().to_owned()));
        outside.after_listing = Some(Box::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        assert_eq!(outside.scan(), Proof::Pending);
        scans.store(0, std::sync::atomic::Ordering::SeqCst);
        let (steps, proof) = outside.stop(Duration::from_secs(5), Duration::from_secs(5));
        assert_eq!((steps, proof), (Vec::<String>::new(), Proof::Done));
        assert_eq!(scans.load(std::sync::atomic::Ordering::SeqCst), 2);
        Ok(())
    }

    #[test]
    fn a_sweep_that_does_not_settle_leaves_the_scan_incomplete()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-351 (decision 8d step 3): between the listing and the sweep the allocations jump past the span limit,
        // or wrap around with too many pids; either way the scan proves nothing.
        for (jump, pid_max) in [(70_000_u64, 4_194_304_u64), (99_050, 100_000)] {
            let root = tempfile::tempdir()?;
            std::fs::create_dir_all(root.path().join("sys/kernel"))?;
            std::fs::write(root.path().join("sys/kernel/ns_last_pid"), b"1000\n")?;
            std::fs::write(root.path().join("sys/kernel/pid_max"), pid_max.to_string())?;
            let last = root.path().join("sys/kernel/ns_last_pid");
            let mut outside = Outside::new("p/A1").with_root(Some(root.path().to_owned()));
            // Each scan, `jump` pids are allocated after the first look (the second case wraps below pid_max).
            outside.after_listing = Some(Box::new(move || {
                let now: u64 = std::fs::read_to_string(&last)
                    .ok()
                    .and_then(|t| t.trim().parse().ok())
                    .unwrap_or(1000);
                let _ = std::fs::write(&last, ((now + jump) % pid_max).to_string());
            }));
            assert_eq!(outside.scan(), Proof::Incomplete, "{pid_max}");
            assert_eq!(outside.scan(), Proof::Incomplete, "{pid_max}");
        }
        Ok(())
    }

    #[test]
    fn a_scan_that_found_a_process_of_the_attempt_is_not_quiet_even_if_it_is_gone_by_its_end()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-300 (degraded proc mode): a marked process is found, then gone before it can be pinned (it may have
        // forked after the last look at the allocations); that scan does not count, so the next one is only the first.
        let root = tempfile::tempdir()?;
        std::fs::create_dir_all(root.path().join("sys/kernel"))?;
        std::fs::write(root.path().join("sys/kernel/ns_last_pid"), b"1000\n")?;
        let dir = root.path().join("4000000");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("stat"),
            b"4000000 (x) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 9 0",
        )?;
        std::fs::write(dir.join("environ"), format!("{ATTEMPT_ENV}=p/A1\0"))?;
        let mut outside = Outside::new("p/A1").with_root(Some(root.path().to_owned()));
        let gone = dir.clone();
        outside.after_finding = Some(Box::new(move || {
            let _ = std::fs::remove_dir_all(&gone);
        }));
        assert_eq!(outside.scan(), Proof::Pending);
        assert_eq!(
            outside.scan(),
            Proof::Pending,
            "the scan that found it counted as quiet"
        );
        assert_eq!(outside.scan(), Proof::Done);
        Ok(())
    }

    #[test]
    fn a_process_whose_environment_cannot_be_read_is_read_by_its_threads()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-456 (M2T06R-301 in the degraded proc mode): its main thread exited, so its environ is refused; a
        // thread still running has the marker.
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir()?;
        let dir = root.path().join("500");
        std::fs::create_dir_all(dir.join("task/501"))?;
        std::fs::write(dir.join("environ"), b"")?;
        std::fs::set_permissions(dir.join("environ"), std::fs::Permissions::from_mode(0o000))?;
        std::fs::write(
            dir.join("task/501/environ"),
            format!("{ATTEMPT_ENV}=p/A1\0"),
        )?;
        let marker = format!("{ATTEMPT_ENV}=p/A1").into_bytes();
        let me = rustix::process::getuid().as_raw();
        let process = parse_stat(
            500,
            b"500 (tool) S 1 500 500 0 -1 0 0 0 0 0 0 0 0 0 20 0 2 0 900 0",
        )
        .ok_or("no stat")?;
        let found = marked_tri(root.path(), &process, &marker, me);
        let other = marked_tri(root.path(), &process, b"HOH_ATTEMPT_ID=other/A9", me);
        std::fs::set_permissions(dir.join("environ"), std::fs::Permissions::from_mode(0o600))?;
        assert_eq!(found, Some(true));
        assert_eq!(other, Some(false));
        Ok(())
    }

    /// A /proc tree of its own: `(pid, stat after the pid, environment)` each.
    fn tree(processes: &[(u32, &str, &[u8])]) -> std::io::Result<tempfile::TempDir> {
        let root = tempfile::tempdir()?;
        for (pid, stat, environ) in processes {
            let dir = root.path().join(pid.to_string());
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("stat"), format!("{pid} {stat}"))?;
            std::fs::write(dir.join("environ"), environ)?;
        }
        Ok(root)
    }

    const MARKED: &[u8] = b"PATH=/bin\0HOH_ATTEMPT_ID=p/A1\0";
    const LEADER: &str = "(agent) S 1 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 500 0";

    #[test]
    fn a_child_of_the_leader_is_a_tool_and_the_leader_alone_is_none()
    -> Result<(), Box<dyn std::error::Error>> {
        // Decision 19 item 4: the leader (pid 100, started at 500) carries the marker itself; a child of it runs.
        let alone = tree(&[
            (100, LEADER, MARKED),
            (7, "(init) S 0 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 1 0", b""),
        ])?;
        assert_eq!(tools(alone.path(), (100, Some(500)), "p/A1"), Tools::Absent);
        let child = "(sleep) S 100 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 900 0";
        let with = tree(&[(100, LEADER, MARKED), (101, child, b"PATH=/bin\0")])?;
        assert_eq!(tools(with.path(), (100, Some(500)), "p/A1"), Tools::Running);
        // A pid reused: the process at 100 is not the leader recorded, so its child is no tool of the attempt.
        assert_eq!(tools(with.path(), (100, Some(499)), "p/A2"), Tools::Absent);
        Ok(())
    }

    #[test]
    fn a_marked_process_outside_the_leaders_tree_is_a_tool_and_a_zombie_is_not()
    -> Result<(), Box<dyn std::error::Error>> {
        let moved = "(tool) S 1 300 300 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 800 0";
        let away = tree(&[(100, LEADER, MARKED), (300, moved, MARKED)])?;
        assert_eq!(tools(away.path(), (100, Some(500)), "p/A1"), Tools::Running);
        // Another attempt's marker is not this one's.
        assert_eq!(tools(away.path(), (100, Some(500)), "p/A11"), Tools::Absent);
        let zombie = "(sleep) Z 100 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 900 0";
        let dead = tree(&[
            (100, LEADER, MARKED),
            (101, zombie, b""),
            (102, &zombie.replace("100 100 100", "1 102 102"), MARKED),
        ])?;
        assert_eq!(tools(dead.path(), (100, Some(500)), "p/A1"), Tools::Absent);
        Ok(())
    }

    #[test]
    fn a_process_that_cannot_be_read_leaves_the_tools_unknown_unless_one_was_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let unread = tree(&[(100, LEADER, MARKED), (200, "garbage", b"")])?;
        assert_eq!(
            tools(unread.path(), (100, Some(500)), "p/A1"),
            Tools::Unknown
        );
        let child = "(sleep) S 100 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 900 0";
        let found = tree(&[
            (100, LEADER, MARKED),
            (101, child, b""),
            (200, "garbage", b""),
        ])?;
        assert_eq!(
            tools(found.path(), (100, Some(500)), "p/A1"),
            Tools::Running
        );
        Ok(())
    }

    /// A python3 process with `marker` (if any) in its environment running `code`, once it printed its first line.
    fn python(code: &str, marker: Option<&str>) -> std::io::Result<(std::process::Child, String)> {
        let mut command = std::process::Command::new("python3");
        command.args(["-c", code]).env_remove(ATTEMPT_ENV);
        if let Some(marker) = marker {
            command.env(ATTEMPT_ENV, marker);
        }
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .spawn()?;
        let mut line = String::new();
        if let Some(out) = child.stdout.take() {
            std::io::BufRead::read_line(&mut std::io::BufReader::new(out), &mut line)?;
        }
        Ok((child, line.trim().to_owned()))
    }

    /// A tool whose main thread has ended (a raw `exit` of that thread alone) while another of its threads runs; it
    /// says `running` once the main thread is gone.
    const THREAD_LIVES_ON: &str = "import ctypes, platform, threading, time
def work():
    time.sleep(0.3)
    print('running', flush=True)
    time.sleep(60)
threading.Thread(target=work).start()
ctypes.CDLL(None).syscall(ctypes.c_long({'x86_64': 60, 'aarch64': 93}[platform.machine()]), ctypes.c_long(0))";

    fn leader_of(pid: u32) -> (u32, Option<u64>) {
        match stat(Path::new("/proc"), pid) {
            Stat::Read(p) => (pid, Some(p.start)),
            _ => (pid, None),
        }
    }

    fn zombie(pid: u32) -> bool {
        matches!(stat(Path::new("/proc"), pid), Stat::Read(p) if p.zombie)
    }

    #[test]
    fn a_tool_whose_main_thread_ended_while_another_thread_runs_is_running()
    -> Result<(), Box<dyn std::error::Error>> {
        // WD-02: its thread group's leader is a zombie, its other thread runs the tool; found by its marker (it left
        // the leader's tree) and as a child of the leader.
        let marker = format!("p/WD02-{}", std::process::id());
        let (mut leader, _) = python(
            "print('up', flush=True); import time; time.sleep(60)",
            Some(&marker),
        )?;
        let (mut tool, said) = python(THREAD_LIVES_ON, Some(&marker))?;
        let marked = (
            said.clone(),
            zombie(tool.id()),
            tools(Path::new("/proc"), leader_of(leader.id()), &marker),
        );
        let _ = tool.kill();
        let _ = tool.wait();
        let spawn = format!(
            "import subprocess, sys; c = subprocess.Popen([sys.executable, '-c', {THREAD_LIVES_ON:?}], \
             stdout=subprocess.PIPE, text=True); print(c.stdout.readline().strip(), c.pid, flush=True); c.wait()"
        );
        let (mut parent, said) = python(&spawn, None)?;
        let child: u32 = said.split(' ').nth(1).unwrap_or_default().parse()?;
        let as_child = (
            said.split(' ').next().unwrap_or_default().to_owned(),
            zombie(child),
            tools(Path::new("/proc"), leader_of(parent.id()), &marker),
        );
        if let Some(pid) = rustix::process::Pid::from_raw(i32::try_from(child)?) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
        let _ = parent.kill();
        let _ = parent.wait();
        let _ = leader.kill();
        let _ = leader.wait();
        assert_eq!(marked, ("running".to_owned(), true, Tools::Running));
        assert_eq!(as_child, ("running".to_owned(), true, Tools::Running));
        Ok(())
    }

    #[test]
    fn a_tool_of_the_same_user_whose_environment_cannot_be_read_is_unknown()
    -> Result<(), Box<dyn std::error::Error>> {
        // WD-03: PR_SET_DUMPABLE 0 makes its environment unreadable (EACCES); started after the leader by the same
        // user, it cannot be shown not to be the attempt's.
        let marker = format!("p/WD03-{}", std::process::id());
        let (mut leader, _) = python(
            "print('up', flush=True); import time; time.sleep(60)",
            Some(&marker),
        )?;
        let (mut tool, said) = python(
            "import ctypes, time; assert ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0; print('hidden', flush=True); \
             time.sleep(60)",
            Some(&marker),
        )?;
        let unreadable = std::fs::read(format!("/proc/{}/environ", tool.id()))
            .err()
            .map(|e| e.kind());
        let found = tools(Path::new("/proc"), leader_of(leader.id()), &marker);
        let _ = tool.kill();
        let _ = tool.wait();
        let _ = leader.kill();
        let _ = leader.wait();
        assert_eq!(said, "hidden");
        assert_eq!(unreadable, Some(std::io::ErrorKind::PermissionDenied));
        assert_eq!(found, Tools::Unknown);
        Ok(())
    }

    #[test]
    fn a_non_dumpable_orphan_of_the_attempt_leaves_the_outside_scan_incomplete()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2R-500 (the scan's side of WD-03): its leader ended, a process of the attempt, marked, outside its leader's
        // tree (a child of another parent here, as one re-parented to init), makes itself non-dumpable (PR_SET_DUMPABLE 0), so its
        // environment is refused (EACCES) though it is this user's and started after the leader. Its marker cannot be
        // read, so no scan may prove the attempt's processes gone while it runs: every scan is incomplete. Before, the
        // refusal read as "unmarked" and two scans gave Done while it ran.
        let marker = format!("p/M2R500-{}", std::process::id());
        let (mut leader, _) = python(
            "print('up', flush=True); import time; time.sleep(60)",
            Some(&marker),
        )?;
        let (mut orphan, said) = python(
            "import ctypes, time; assert ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0; print('hidden', flush=True); \
             time.sleep(60)",
            Some(&marker),
        )?;
        let unreadable = std::fs::read(format!("/proc/{}/environ", orphan.id()))
            .err()
            .map(|e| e.kind());
        let start = match stat(Path::new("/proc"), leader.id()) {
            Stat::Read(p) => Some(p.start.to_string()),
            _ => None,
        };
        // The leader has ended (the settle after its end), as recorded.
        let _ = leader.kill();
        let _ = leader.wait();
        let mut outside = Outside::new(&marker);
        outside.set_leader(Some(i64::from(leader.id())), start.as_deref());
        let proofs: Vec<Proof> = (0..3).map(|_| outside.scan()).collect();
        let _ = orphan.kill();
        let _ = orphan.wait();
        assert_eq!(said, "hidden");
        assert_eq!(unreadable, Some(std::io::ErrorKind::PermissionDenied));
        assert!(start.is_some(), "the leader's start was not read");
        assert_eq!(proofs, [Proof::Incomplete; 3]);
        Ok(())
    }

    #[test]
    fn a_later_unrelated_non_dumpable_process_of_this_user_holds_the_proof_in_proc_tracking()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2R-500's cost, measured: a process of this user unrelated to the attempt (unmarked, as a later login's sshd
        // session or a later ssh-agent), started after the leader and non-dumpable, cannot be told apart from one of
        // the attempt's that hid its marker. In /proc tracking every scan is incomplete while it runs (with a cgroup it
        // counts only inside the user's manager: the fake /proc test above, as where this one runs depends on the host).
        let marker = format!("p/M2R500-cost-{}", std::process::id());
        let (mut leader, _) = python(
            "print('up', flush=True); import time; time.sleep(60)",
            Some(&marker),
        )?;
        let (mut later, said) = python(
            "import ctypes, time; assert ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0; print('hidden', flush=True); \
             time.sleep(60)",
            None,
        )?;
        let start = match stat(Path::new("/proc"), leader.id()) {
            Stat::Read(p) => Some(p.start.to_string()),
            _ => None,
        };
        let _ = leader.kill();
        let _ = leader.wait();
        let mut outside = Outside::new(&marker);
        outside.set_leader(Some(i64::from(leader.id())), start.as_deref());
        let proc_mode = [outside.scan(), outside.scan()];
        let _ = later.kill();
        let _ = later.wait();
        assert_eq!(said, "hidden");
        assert!(start.is_some(), "the leader's start was not read");
        assert_eq!(proc_mode, [Proof::Incomplete; 2]);
        Ok(())
    }

    #[test]
    fn with_a_cgroup_an_unreadable_process_holds_the_proof_only_inside_the_users_manager()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2R-500 with the attempt's delegated cgroup (CTO decision, 2026-11-21): an unreadable process of this user
        // started after the leader could be one of the attempt's only inside the user's manager (user@<uid>.service),
        // where an attempt's process can move itself; one in a login session's scope (a later SSH login's sshd
        // session) or another system-owned cgroup is not counted. One whose cgroup cannot be read is counted, and so is
        // every one when the host's own cgroup does not show its user's manager. In /proc tracking each is counted. Its environment here is a directory: a read that fails.
        let me = rustix::process::getuid().as_raw();
        let host =
            format!("0::/user.slice/user-{me}.slice/user@{me}.service/app.slice/hoh.scope\n");
        let unreadable = |cgroup: Option<&str>| -> std::io::Result<tempfile::TempDir> {
            let root = tree(&[])?;
            std::fs::create_dir_all(root.path().join("sys/kernel"))?;
            std::fs::write(root.path().join("sys/kernel/ns_last_pid"), b"300\n")?;
            std::fs::create_dir_all(root.path().join("self"))?;
            std::fs::write(root.path().join("self/cgroup"), &host)?;
            let dir = root.path().join("300");
            std::fs::create_dir_all(dir.join("environ"))?;
            std::fs::create_dir_all(dir.join("task/300/environ"))?;
            let stat = "300 (sshd) S 1 300 300 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 900 0";
            std::fs::write(dir.join("stat"), stat)?;
            std::fs::write(dir.join("task/300/stat"), stat)?;
            std::fs::write(
                dir.join("status"),
                format!("Name:\tsshd\nTgid:\t300\nUid:\t{me}\t{me}\t{me}\t{me}\n"),
            )?;
            match cgroup {
                Some(cgroup) => std::fs::write(dir.join("cgroup"), cgroup)?,
                // A cgroup file that cannot be read.
                None => std::fs::create_dir_all(dir.join("cgroup"))?,
            }
            Ok(root)
        };
        let proofs = |root: &tempfile::TempDir, by_cgroup: bool| {
            let mut outside = Outside::new("p/A1")
                .by_cgroup(by_cgroup)
                .with_root(Some(root.path().to_owned()));
            outside.set_leader(Some(100), Some("500"));
            [outside.scan(), outside.scan()]
        };
        let session = unreadable(Some(&format!(
            "0::/user.slice/user-{me}.slice/session-3.scope\n"
        )))?;
        let system = unreadable(Some("0::/system.slice/ssh.service\n"))?;
        let manager = unreadable(Some(&format!(
            "0::/user.slice/user-{me}.slice/user@{me}.service/app.slice/x.scope\n"
        )))?;
        let unknown = unreadable(None)?;
        let done = [Proof::Pending, Proof::Done];
        assert_eq!(proofs(&session, true), done);
        assert_eq!(proofs(&system, true), done);
        assert_eq!(proofs(&manager, true), [Proof::Incomplete; 2]);
        assert_eq!(proofs(&unknown, true), [Proof::Incomplete; 2]);
        assert_eq!(proofs(&session, false), [Proof::Incomplete; 2]);
        // A host whose own view does not show its user's manager (another cgroup namespace's): nothing can be told.
        std::fs::write(
            session.path().join("self/cgroup"),
            "0::/app.slice/hoh.scope\n",
        )?;
        assert_eq!(proofs(&session, true), [Proof::Incomplete; 2]);
        Ok(())
    }

    #[test]
    fn an_unreadable_process_leaves_the_outside_scan_incomplete_unless_older_than_the_leader_or_another_users()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2R-500, as WD-03: a process whose environment and threads' cannot be read is unrelated to the attempt only
        // when it started before the leader as recorded (even once the leader is gone and no longer pinned), or its
        // real user is another; then two scans prove the attempt's processes gone. Else every scan is incomplete. With
        // no start recorded it is not counted (the stated residual of such a record). Its environment here is a
        // directory: a read that fails.
        let me = rustix::process::getuid().as_raw();
        let unreadable = |start: u32, uid: u32| -> std::io::Result<tempfile::TempDir> {
            let root = tree(&[])?;
            std::fs::create_dir_all(root.path().join("sys/kernel"))?;
            std::fs::write(root.path().join("sys/kernel/ns_last_pid"), b"300\n")?;
            let dir = root.path().join("300");
            std::fs::create_dir_all(dir.join("environ"))?;
            std::fs::create_dir_all(dir.join("task/300/environ"))?;
            let stat = format!("300 (tool) S 1 300 300 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start} 0");
            std::fs::write(dir.join("stat"), &stat)?;
            std::fs::write(dir.join("task/300/stat"), &stat)?;
            std::fs::write(
                dir.join("status"),
                format!("Name:\ttool\nTgid:\t300\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
            )?;
            Ok(root)
        };
        let proofs = |root: &tempfile::TempDir, leader: Option<&str>| {
            let mut outside = Outside::new("p/A1")
                .born(leader)
                .with_root(Some(root.path().to_owned()));
            // The leader recorded (pid 100) is gone: no pin, its start time kept.
            outside.set_leader(Some(100), leader);
            [outside.scan(), outside.scan()]
        };
        let (older, other, same) = (
            unreadable(400, me)?,
            unreadable(900, me.wrapping_add(1))?,
            unreadable(900, me)?,
        );
        assert_eq!(proofs(&older, Some("500")), [Proof::Pending, Proof::Done]);
        assert_eq!(proofs(&other, Some("500")), [Proof::Pending, Proof::Done]);
        assert_eq!(proofs(&same, Some("500")), [Proof::Incomplete; 2]);
        assert_eq!(proofs(&same, None), [Proof::Pending, Proof::Done]);
        Ok(())
    }

    #[test]
    fn an_unreadable_process_is_not_the_attempts_when_older_than_its_leader_or_another_users()
    -> Result<(), Box<dyn std::error::Error>> {
        // WD-03: what shows an unreadable process unrelated: it started before the leader (every process of the
        // attempt descends from it), or its real user is another (the attempt's run as the host's user; a set-user-id
        // tool keeps it). Else it is unknown. Its environment here is a directory: a read that fails.
        let me = rustix::process::getuid().as_raw();
        let unreadable = |start: u32, uid: u32| -> std::io::Result<tempfile::TempDir> {
            let root = tree(&[(100, LEADER, MARKED)])?;
            let dir = root.path().join("300");
            std::fs::create_dir_all(dir.join("environ"))?;
            std::fs::create_dir_all(dir.join("task/300/environ"))?;
            let stat = format!("300 (tool) S 1 300 300 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start} 0");
            std::fs::write(dir.join("stat"), &stat)?;
            std::fs::write(dir.join("task/300/stat"), &stat)?;
            std::fs::write(
                dir.join("status"),
                format!("Name:\ttool\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
            )?;
            Ok(root)
        };
        let older = unreadable(400, me)?;
        let other = unreadable(900, me.wrapping_add(1))?;
        let same = unreadable(900, me)?;
        assert_eq!(
            [older.path(), other.path(), same.path()].map(|r| tools(r, (100, Some(500)), "p/A1")),
            [Tools::Absent, Tools::Absent, Tools::Unknown]
        );
        Ok(())
    }

    /// A process 300 of the host's user started after the leader whose environment cannot be read (a directory): its
    /// `stat` says `state` with `threads` threads and the flags `flags`; its threads' `(tid, stat state, flags)` are
    /// listed under task/, and its status counts `counted` threads.
    fn frozen(
        (state, flags, threads): (&str, u64, u32),
        listed: &[(u32, &str, u64)],
        counted: u32,
    ) -> std::io::Result<tempfile::TempDir> {
        let me = rustix::process::getuid().as_raw();
        let line = |tid: u32, state: &str, flags: u64, threads: u32| {
            format!(
                "{tid} (probe) {state} 1 300 300 0 -1 {flags} 0 0 0 0 0 0 0 0 20 0 {threads} 0 900 0"
            )
        };
        let root = tree(&[(100, LEADER, MARKED)])?;
        let dir = root.path().join("300");
        std::fs::create_dir_all(dir.join("environ"))?;
        std::fs::write(dir.join("stat"), line(300, state, flags, threads))?;
        for (tid, state, flags) in listed {
            std::fs::create_dir_all(dir.join(format!("task/{tid}/environ")))?;
            std::fs::write(
                dir.join(format!("task/{tid}/stat")),
                line(*tid, state, *flags, 1),
            )?;
        }
        std::fs::write(
            dir.join("status"),
            format!("Name:\tprobe\nUid:\t{me}\t{me}\t{me}\t{me}\nThreads:\t{counted}\n"),
        )?;
        Ok(root)
    }

    #[test]
    fn a_takeover_by_exec_or_an_exit_still_running_is_not_taken_for_an_end()
    -> Result<(), Box<dyn std::error::Error>> {
        // WDF-100: a non-leader exec (de_thread) hands the zombie leader's tid and start time to the execing thread.
        // Frozen at the moment Codex's schedule reached: the listing's stat said zombie with 2 threads (the leader and
        // the worker it started), the leader's task stat was read as Z, and by the count the worker had taken over
        // (Threads: 1, the same tid listed). Then a single-threaded process still exiting (PF_EXITING, not yet a
        // zombie). Neither has ended by one read; a zombie with one thread has.
        let exiting = 0x40_000C;
        let cases = [
            frozen(("Z", 0x40_800C, 2), &[(300, "Z", 0x40_800C)], 1)?,
            frozen(("R", exiting, 1), &[(300, "R", exiting)], 1)?,
            frozen(("Z", 0x40_800C, 1), &[(300, "Z", 0x40_800C)], 1)?,
        ];
        assert_eq!(
            cases
                .each_ref()
                .map(|r| tools(r.path(), (100, Some(500)), "p/A1")),
            [Tools::Unknown, Tools::Unknown, Tools::Absent]
        );
        Ok(())
    }

    /// The `stat` line of process 300 in [`frozen`]'s tree: `state`, `flags`, `threads` threads, started at `start`.
    fn stat_of(state: &str, flags: u64, threads: u32, start: u64) -> String {
        format!(
            "300 (probe) {state} 1 300 300 0 -1 {flags} 0 0 0 0 0 0 0 0 20 0 {threads} 0 {start} 0"
        )
    }

    /// [`tools_with`] on `root`, its pauses counted and the first one running `then` (what happened meanwhile).
    fn looked(root: &Path, then: impl FnOnce(&Path)) -> (Tools, usize) {
        let mut waits = 0;
        let mut then = Some(then);
        let found = tools_with(root, (100, Some(500)), "p/A1", &mut |_| {
            waits += 1;
            if let Some(then) = then.take() {
                then(&root.join("300"));
            }
        });
        (found, waits)
    }

    #[test]
    fn a_process_that_is_exiting_is_no_tool_once_one_read_shows_it_ended_within_the_settle_time()
    -> Result<(), Box<dyn std::error::Error>> {
        // Decision 19's corrections of 2026-11-17: on Linux a process that is exiting (PF_EXITING, 0x4 in its stat's
        // flags; past its memory, not yet a zombie) refuses /proc/<pid>/environ, and unrelated processes of the host's
        // user exit all the time. One that cannot be read and is exiting or a zombie is let end: it is no tool once one
        // stat read shows it gone, its pid another's, or a zombie with one thread; else, after the settle time, it is
        // unknown. The flags are what a real exiting process showed (0x40000C).
        let exiting = 0x40_000C;
        let zombie = 0x40_800C;
        let write = |dir: &Path, line: String| {
            let _ = std::fs::write(dir.join("stat"), line);
        };
        let cases = [
            // Exiting, then a zombie with one thread.
            looked(frozen(("R", exiting, 1), &[], 1)?.path(), |d| {
                write(d, stat_of("Z", zombie, 1, 900));
            }),
            // Exiting, then reaped.
            looked(frozen(("R", exiting, 1), &[], 1)?.path(), |d| {
                let _ = std::fs::remove_dir_all(d);
            }),
            // Exiting, then reaped and its pid given to another process.
            looked(frozen(("R", exiting, 1), &[], 1)?.path(), |d| {
                write(d, stat_of("S", 0, 1, 950));
            }),
            // A zombie main thread whose other thread then ends too.
            looked(frozen(("Z", zombie, 2), &[], 2)?.path(), |d| {
                write(d, stat_of("Z", zombie, 1, 900));
            }),
            // Its main thread exiting while another thread runs on: never ends within the settle time.
            looked(frozen(("R", exiting, 2), &[], 2)?.path(), |_| {}),
            // Not exiting at all (PR_SET_DUMPABLE=0 refuses the same way): unknown at once, no wait.
            looked(frozen(("S", 0x40_0000, 1), &[], 1)?.path(), |_| {}),
        ];
        let steps = (SETTLE_WITHIN.as_millis() / SETTLE_STEP.as_millis()) as usize;
        assert_eq!(
            cases,
            [
                (Tools::Absent, 1),
                (Tools::Absent, 1),
                (Tools::Absent, 1),
                (Tools::Absent, 1),
                (Tools::Unknown, steps),
                (Tools::Unknown, 0)
            ]
        );
        Ok(())
    }

    #[test]
    fn too_many_processes_to_let_end_or_a_look_already_unknown_waits_for_none()
    -> Result<(), Box<dyn std::error::Error>> {
        // WDF-250: the settle set's bound and its skip are pinned. One more exiting, unreadable process of the host's
        // user than SETTLE_AT_MOST is unknown at once, with no wait; and a look already unknown (here a live process
        // that cannot be read) does not wait for the exiting one beside it.
        let me = rustix::process::getuid().as_raw();
        let exiting = |root: &Path, pid: u32, state: &str, flags: u64| -> std::io::Result<()> {
            let dir = root.join(pid.to_string());
            std::fs::create_dir_all(dir.join("environ"))?;
            std::fs::write(
                dir.join("stat"),
                format!(
                    "{pid} (probe) {state} 1 {pid} {pid} 0 -1 {flags} 0 0 0 0 0 0 0 0 20 0 1 0 900 0"
                ),
            )?;
            std::fs::write(
                dir.join("status"),
                format!("Name:\tprobe\nUid:\t{me}\t{me}\t{me}\t{me}\n"),
            )
        };
        let many = tree(&[(100, LEADER, MARKED)])?;
        for i in 0..=SETTLE_AT_MOST {
            exiting(many.path(), 300 + u32::try_from(i)?, "R", 0x40_000C)?;
        }
        let beside = tree(&[(100, LEADER, MARKED)])?;
        exiting(beside.path(), 300, "R", 0x40_000C)?;
        exiting(beside.path(), 301, "S", 0x40_0000)?;
        let count = |root: &Path| {
            let mut waits = 0;
            let found = tools_with(root, (100, Some(500)), "p/A1", &mut |_| waits += 1);
            (found, waits)
        };
        assert_eq!(
            (count(many.path()), count(beside.path())),
            ((Tools::Unknown, 0), (Tools::Unknown, 0))
        );
        Ok(())
    }

    #[test]
    fn a_zombie_whose_worker_takes_it_over_by_exec_is_never_taken_for_ended()
    -> Result<(), Box<dyn std::error::Error>> {
        // WDF-100, Codex's two phases through the settle seam. Phase one, the listing: the main thread has exited (a
        // zombie) after starting a worker, so the stat shows 2 threads. Phase two, while it is let end: the worker
        // execs, taking over the pid and its start time (de_thread), and runs alone (one thread, not a zombie). No
        // read ever shows an end, so it stays unknown; had the worker ended instead, it is no tool.
        let zombie = 0x40_800C;
        let takeover = looked(frozen(("Z", zombie, 2), &[], 2)?.path(), |d| {
            let _ = std::fs::write(d.join("stat"), stat_of("S", 0x40_0000, 1, 900));
        });
        let ends = looked(frozen(("Z", zombie, 2), &[], 2)?.path(), |d| {
            let _ = std::fs::write(d.join("stat"), stat_of("Z", zombie, 1, 900));
        });
        let steps = (SETTLE_WITHIN.as_millis() / SETTLE_STEP.as_millis()) as usize;
        assert_eq!(
            (takeover, ends),
            ((Tools::Unknown, steps), (Tools::Absent, 1))
        );
        Ok(())
    }

    #[test]
    fn a_scan_that_could_not_read_a_process_proves_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-101 (decision 8d): a /proc tree with a process whose stat cannot be understood; however often it is
        // scanned, nothing is concluded (no Done), and once that process is gone two scans prove it.
        let root = tempfile::tempdir()?;
        let write = |pid: &str, stat: &[u8]| -> std::io::Result<()> {
            std::fs::create_dir_all(root.path().join(pid))?;
            std::fs::write(root.path().join(pid).join("stat"), stat)?;
            std::fs::write(root.path().join(pid).join("environ"), b"PATH=/bin\0")
        };
        std::fs::create_dir_all(root.path().join("sys/kernel"))?;
        std::fs::write(root.path().join("sys/kernel/ns_last_pid"), b"101\n")?;
        write(
            "100",
            b"100 (sh) S 1 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 500 0",
        )?;
        write("101", b"garbage")?;
        let mut outside = Outside::new("p/A1");
        outside.root = root.path().to_owned();
        for _ in 0..3 {
            assert_eq!(outside.scan(), Proof::Incomplete);
        }
        std::fs::remove_dir_all(root.path().join("101"))?;
        assert_eq!(outside.scan(), Proof::Pending);
        assert_eq!(outside.scan(), Proof::Done);
        Ok(())
    }

    #[test]
    fn a_process_whose_name_is_not_utf8_is_read_and_found_by_its_marker()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-101: a script named with a byte 0xff (its process is named after it); before, its stat could not be
        // read as text and the scan skipped it.
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::process::CommandExt as _;
        // A link to /bin/sh named w\xffr (the process is named after the path it was started by); no file is written and
        // then executed, so a parallel test's fork cannot make it busy (M2T06R-354).
        let dir = tempfile::tempdir()?;
        let path = dir.path().join(std::ffi::OsStr::from_bytes(b"w\xffr"));
        std::os::unix::fs::symlink("/bin/sh", &path)?;
        let marker = format!("test-{}/A1", std::process::id());
        let mut child = std::process::Command::new(&path)
            .args(["-c", "while :; do sleep 1; done"])
            .env(ATTEMPT_ENV, &marker)
            .process_group(0)
            .spawn()?;
        let pid = child.id();
        let end = Instant::now() + Duration::from_secs(5);
        while !std::fs::read(format!("/proc/{pid}/stat"))
            .is_ok_and(|s| s.windows(3).any(|w| w == b"w\xffr"))
        {
            if Instant::now() > end {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let named = std::fs::read(format!("/proc/{pid}/stat"))?;
        let mut outside = Outside::new(&marker);
        let proof = outside.scan();
        let pinned: Vec<u32> = outside.pins.iter().map(|p| p.pid).collect();
        if let Some(group) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = child.wait();
        assert!(
            std::str::from_utf8(&named).is_err(),
            "the precondition: a name that is not UTF-8"
        );
        assert!(pinned.contains(&pid), "{pinned:?} lacks {pid}");
        assert_eq!(proof, Proof::Pending);
        Ok(())
    }
}
