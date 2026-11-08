//! Pause and cancel on the host (REQ-043, docs/adr/ADR-021.md decisions 7 and 8): a loop that runs whether or not
//! the host has an executor. A pausing run is moved to PAUSED at its safe point; a cancelling run gets a task of its
//! own that stops what this host runs for it, escalates over its attempts' process groups pass after pass, and once
//! nothing of it runs records the workspace and cancels it. Neither touches the Commander (REQ-043 behaviour 7).

use super::http::Shared;
use super::runner::{Entry, Executor, Work};
use hoh_agents::identity::{self, Presence};
use hoh_core::cancel::{Canceller, GRACES, Look, ProcessControl, Processes, Signal};
use hoh_core::delivery::Clock;
use hoh_core::persistence::{AttemptRow, Finished, PersistError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// The attempts this host runs (the runner's, shared so a cancel can stop them first).
pub(super) type Inflight = Arc<Mutex<BTreeMap<String, Entry>>>;

/// How often the loop looks again when nothing woke it.
const TICK: Duration = Duration::from_millis(200);

/// How often a cancel pass looks at a group at least (a group seen held must be looked at again within
/// `identity::HELD_FOR` after its leader's reap to stay signalable).
const LOOK_EVERY: Duration = Duration::from_millis(10);

/// The pause between two cancel steps of a run whose group still lives or was lost track of.
const STEP_AGAIN: Duration = Duration::from_secs(1);

/// What the loop and its cancel tasks share.
struct Common {
    shared: Arc<Shared>,
    clock: Arc<dyn Clock>,
    executor: Option<Arc<dyn Executor>>,
    inflight: Inflight,
    project: PathBuf,
    canceller: Canceller,
    cancelling: Mutex<BTreeSet<String>>,
}

/// The loop of `shared`'s project until `stop`.
pub(super) async fn run(
    shared: Arc<Shared>,
    clock: Arc<dyn Clock>,
    executor: Option<Arc<dyn Executor>>,
    inflight: Inflight,
    project: PathBuf,
    mut stop: watch::Receiver<bool>,
) {
    let mut watermark = shared.store.watermark();
    let common = Arc::new(Common {
        shared,
        clock,
        executor,
        inflight,
        project,
        canceller: Canceller::new(GRACES, LOOK_EVERY),
        cancelling: Mutex::new(BTreeSet::new()),
    });
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    loop {
        if *stop.borrow() {
            break;
        }
        if let Err(e) = tick(&common, &mut tasks).await {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "hoh host: pause and cancel: {e}");
        }
        tasks.retain(|t| !t.is_finished());
        tokio::select! {
            _ = watermark.changed() => {}
            () = tokio::time::sleep(TICK) => {}
            _ = stop.changed() => break,
        }
    }
    // The host stops: a cancel in progress is left to the next host (its reconcile stops the processes and its loop
    // finishes the cancel).
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
}

async fn tick(
    common: &Arc<Common>,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
) -> Result<(), PersistError> {
    let store = &common.shared.store;
    let waiting = store.runner_waiting(&common.shared.project_id).await?;
    for run in &waiting.pausing {
        store.pause_step(run, common.clock.now()).await?;
    }
    for run in waiting.cancelling {
        let new = common
            .cancelling
            .lock()
            .map(|mut c| c.insert(run.clone()))
            .unwrap_or(false);
        if new {
            let common = common.clone();
            tasks.push(tokio::spawn(async move {
                cancel(&common, &run).await;
                if let Ok(mut c) = common.cancelling.lock() {
                    c.remove(&run);
                }
            }));
        }
    }
    Ok(())
}

/// Cancel `run_id` until it is CANCELLED (or no longer cancelling).
async fn cancel(common: &Arc<Common>, run_id: &str) {
    let mut works: BTreeMap<String, Work> = BTreeMap::new();
    let pins: Pins = Arc::default();
    loop {
        // What this host runs for the run stops first, so the cancel alone ends its attempts (one writer, ADR-021
        // decision 8c); a native interrupt goes to the work it was given.
        let entries: Vec<(String, Entry)> = common
            .inflight
            .lock()
            .map(|mut inflight| {
                let ids: Vec<String> = inflight
                    .iter()
                    .filter(|(_, e)| e.work.run_id == run_id)
                    .map(|(id, _)| id.clone())
                    .collect();
                ids.into_iter()
                    .filter_map(|id| inflight.remove(&id).map(|e| (id, e)))
                    .collect()
            })
            .unwrap_or_default();
        for (id, entry) in entries {
            if let Some(task) = entry.task {
                task.abort();
                let _ = task.await;
            }
            works.insert(id, entry.work);
        }
        let step = async {
            let processes: Arc<dyn Processes> = Arc::new(HostProcesses {
                works: works.clone(),
                executor: common.executor.clone(),
                project: common.project.clone(),
                project_id: common.shared.project_id.clone(),
                pins: pins.clone(),
            });
            common
                .canceller
                .step(
                    &common.shared.store,
                    run_id,
                    processes,
                    common.clock.clone(),
                    Arc::new(std::thread::sleep),
                )
                .await
        };
        match step.await {
            Ok(Finished::Running(_)) => tokio::time::sleep(STEP_AGAIN).await,
            Ok(Finished::Cancelled | Finished::NotCancelling) => return,
            Err(e) => {
                use std::io::Write as _;
                let _ = writeln!(std::io::stderr(), "hoh host: the cancel of {run_id}: {e}");
                tokio::time::sleep(STEP_AGAIN).await;
            }
        }
    }
}

/// The product's processes for a cancel: an attempt's group found by its recorded identity (ADR-019 decision 4) and
/// the processes of the attempt found outside it, its native interrupt through the executor that runs it, and the
/// project's workspace.
struct HostProcesses {
    works: BTreeMap<String, Work>,
    executor: Option<Arc<dyn Executor>>,
    project: PathBuf,
    project_id: String,
    pins: Pins,
}

impl Processes for HostProcesses {
    fn control(&self, attempt: &AttemptRow) -> Option<(Box<dyn ProcessControl + Send>, bool)> {
        let presence = identity::presence(
            attempt.pid,
            attempt.boot_id.as_deref(),
            attempt.process_start_id.as_deref(),
        );
        let native = match (&self.executor, self.works.get(&attempt.attempt_id)) {
            (Some(executor), Some(work)) => Some((executor.clone(), work.clone())),
            _ => None,
        };
        let has_native = native.is_some();
        let control = |group: Option<identity::Found>, leader: Option<u32>| AttemptControl {
            group,
            leader,
            native: native.clone(),
            attempt_id: attempt.attempt_id.clone(),
            marker: format!(
                "{ATTEMPT_ENV}={}",
                super::runner::marker(&self.project_id, &attempt.attempt_id)
            )
            .into_bytes(),
            pins: self.pins.clone(),
            scanned: None,
            sent: None,
            quiet: 0,
        };
        Some(match presence {
            Ok(Presence::Ours(found)) => {
                let leader = attempt.pid.and_then(|pid| u32::try_from(pid).ok());
                (Box::new(control(Some(found), leader)), has_native)
            }
            // Its group is gone (or no process was recorded: held at its start gate, which closed when its task
            // stopped); what of it left the group is still looked for.
            Ok(Presence::Gone(_)) => (Box::new(control(None, None)), false),
            // It cannot be told apart: no signal may go (ADR-019 decision 8); it stays cancelling.
            _ => (Box::new(Fixed(Look::Lost)), false),
        })
    }

    fn workspace(&self) -> Value {
        workspace(&self.project)
    }
}

/// The environment variable an executor starts an attempt's leader with, its value [`Work::marker`] (the project's id
/// and the attempt's). Every descendant inherits it, so a cancel finds the attempt's processes wherever they went: another group, another session, another
/// parent (docs/adr/ADR-021.md decision 8d).
pub const ATTEMPT_ENV: &str = "HOH_ATTEMPT_ID";

/// How often a pass looks for the attempt's processes outside its group while the group lives.
const RESCAN: Duration = Duration::from_millis(100);

/// The processes of each attempt of a cancel found outside its group, pinned: kept for the whole cancel, across its
/// passes.
type Pins = Arc<Mutex<BTreeMap<String, Vec<Pinned>>>>;

/// An attempt's group found by its identity (or gone), and its processes found outside it (decision 8d): a group
/// signal misses a descendant that left the group (setsid, setpgid), so each is pinned by a pidfd, its start time
/// checked after the open (a reused pid is never taken), and signalled through it. They are found by the /proc parent
/// chain from the leader and from every pinned one (REQ-043 decision 3), and by the attempt's [`ATTEMPT_ENV`] in their
/// environment, whatever their parent now is (an orphan too). The group counts as empty only once no pinned one lives
/// and two scans in a row found no other.
struct AttemptControl {
    group: Option<identity::Found>,
    leader: Option<u32>,
    native: Option<(Arc<dyn Executor>, Work)>,
    attempt_id: String,
    /// `ATTEMPT_ENV=<marker>` as it appears in an environment.
    marker: Vec<u8>,
    pins: Pins,
    /// When this pass last looked for them.
    scanned: Option<Instant>,
    /// The strongest signal this pass sent: a process found later gets it at once.
    sent: Option<rustix::process::Signal>,
    /// Scans in a row that found nothing alive with the group empty.
    quiet: u8,
}

impl AttemptControl {
    /// Look for the attempt's processes outside its group, pin the new ones and send them this pass's strongest
    /// signal so far; whether any pinned one lives.
    fn scan(&mut self) -> bool {
        self.scanned = Some(Instant::now());
        let Ok(mut pins) = self.pins.lock() else {
            return true;
        };
        let pinned = pins.entry(self.attempt_id.clone()).or_default();
        let processes = processes();
        let mut found: BTreeSet<u32> = BTreeSet::new();
        let mut parents: Vec<u32> = self
            .leader
            .into_iter()
            .chain(pinned.iter().filter(|p| p.alive()).map(|p| p.pid))
            .collect();
        while let Some(parent) = parents.pop() {
            for process in processes.iter().filter(|p| p.ppid == parent) {
                if found.insert(process.pid) {
                    parents.push(process.pid);
                }
            }
        }
        let marked: Vec<u32> = processes
            .iter()
            .filter(|p| !found.contains(&p.pid) && marked(p.pid, &self.marker))
            .map(|p| p.pid)
            .collect();
        found.extend(marked);
        for process in processes.iter().filter(|p| found.contains(&p.pid)) {
            if Some(process.pid) == self.leader
                || pinned
                    .iter()
                    .any(|q| q.pid == process.pid && q.start == process.start)
            {
                continue;
            }
            if let Some(pin) = Pinned::open(process.pid, process.start) {
                if let Some(signal) = self.sent {
                    let _ = rustix::process::pidfd_send_signal(&pin.pidfd, signal);
                }
                pinned.push(pin);
            }
        }
        pinned.retain(Pinned::alive);
        !pinned.is_empty()
    }

    fn to_pinned(&self, signal: rustix::process::Signal) -> bool {
        self.pins.lock().is_ok_and(|pins| {
            pins.get(&self.attempt_id)
                .into_iter()
                .flatten()
                .filter(|p| p.alive())
                .map(|p| rustix::process::pidfd_send_signal(&p.pidfd, signal).is_ok())
                .fold(false, |a, b| a | b)
        })
    }
}

impl ProcessControl for AttemptControl {
    fn look(&mut self) -> Look {
        let group = match self.group.as_ref().map(identity::Found::look) {
            Some(identity::Group::Held) => Look::Held,
            Some(identity::Group::Lost) => return Look::Lost,
            Some(identity::Group::Empty) | None => Look::Empty,
        };
        let due = group == Look::Empty || self.scanned.is_none_or(|at| at.elapsed() >= RESCAN);
        let living = if due {
            self.scan()
        } else {
            self.pins.lock().map_or(true, |pins| {
                pins.get(&self.attempt_id)
                    .is_some_and(|p| p.iter().any(Pinned::alive))
            })
        };
        if group == Look::Held || living {
            self.quiet = 0;
            return Look::Held;
        }
        // Two scans in a row (a look apart) found nothing: one just forked may not show its environment yet.
        self.quiet += 1;
        if self.quiet >= 2 {
            Look::Empty
        } else {
            Look::Held
        }
    }

    fn send(&mut self, signal: Signal) -> bool {
        let (sig, group) = match signal {
            Signal::Native => {
                return match &self.native {
                    Some((executor, work)) => {
                        executor.cancel(work);
                        true
                    }
                    None => false,
                };
            }
            Signal::Term => (
                rustix::process::Signal::TERM,
                self.group.as_ref().is_some_and(identity::Found::terminate),
            ),
            Signal::Kill => (
                rustix::process::Signal::KILL,
                self.group.as_ref().is_some_and(identity::Found::kill),
            ),
        };
        if self.sent != Some(rustix::process::Signal::KILL) {
            self.sent = Some(sig);
        }
        self.to_pinned(sig) || group
    }
}

/// A process of an attempt outside its group, pinned by a pidfd.
struct Pinned {
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
        stat(pid)
            .is_some_and(|s| s.start == start)
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
}

/// A process as `/proc/<pid>/stat` shows it: its parent and its start time (fields 4 and 22).
struct Proc {
    pid: u32,
    ppid: u32,
    start: u64,
}

fn stat(pid: u32) -> Option<Proc> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = text.rsplit_once(") ")?.1.split(' ').collect();
    Some(Proc {
        pid,
        ppid: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

/// Every process /proc shows.
fn processes() -> Vec<Proc> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(stat)
        .collect()
}

/// Whether `pid`'s environment has `marker` (`NAME=value`) as one of its entries; a process of another user cannot be
/// read and has not.
fn marked(pid: u32, marker: &[u8]) -> bool {
    std::fs::read(format!("/proc/{pid}/environ"))
        .is_ok_and(|env| env.split(|b| *b == 0).any(|entry| entry == marker))
}

/// A group whose look is known without looking: gone, or lost track of.
struct Fixed(Look);

impl ProcessControl for Fixed {
    fn look(&mut self) -> Look {
        self.0
    }

    fn send(&mut self, _signal: Signal) -> bool {
        false
    }
}

/// How long one git command of the workspace record may take.
const GIT_LIMIT: Duration = Duration::from_secs(5);

/// git's empty tree, the base of a diff in a repository with no commit yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Run git read-only in `project` (no optional lock, no fsmonitor, so it writes no index, object or ref) for at most
/// [`GIT_LIMIT`]; its output.
fn git(project: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let mut child = std::process::Command::new("git")
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(project)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    let mut stdout = child.stdout.take().ok_or("git: no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    });
    let end = Instant::now() + GIT_LIMIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git {}: over {GIT_LIMIT:?}", args.join(" ")));
            }
            Err(e) => return Err(format!("git {}: {e}", args.join(" "))),
        }
    };
    let out = reader
        .join()
        .map_err(|_| "git: its output was lost".to_owned())?
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !status.success() {
        return Err(format!("git {}: {status}", args.join(" ")));
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One untracked path's part of the workspace record, read without following a link and without waiting on anything
/// that is not a regular file (M2T06R-06): a regular file is its content's hash, a symbolic link its target's, anything
/// else (a FIFO, a socket, a device, a directory) its type, never opened. The file is opened without following a link
/// and without blocking, and read only if it is still a regular file then.
fn untracked_entry(path: &Path) -> String {
    use rustix::fs::{FileType, Mode, OFlags};
    use std::io::Read as _;
    let kind = |t: FileType| match t {
        FileType::Fifo => "fifo",
        FileType::Socket => "socket",
        FileType::CharacterDevice => "char_device",
        FileType::BlockDevice => "block_device",
        FileType::Directory => "directory",
        FileType::Symlink => "symlink",
        _ => "other",
    };
    let file_type = match rustix::fs::lstat(path) {
        Ok(stat) => FileType::from_raw_mode(stat.st_mode),
        Err(e) => return format!("unreadable: {e}"),
    };
    match file_type {
        FileType::Symlink => {
            return match std::fs::read_link(path) {
                Ok(target) => format!("symlink:{}", hex(target.as_os_str().as_bytes())),
                Err(e) => format!("unreadable: {e}"),
            };
        }
        FileType::RegularFile => {}
        other => return format!("type:{}", kind(other)),
    }
    let flags =
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    let fd = match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(e) => return format!("unreadable: {e}"),
    };
    match rustix::fs::fstat(&fd) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {}
        Ok(stat) => return format!("type:{}", kind(FileType::from_raw_mode(stat.st_mode))),
        Err(e) => return format!("unreadable: {e}"),
    }
    let mut file = std::fs::File::from(fd);
    let mut content = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => content.update(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return format!("unreadable: {e}"),
        }
    }
    content
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The workspace's record before CANCELLED (REQ-043 behaviour 4, ADR-021 decision 8g): HEAD and its tree, and the
/// hashes of the diff against HEAD, of the untracked files' names and contents and of the status, and of all of
/// them; read-only, nothing reset, cleaned or removed. A directory that is not a git work tree, or a git that fails,
/// is recorded as such.
pub(super) fn workspace(project: &Path) -> Value {
    let text = |out: Vec<u8>| String::from_utf8_lossy(&out).trim().to_owned();
    let head = git(project, &["rev-parse", "--verify", "HEAD"])
        .map(text)
        .ok();
    let head_tree = git(project, &["rev-parse", "--verify", "HEAD^{tree}"])
        .map(text)
        .ok();
    let base = head.clone().unwrap_or_else(|| EMPTY_TREE.to_owned());
    let record = (|| -> Result<Value, String> {
        let diff = git(
            project,
            &["diff", "--binary", "--no-ext-diff", "--no-textconv", &base],
        )?;
        let names = git(
            project,
            &["ls-files", "-z", "--others", "--exclude-standard"],
        )?;
        let mut untracked = Sha256::new();
        for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
            untracked.update(name);
            untracked.update([0]);
            let path = project.join(std::ffi::OsStr::from_bytes(name));
            untracked.update(untracked_entry(&path).as_bytes());
            untracked.update([0]);
        }
        let untracked: String = untracked
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let status = git(project, &["status", "--porcelain", "-z"])?;
        let (diff, status) = (hex(&diff), hex(&status));
        let all = hex(format!(
            "{}\n{}\n{diff}\n{untracked}\n{status}",
            head.as_deref().unwrap_or(""),
            head_tree.as_deref().unwrap_or("")
        )
        .as_bytes());
        Ok(json!({
            "head": head,
            "head_tree": head_tree,
            "diff_sha256": diff,
            "untracked_sha256": untracked,
            "status_sha256": status,
            "workspace_sha256": all,
        }))
    })();
    let mut record =
        record.unwrap_or_else(|e| json!({"head": head, "head_tree": head_tree, "error": e}));
    if let Some(fields) = record.as_object_mut() {
        fields.insert(
            "host_log".to_owned(),
            json!(project.join(".hoh/host.log").to_string_lossy()),
        );
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn git_in(dir: &Path, args: &[&str]) -> R {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("git {args:?}: {status}").into())
        }
    }

    #[test]
    fn the_workspace_record_never_waits_on_a_fifo_and_records_a_link_by_its_target() -> R {
        // M2T06R-06: an untracked FIFO with no writer, and an untracked link to it; before, the record opened the
        // link's target and waited there for a writer.
        let dir = tempfile::tempdir()?;
        let project = dir.path();
        git_in(project, &["init", "-q"])?;
        std::fs::write(project.join("plain.txt"), b"content")?;
        rustix::fs::mknodat(
            rustix::fs::CWD,
            project.join("pipe"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )?;
        std::os::unix::fs::symlink(project.join("pipe"), project.join("link"))?;
        assert_eq!(untracked_entry(&project.join("pipe")), "type:fifo");
        let to_pipe = untracked_entry(&project.join("link"));
        assert_eq!(
            to_pipe,
            format!(
                "symlink:{}",
                hex(project.join("pipe").as_os_str().as_bytes())
            )
        );
        assert_eq!(untracked_entry(&project.join("plain.txt")), hex(b"content"));
        let (sent, got) = std::sync::mpsc::channel();
        let at = project.to_owned();
        std::thread::spawn(move || {
            let _ = sent.send(workspace(&at));
        });
        let record = got.recv_timeout(Duration::from_secs(20))?;
        assert!(record.get("error").is_none(), "{record}");
        assert!(record["untracked_sha256"].is_string(), "{record}");
        // A link to another target is another record.
        std::fs::remove_file(project.join("link"))?;
        std::os::unix::fs::symlink(project.join("plain.txt"), project.join("link"))?;
        assert_ne!(untracked_entry(&project.join("link")), to_pipe);
        assert_ne!(
            workspace(project)["untracked_sha256"],
            record["untracked_sha256"]
        );
        Ok(())
    }
}
