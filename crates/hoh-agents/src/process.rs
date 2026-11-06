//! Process substrate for native agent CLIs (docs/13-adapters.md "进程与终结", docs/14-resilience.md).
//!
//! - Every agent runs argv-only (no shell) as the leader of its own process group.
//! - stdout is read as [`Frame`]s with the frame limit while stderr is drained in parallel into a bounded tail, so
//!   neither pipe can stall the child or grow without bound. Frames wait in a bounded queue: a consumer that does not
//!   read stops the reader and, through the full pipe, the child (backpressure). After a refused frame stdout is
//!   drained and discarded, so the child is not blocked while the adapter cancels it.
//! - Cancellation and the wall deadline escalate: optional native signal to the leader, then SIGTERM to the group,
//!   then SIGKILL to the group, each after its grace, and the leader is reaped. Native requests sent over stdin are
//!   the adapter's; it sends them before calling [`Process::cancel`] with no native signal.
//! - When the leader exits, whatever it left in its group is killed before the leader is reaped (its process group
//!   id cannot be reused while the leader is an unreaped zombie), and the report says whether the group was gone
//!   within the reap grace. Descendants that left the group (`setsid`) are outside it; pipes they keep open are
//!   abandoned after the drain grace so the exit report is never held up by them.
//! - Dropping the [`Process`] escalates like a deadline; dropping the runtime kills the group with SIGKILL.
//!
//! The wall deadline here is the backstop: an inactivity watchdog is separate and does not kill (docs/13).

use crate::frame::{FRAME_LIMIT, Frame, FrameReader, READ_CHUNK};
use rustix::process::{
    Pid, PidfdFlags, Signal as RawSignal, WaitId, WaitIdOptions, kill_process_group, pidfd_open,
    pidfd_send_signal, test_kill_process_group, waitid,
};
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::net::unix::pipe;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};

/// Signals the substrate sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Terminate,
    Kill,
}

impl Signal {
    fn raw(self) -> RawSignal {
        match self {
            Signal::Interrupt => RawSignal::INT,
            Signal::Terminate => RawSignal::TERM,
            Signal::Kill => RawSignal::KILL,
        }
    }
}

/// Who a signal went to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Leader,
    Group,
}

/// One escalation step, in the order sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub at: Instant,
    pub target: Target,
    pub signal: Signal,
}

/// How to end a process: `native` (if any) goes to the leader, SIGTERM to the group after `native_grace`, SIGKILL to
/// the group after a further `term_grace`; after another `kill_grace` without the leader exiting the process is
/// reported as not reaped. A leader that left its group gets SIGTERM and SIGKILL as well (M1T07R-103).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Escalation {
    pub native: Option<Signal>,
    pub native_grace: Duration,
    pub term_grace: Duration,
    pub kill_grace: Duration,
}

impl Default for Escalation {
    fn default() -> Self {
        Self {
            native: None,
            native_grace: Duration::ZERO,
            term_grace: Duration::from_secs(5),
            kill_grace: Duration::from_secs(5),
        }
    }
}

/// Limits and timings of one process.
#[derive(Debug, Clone)]
pub struct Config {
    pub frame_limit: usize,
    pub read_chunk: usize,
    /// stderr bytes kept (the last ones).
    pub stderr_tail: usize,
    /// Frames waiting for the consumer before the reader stops.
    pub queue: usize,
    /// Wall-clock limit from spawn; reaching it escalates with `escalation`.
    pub deadline: Option<Duration>,
    /// Used at the deadline and when the [`Process`] is dropped.
    pub escalation: Escalation,
    /// How long the group may take to disappear after the leader is reaped.
    pub reap_grace: Duration,
    /// How long stdout/stderr may stay open once the group is gone (held by escaped descendants).
    pub drain_grace: Duration,
    /// Write every stdout byte, as read and before framing, to this file (created or truncated at spawn).
    pub stdout_copy: Option<PathBuf>,
    /// Write every stderr byte to this file; the bounded tail is kept as well.
    pub stderr_copy: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            frame_limit: FRAME_LIMIT,
            read_chunk: READ_CHUNK,
            stderr_tail: 64 << 10,
            queue: 64,
            deadline: None,
            escalation: Escalation::default(),
            reap_grace: Duration::from_secs(2),
            drain_grace: Duration::from_secs(2),
            stdout_copy: None,
            stderr_copy: None,
        }
    }
}

/// How the leader ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    Code(i32),
    Signal(i32),
}

/// Why the process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The leader exited without being asked.
    Exited,
    Cancelled,
    DeadlineExceeded,
    /// The [`Process`] was dropped.
    Abandoned,
}

/// What became of one output stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamReport {
    pub bytes: u64,
    /// Most bytes the frame reader held at once (stdout only).
    pub max_buffered: usize,
    /// Bytes read and dropped after a refused frame (stdout only).
    pub discarded: u64,
    /// The stream ended (EOF) rather than being abandoned or failing.
    pub eof: bool,
    pub error: Option<String>,
}

/// The last event of a process.
#[derive(Debug, Clone)]
pub struct ExitReport {
    pub pid: u32,
    /// `None` if the leader did not exit even after SIGKILL and the kill grace.
    pub status: Option<Termination>,
    pub cause: Cause,
    pub steps: Vec<Step>,
    /// Group members other than the leader found when the leader exited; they were killed.
    pub orphans: usize,
    /// The leader was reaped and no member of its process group was left within the reap grace.
    pub group_reaped: bool,
    pub stdout: StreamReport,
    pub stderr: StreamReport,
    pub stderr_tail: Vec<u8>,
}

/// What a [`Process`] yields: stdout frames, then exactly one [`Event::Exit`].
#[derive(Debug, Clone)]
pub enum Event {
    Frame(Frame),
    Exit(Box<ExitReport>),
}

/// Requests escalation from any task.
#[derive(Debug, Clone)]
pub struct Control {
    /// A single slot: the first plan is latched at once and later ones are dropped, so requests never queue
    /// (M1T02R-03) and a request made before the supervisor sees the exit still decides the cause (M1T02R-01).
    slot: std::sync::Arc<watch::Sender<Option<Escalation>>>,
}

impl Control {
    /// Ask to end the process with `plan`. The first request wins; it makes the exit report's cause
    /// [`Cause::Cancelled`] unless a deadline or abandonment escalation was already running, even if the process
    /// exits by itself before the plan takes effect.
    pub fn cancel(&self, plan: Escalation) {
        self.slot.send_if_modified(|slot| {
            let first = slot.is_none();
            if first {
                *slot = Some(plan);
            }
            first
        });
    }
}

/// A supervised child process.
#[derive(Debug)]
pub struct Process {
    pid: u32,
    stdin: Option<pipe::Sender>,
    events: mpsc::Receiver<Event>,
    control: Control,
}

impl Process {
    /// Start `command` with piped stdio in a new process group. Must be called within a Tokio runtime with IO and
    /// time enabled.
    pub fn spawn(mut command: Command, config: Config) -> io::Result<Process> {
        // The copies exist before the child does, so a file that cannot be created starts nothing.
        let create = |path: &Option<PathBuf>| path.as_ref().map(std::fs::File::create).transpose();
        let stdout_copy = create(&config.stdout_copy)?;
        let stderr_copy = create(&config.stderr_copy)?;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let pid = Pid::from_child(&child);
        let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(fd) => fd,
            Err(e) => {
                // Without a pidfd the child cannot be supervised: end it now rather than leave it running.
                let _ = kill_process_group(pid, RawSignal::KILL);
                // Unreaped, its pid is still its own (M1T07R-103).
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        };
        // From here on the leader is ours to reap (dropping `Leader` kills the group): until then it holds its pid
        // and process group id.
        let leader = Leader {
            pid,
            pidfd,
            reaped: false,
        };
        let sigchld = signal(SignalKind::child())?;
        let piped = |fd: Option<OwnedFd>| fd.ok_or_else(|| io::Error::other("stdio was not piped"));
        let stdin = pipe::Sender::from_owned_fd(piped(child.stdin.take().map(OwnedFd::from))?)?;
        let stdout = pipe::Receiver::from_owned_fd(piped(child.stdout.take().map(OwnedFd::from))?)?;
        let stderr = pipe::Receiver::from_owned_fd(piped(child.stderr.take().map(OwnedFd::from))?)?;
        // The std handle neither waits nor kills on drop; the leader is reaped through its pidfd.
        drop(child);

        let (events_tx, events) = mpsc::channel(config.queue.max(1));
        let (cancel_tx, cancel_rx) = watch::channel(None);
        let (abandon_tx, abandon) = watch::channel(false);
        let out = tokio::spawn(read_stdout(
            FrameReader::new(
                Tee {
                    inner: stdout,
                    copy: stdout_copy,
                },
                config.frame_limit,
                config.read_chunk,
            ),
            events_tx.clone(),
            abandon.clone(),
        ));
        let err = tokio::spawn(drain_stderr(
            stderr,
            config.stderr_tail,
            stderr_copy,
            abandon,
        ));
        tokio::spawn(supervise(Supervisor {
            leader,
            sigchld,
            config,
            started: tokio::time::Instant::now(),
            events: events_tx,
            cancel: cancel_rx,
            abandon: abandon_tx,
            out,
            err,
        }));
        Ok(Process {
            pid: pid.as_raw_pid() as u32,
            stdin: Some(stdin),
            events,
            control: Control {
                slot: std::sync::Arc::new(cancel_tx),
            },
        })
    }

    /// Process id of the leader, which is also the process group id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The child's stdin; dropping it closes the pipe.
    pub fn take_stdin(&mut self) -> Option<pipe::Sender> {
        self.stdin.take()
    }

    /// The next frame, then the exit report; `None` after that.
    pub async fn next(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    pub fn control(&self) -> Control {
        self.control.clone()
    }

    /// See [`Control::cancel`].
    pub fn cancel(&self, plan: Escalation) {
        self.control.cancel(plan);
    }
}

/// The processes in process group `pgid` (zombies included), from `/proc`.
pub fn group_members(pgid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            // Fields after the parenthesised command name: state, ppid, pgrp.
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    let rest = &stat[stat.rfind(')')? + 1..];
                    rest.split_whitespace().nth(2)?.parse::<u32>().ok()
                })
                == Some(pgid)
        })
        .collect()
}

/// The group leader, reaped only through its pidfd.
struct Leader {
    pid: Pid,
    pidfd: OwnedFd,
    reaped: bool,
}

/// How often the leader is checked when no SIGCHLD arrives (signals coalesce; this is only a fallback).
const EXIT_POLL: Duration = Duration::from_millis(100);

impl Leader {
    /// The exit status if the leader has exited, without reaping it.
    fn status(&self) -> io::Result<Option<Termination>> {
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
        let status = waitid(WaitId::PidFd(self.pidfd.as_fd()), options)?;
        Ok(status.and_then(|s| {
            s.exit_status()
                .map(Termination::Code)
                .or_else(|| s.terminating_signal().map(Termination::Signal))
        }))
    }

    /// Wait for the leader to exit, woken by SIGCHLD (the listener exists before the first check, so no exit is
    /// missed) or by the fallback poll.
    async fn exited(&self, sigchld: &mut tokio::signal::unix::Signal) -> io::Result<Termination> {
        loop {
            if let Some(status) = self.status()? {
                return Ok(status);
            }
            tokio::select! {
                _ = sigchld.recv() => {}
                _ = tokio::time::sleep(EXIT_POLL) => {}
            }
        }
    }

    fn signal(&self, target: Target, signal: Signal) {
        let _ = match target {
            Target::Leader => pidfd_send_signal(&self.pidfd, signal.raw()),
            Target::Group => {
                // A leader that moved itself into another group is not reached through the one it left
                // (M1T07R-103); its pidfd still names it until it is reaped. Whether it left is decided before the
                // group is signalled, so a leader that leaves in its handler for this very signal is not sent it
                // again (FU-01): any signal but SIGKILL reaches the leader at most once, and one that leaves just
                // between the check and the group signal misses it and gets SIGKILL after the grace. SIGKILL goes to
                // the leader in any case (a second one changes nothing).
                let left = self.left_group();
                let sent = kill_process_group(self.pid, signal.raw());
                #[cfg(test)]
                tests::after_group_signal(self.pid);
                if signal == Signal::Kill || left {
                    let _ = pidfd_send_signal(&self.pidfd, signal.raw());
                }
                sent
            }
        };
    }

    /// Whether the leader is no longer in the group it was started in (its own).
    fn left_group(&self) -> bool {
        rustix::process::getpgid(Some(self.pid)).is_ok_and(|group| group != self.pid)
    }

    fn reap(&mut self) -> io::Result<()> {
        waitid(WaitId::PidFd(self.pidfd.as_fd()), WaitIdOptions::EXITED)?;
        self.reaped = true;
        Ok(())
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        // The supervisor was dropped (runtime shutdown) before the leader was reaped: kill the whole group and give
        // the leader a moment to be reaped, so at most a zombie stays until this process exits.
        if self.reaped {
            return;
        }
        let _ = kill_process_group(self.pid, RawSignal::KILL);
        // Also a leader that left its group (M1T07R-103).
        let _ = pidfd_send_signal(&self.pidfd, RawSignal::KILL);
        let reap = WaitIdOptions::EXITED | WaitIdOptions::NOHANG;
        for _ in 0..50 {
            if let Ok(Some(_)) = waitid(WaitId::PidFd(self.pidfd.as_fd()), reap) {
                self.reaped = true;
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

struct Supervisor {
    leader: Leader,
    sigchld: tokio::signal::unix::Signal,
    config: Config,
    started: tokio::time::Instant,
    events: mpsc::Sender<Event>,
    cancel: watch::Receiver<Option<Escalation>>,
    abandon: watch::Sender<bool>,
    out: tokio::task::JoinHandle<StreamReport>,
    err: tokio::task::JoinHandle<(StreamReport, Vec<u8>)>,
}

/// The next escalation action.
#[derive(Clone, Copy)]
enum Next {
    Term,
    Kill,
    GiveUp,
}

/// A running escalation: its plan and the next action with its due time.
struct Escalating {
    plan: Escalation,
    next: Next,
    due: tokio::time::Instant,
}

impl Escalating {
    fn begin(plan: Escalation, leader: &Leader, steps: &mut Vec<Step>) -> Self {
        // Graces run from the moment the previous signal was sent, so recorded steps are at least a grace apart.
        let sent = match plan.native {
            Some(native) => send(leader, steps, Target::Leader, native),
            None => Instant::now(),
        };
        Self {
            plan,
            next: Next::Term,
            due: tokio::time::Instant::from_std(sent + plan.native_grace),
        }
    }

    /// Take the due action; `false` once escalation is exhausted.
    fn advance(&mut self, leader: &Leader, steps: &mut Vec<Step>) -> bool {
        let (next, signal, grace) = match self.next {
            Next::Term => (Next::Kill, Signal::Terminate, self.plan.term_grace),
            Next::Kill => (Next::GiveUp, Signal::Kill, self.plan.kill_grace),
            Next::GiveUp => return false,
        };
        let sent = send(leader, steps, Target::Group, signal);
        (self.next, self.due) = (next, tokio::time::Instant::from_std(sent + grace));
        true
    }
}

/// Send `signal` and record it; returns when it was sent.
fn send(leader: &Leader, steps: &mut Vec<Step>, target: Target, signal: Signal) -> Instant {
    leader.signal(target, signal);
    let at = Instant::now();
    steps.push(Step { at, target, signal });
    at
}

async fn supervise(mut s: Supervisor) {
    let mut cause = Cause::Exited;
    let mut steps = Vec::new();
    let mut escalating: Option<Escalating> = None;
    let mut control_open = true;
    let deadline = s.config.deadline.map(|d| s.started + d);

    let status = loop {
        let idle = escalating.is_none();
        let due = escalating.as_ref().map_or(s.started, |e| e.due);
        let why = tokio::select! {
            biased;
            status = s.leader.exited(&mut s.sigchld) => break status.ok(),
            changed = s.cancel.changed(), if control_open && idle => match changed {
                Ok(()) => s.cancel.borrow_and_update().map(|plan| (plan, Cause::Cancelled)),
                Err(_) => {
                    control_open = false;
                    None
                }
            },
            _ = tokio::time::sleep_until(deadline.unwrap_or(s.started)), if idle && deadline.is_some() => {
                Some((s.config.escalation, Cause::DeadlineExceeded))
            }
            _ = s.events.closed(), if idle => Some((s.config.escalation, Cause::Abandoned)),
            _ = tokio::time::sleep_until(due), if !idle => {
                if let Some(e) = escalating.as_mut() && !e.advance(&s.leader, &mut steps) {
                    break None;
                }
                None
            }
        };
        if let Some((plan, why)) = why {
            cause = why;
            escalating = Some(Escalating::begin(plan, &s.leader, &mut steps));
        }
    };

    let pid = s.leader.pid.as_raw_pid() as u32;
    let mut orphans = 0;
    let mut group_reaped = false;
    if status.is_some() {
        // The leader is an unreaped zombie, so its group id still names only its own group.
        orphans = group_members(pid).into_iter().filter(|&p| p != pid).count();
        s.leader.signal(Target::Group, Signal::Kill);
        if s.leader.reap().is_ok() {
            group_reaped = group_gone(s.leader.pid, s.config.reap_grace).await;
        }
    }

    // Output written before the group went away is still read; only pipes held open beyond the drain grace are
    // abandoned.
    let mut streams = Box::pin(async { (s.out.await, s.err.await) });
    let (out, err) = tokio::select! {
        r = &mut streams => r,
        _ = tokio::time::sleep(s.config.drain_grace) => {
            let _ = s.abandon.send(true);
            streams.await
        }
    };
    let joined = |e: tokio::task::JoinError| StreamReport {
        error: Some(e.to_string()),
        ..StreamReport::default()
    };
    // A cancel latched before the report is made decides the cause even if the exit was seen first (M1T02R-01).
    if cause == Cause::Exited && s.cancel.borrow().is_some() {
        cause = Cause::Cancelled;
    }
    let stdout = out.unwrap_or_else(joined);
    let (stderr, stderr_tail) = err.unwrap_or_else(|e| (joined(e), Vec::new()));
    let report = ExitReport {
        pid,
        status,
        cause,
        steps,
        orphans,
        group_reaped,
        stdout,
        stderr,
        stderr_tail,
    };
    let _ = s.events.send(Event::Exit(Box::new(report))).await;
}

/// Wait until no process is left in group `pgid` (ESRCH), at most `grace`.
async fn group_gone(pgid: Pid, grace: Duration) -> bool {
    let until = tokio::time::Instant::now() + grace;
    loop {
        if test_kill_process_group(pgid).err() == Some(rustix::io::Errno::SRCH) {
            return true;
        }
        if tokio::time::Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn abandoned(abandon: &mut watch::Receiver<bool>) {
    let _ = abandon.wait_for(|a| *a).await;
}

/// A reader that writes what it reads to a file. The writes are plain blocking writes of what one read returned (at
/// most one chunk); a failed write fails the read, so an incomplete copy is reported as a stream error.
struct Tee<R> {
    inner: R,
    copy: Option<std::fs::File>,
}

impl<R: AsyncRead + Unpin> AsyncRead for Tee<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let (Poll::Ready(Ok(())), Some(copy)) = (&polled, self.copy.as_mut())
            && let Err(e) = copy.write_all(&buf.filled()[before..])
        {
            return Poll::Ready(Err(e));
        }
        polled
    }
}

async fn read_stdout(
    mut reader: FrameReader<Tee<pipe::Receiver>>,
    events: mpsc::Sender<Event>,
    mut abandon: watch::Receiver<bool>,
) -> StreamReport {
    let mut report = StreamReport::default();
    let mut refused = false;
    loop {
        let frame = tokio::select! {
            biased;
            frame = reader.next() => frame,
            _ = abandoned(&mut abandon) => break,
        };
        match frame {
            Ok(Some(frame)) => {
                refused = matches!(frame, Frame::TooLarge(_));
                // Waiting here for the consumer is the backpressure; it is never abandoned.
                if events.send(Event::Frame(frame)).await.is_err() {
                    break;
                }
            }
            Ok(None) => {
                report.eof = !refused;
                break;
            }
            Err(e) => {
                report.error = Some(e.to_string());
                break;
            }
        }
    }
    report.bytes = reader.bytes_read();
    report.max_buffered = reader.max_buffered();
    if refused {
        // Keep the child unblocked while the adapter ends it; nothing more is framed or kept.
        let mut inner = reader.into_inner();
        let mut sink = vec![0; READ_CHUNK];
        loop {
            let n = tokio::select! {
                biased;
                n = inner.read(&mut sink) => n,
                _ = abandoned(&mut abandon) => break,
            };
            match n {
                Ok(0) => {
                    report.eof = true;
                    break;
                }
                Ok(n) => report.discarded += n as u64,
                Err(e) => {
                    report.error = Some(e.to_string());
                    break;
                }
            }
        }
        report.bytes += report.discarded;
    }
    report
}

async fn drain_stderr(
    mut stderr: pipe::Receiver,
    keep: usize,
    mut copy: Option<std::fs::File>,
    mut abandon: watch::Receiver<bool>,
) -> (StreamReport, Vec<u8>) {
    let mut report = StreamReport::default();
    let mut tail = Vec::new();
    let mut chunk = vec![0; READ_CHUNK];
    loop {
        let n = tokio::select! {
            biased;
            n = stderr.read(&mut chunk) => n,
            _ = abandoned(&mut abandon) => break,
        };
        match n {
            Ok(0) => {
                report.eof = true;
                break;
            }
            Ok(n) => {
                report.bytes += n as u64;
                if let Some(file) = copy.as_mut()
                    && let Err(e) = file.write_all(&chunk[..n])
                {
                    report.error = Some(format!("stderr copy: {e}"));
                    copy = None;
                }
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > keep {
                    tail.drain(..tail.len() - keep);
                }
            }
            Err(e) => {
                report.error = Some(e.to_string());
                break;
            }
        }
    }
    (report, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Set by a test: after a group signal to this leader, wait until this file exists (the leader's handler ran).
    static AFTER_GROUP_SIGNAL: std::sync::Mutex<Option<(Pid, PathBuf)>> =
        std::sync::Mutex::new(None);

    pub(super) fn after_group_signal(pid: Pid) {
        let marker = AFTER_GROUP_SIGNAL.lock().ok().and_then(|hook| {
            hook.as_ref()
                .filter(|(leader, _)| *leader == pid)
                .map(|(_, marker)| marker.clone())
        });
        if let Some(marker) = marker {
            let until = Instant::now() + Duration::from_secs(2);
            while !marker.exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// A leader whose SIGTERM handler moves it out of its group gets SIGTERM once, not again through its pidfd, and
    /// is still killed after the grace (FU-01).
    #[tokio::test]
    async fn a_leader_leaving_its_group_on_sigterm_gets_it_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (count, marker) = (tmp.path().join("terms"), tmp.path().join("moved"));
        let mut command = std::process::Command::new("python3");
        command.arg("-c").arg(format!(
            "import os, signal, time\n\
             def on_term(*_):\n    open('{count}', 'a').write('term\\n')\n    \
             if not os.path.exists('{marker}'):\n        \
             os.setpgid(0, os.getpgid(os.getppid()))\n        open('{marker}', 'w').close()\n\
             signal.signal(signal.SIGTERM, on_term)\n\
             print(os.getpid(), flush=True)\n\
             while True:\n    time.sleep(1)\n",
            count = count.display(),
            marker = marker.display()
        ));
        let config = Config {
            deadline: Some(Duration::from_millis(300)),
            escalation: Escalation {
                native: None,
                native_grace: Duration::ZERO,
                term_grace: Duration::from_millis(500),
                kill_grace: Duration::from_secs(2),
            },
            ..Config::default()
        };
        let mut p = Process::spawn(command, config)?;
        let leader = Pid::from_raw(p.pid() as i32).ok_or("a pid")?;
        *AFTER_GROUP_SIGNAL.lock().map_err(|_| "hook")? = Some((leader, marker.clone()));
        let report = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = p.next().await {
                if let Event::Exit(report) = event {
                    return Some(*report);
                }
            }
            None
        })
        .await;
        *AFTER_GROUP_SIGNAL.lock().map_err(|_| "hook")? = None;
        let left = Path::new(&format!("/proc/{}", leader.as_raw_pid())).exists();
        if left {
            let _ = rustix::process::kill_process(leader, RawSignal::KILL);
        }
        let report = report?.ok_or("no exit report")?;
        let terms = std::fs::read_to_string(&count)
            .unwrap_or_default()
            .lines()
            .count();
        assert!(marker.exists(), "the handler did not move the leader");
        assert_eq!(terms, 1, "SIGTERM delivered {terms} times");
        assert_eq!(report.status, Some(Termination::Signal(9)), "{report:?}");
        assert!(!left, "leader survived");
        Ok(())
    }
}
