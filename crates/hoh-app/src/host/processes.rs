//! An attempt's processes outside its process group and the proof that none is left (docs/adr/ADR-021.md decision 8d,
//! version 3): every path that ends an attempt or releases its writer lease (the cancel, the executor's report of its
//! leader's end, the settle of an expired lease or a lost executor, a host's start reconcile) first proves its group
//! empty and none of these alive.

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

/// A process as `/proc/<pid>/stat` shows it: its parent and its start time (fields 4 and 22).
pub(super) struct Proc {
    pub(super) pid: u32,
    pub(super) ppid: u32,
    pub(super) start: u64,
}

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

/// Whether `pid`'s environment has `marker` (`NAME=value`) as one of its entries: `None` when that cannot be told (a
/// read that failed other than for another user's process or a process gone).
fn marked(root: &Path, pid: u32, marker: &[u8]) -> Option<bool> {
    match std::fs::read(root.join(pid.to_string()).join("environ")) {
        Ok(env) => Some(env.split(|b| *b == 0).any(|entry| entry == marker)),
        // Its main thread may have exited while other threads still run: theirs are read (M2T06R-301).
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Some(marked_threads(root, pid, marker))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
            Some(false)
        }
        Err(_) => None,
    }
}

/// Whether a thread of `pid` that can be read has `marker` in its environment.
fn marked_threads(root: &Path, pid: u32, marker: &[u8]) -> bool {
    let Ok(tasks) = std::fs::read_dir(root.join(pid.to_string()).join("task")) else {
        return false;
    };
    tasks.filter_map(Result::ok).any(|task| {
        std::fs::read(task.path().join("environ"))
            .is_ok_and(|env| env.split(|b| *b == 0).any(|entry| entry == marker))
    })
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
        let mut batch: Vec<u32> = processes.iter().map(|p| p.pid).collect();
        let mut settled = false;
        for _ in 0..=SWEEPS {
            for pid in batch.drain(..) {
                match marked(&self.root, pid, &self.marker) {
                    Some(true) => {
                        by_marker.insert(pid);
                    }
                    Some(false) => {}
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
                        batch.push(pid);
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

    #[test]
    fn the_leader_roots_the_parent_chain_only_as_the_process_recorded()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-151: a process with a child, unmarked. Named as the leader with another start time (a pid reused), it
        // roots nothing; named with its own, its child is found and pinned.
        use std::os::unix::process::CommandExt as _;
        let mut parent = std::process::Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .process_group(0)
            .spawn()?;
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
        let mut reused = Outside::new("p/A1");
        reused.set_leader(Some(i64::from(pid)), Some(&(start + 1).to_string()));
        let _ = reused.scan();
        let mut recorded = Outside::new("p/A1");
        recorded.set_leader(Some(i64::from(pid)), Some(&start.to_string()));
        let _ = recorded.scan();
        let pinned = (reused.pins.len(), recorded.pins.len());
        if let Some(group) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = parent.wait();
        assert_eq!(pinned, (0, 1));
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
    fn a_scan_that_could_not_read_a_process_proves_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-101 (decision 8d v3): a /proc tree with a process whose stat cannot be understood; however often it is
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
