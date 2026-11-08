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

/// The product's processes for a cancel: an attempt's group found by its recorded identity (ADR-019 decision 4),
/// its native interrupt through the executor that runs it, and the project's workspace.
struct HostProcesses {
    works: BTreeMap<String, Work>,
    executor: Option<Arc<dyn Executor>>,
    project: PathBuf,
}

impl Processes for HostProcesses {
    fn control(&self, attempt: &AttemptRow) -> Option<(Box<dyn ProcessControl + Send>, bool)> {
        let presence = identity::presence(
            attempt.pid,
            attempt.boot_id.as_deref(),
            attempt.process_start_id.as_deref(),
        );
        Some(match presence {
            Ok(Presence::Ours(found)) => {
                let native = match (&self.executor, self.works.get(&attempt.attempt_id)) {
                    (Some(executor), Some(work)) => Some((executor.clone(), work.clone())),
                    _ => None,
                };
                let has_native = native.is_some();
                let escaped = attempt
                    .pid
                    .and_then(|pid| u32::try_from(pid).ok())
                    .map(escaped)
                    .unwrap_or_default();
                (
                    Box::new(FoundControl {
                        found,
                        native,
                        escaped,
                    }) as Box<dyn ProcessControl + Send>,
                    has_native,
                )
            }
            // Nothing of it runs: no process was recorded (held at its start gate, which closed when its task
            // stopped) or its group is empty.
            Ok(Presence::Gone(_)) => (Box::new(Fixed(Look::Empty)), false),
            // It cannot be told apart: no signal may go (ADR-019 decision 8); it stays cancelling.
            _ => (Box::new(Fixed(Look::Lost)), false),
        })
    }

    fn workspace(&self) -> Value {
        workspace(&self.project)
    }
}

/// A group found by its identity, with the descendants of its leader that left it.
struct FoundControl {
    found: identity::Found,
    native: Option<(Arc<dyn Executor>, Work)>,
    escaped: Vec<Escaped>,
}

impl ProcessControl for FoundControl {
    fn look(&mut self) -> Look {
        let group = match self.found.look() {
            identity::Group::Held => Look::Held,
            identity::Group::Empty => Look::Empty,
            identity::Group::Lost => Look::Lost,
        };
        // Its group empty while an escaped descendant lives: not yet (ADR-021 decision 8d).
        match group {
            Look::Empty if self.escaped.iter().any(Escaped::alive) => Look::Held,
            other => other,
        }
    }

    fn send(&mut self, signal: Signal) -> bool {
        let to_escaped = |sig: rustix::process::Signal, escaped: &[Escaped]| {
            escaped
                .iter()
                .filter(|e| e.alive())
                .map(|e| rustix::process::pidfd_send_signal(&e.pidfd, sig).is_ok())
                .fold(false, |a, b| a | b)
        };
        match signal {
            Signal::Native => match &self.native {
                Some((executor, work)) => {
                    executor.cancel(work);
                    true
                }
                None => false,
            },
            Signal::Term => {
                let group = self.found.terminate();
                to_escaped(rustix::process::Signal::TERM, &self.escaped) || group
            }
            Signal::Kill => {
                let group = self.found.kill();
                to_escaped(rustix::process::Signal::KILL, &self.escaped) || group
            }
        }
    }
}

/// A descendant of an attempt's leader that left its process group (setsid, setpgid): a group signal misses it, so it
/// is pinned by a pidfd, its start time checked after the open (a reused pid is never taken), and signalled through
/// it (REQ-043 decision 3, docs/adr/ADR-021.md decision 8d). Only descendants still under the leader are found: one
/// orphaned before the scan cannot be told apart (the residual of decision 8d).
struct Escaped {
    pidfd: rustix::fd::OwnedFd,
}

impl Escaped {
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

/// A process's parent, process group and start time (`/proc/<pid>/stat` fields 4, 5 and 22).
fn stat(pid: u32) -> Option<(u32, u32, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = text.rsplit_once(") ")?.1.split(' ').collect();
    Some((
        fields.get(1)?.parse().ok()?,
        fields.get(2)?.parse().ok()?,
        fields.get(19)?.parse().ok()?,
    ))
}

/// The descendants of `leader` (its process group's number) that are in another process group, pinned.
fn escaped(leader: u32) -> Vec<Escaped> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let all: Vec<(u32, (u32, u32, u64))> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| Some((pid, stat(pid)?)))
        .collect();
    let mut found = Vec::new();
    let mut parents = vec![leader];
    while let Some(parent) = parents.pop() {
        for (pid, (ppid, pgrp, start)) in &all {
            if *ppid != parent || *pid == leader {
                continue;
            }
            parents.push(*pid);
            if *pgrp == leader {
                continue;
            }
            let Some(raw) = i32::try_from(*pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            else {
                continue;
            };
            let Ok(pidfd) = rustix::process::pidfd_open(raw, rustix::process::PidfdFlags::empty())
            else {
                continue;
            };
            // The pidfd names the process the scan saw only if it is still that one.
            if stat(*pid).is_some_and(|(_, _, s)| s == *start) {
                found.push(Escaped { pidfd });
            }
        }
    }
    found
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
