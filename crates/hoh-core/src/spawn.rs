//! Who starts a process, and how it is owned until it is reaped (docs/adr/ADR-031.md, 设计修正 2, A and A2).
//!
//! Library code that starts a process takes a [`Spawner`] as a parameter, never a default: the host passes
//! [`Direct`], a client its signal state's gate. A helper (a probe command, an agent, the WAL self-test probe) comes
//! back as a [`Helper`], a managed handle whose [`Shared`] state is the one place its leader is reaped; a host comes
//! back as a [`WaitOnly`], which can be waited for and nothing else.
//!
//! The order a helper ends in is the one the probe and the adapters' supervisor have always used: see that the leader
//! exited without consuming its status ([`Shared::observe`]), kill what is left of its process group while the zombie
//! leader still holds the group's id ([`Shared::signal_group`]), then reap it ([`Shared::reap`]), the one step that
//! consumes the status and revokes numeric group signals. No lock is held across a wait.

use rustix::process::{
    Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, WaitIdStatus, kill_process_group, pidfd_open,
    pidfd_send_signal, waitid,
};
use std::fmt;
use std::io;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Where a helper's stderr goes: every start names it, and the spawner sets it before the spawn, so a helper never
/// inherits its starter's fd 2 (docs/adr/ADR-031.md, 设计修正 2, C and round 3: in a client only the writer writes fd 2,
/// CLT-1400).
#[derive(Debug)]
pub enum HelperStderr {
    /// The null device.
    Null,
    /// A pipe of its own, handed over in [`Helper::stderr`].
    Piped,
    /// A file it is given.
    File(std::fs::File),
}

impl HelperStderr {
    fn stdio(self) -> std::process::Stdio {
        match self {
            HelperStderr::Null => std::process::Stdio::null(),
            HelperStderr::Piped => std::process::Stdio::piped(),
            HelperStderr::File(file) => std::process::Stdio::from(file),
        }
    }
}

/// Who starts the processes of some code (docs/adr/ADR-031.md, 设计修正 2, A).
pub trait Spawner: Send + Sync + fmt::Debug {
    /// Start `command` as a helper, in a process group of its own, its stderr as `stderr` says (whatever `command`
    /// was left with): its managed handle, or why not (a client's gate refuses once its client stops).
    fn helper(&self, command: &mut Command, stderr: HelperStderr) -> io::Result<Helper>;

    /// Start `command` as a host: an object that can only be waited for, never signalled.
    fn host(&self, command: &mut Command) -> io::Result<WaitOnly>;
}

/// The spawner that just starts the process: the host's own (its stop is its `ProbeStop`), a client's gate's
/// underneath, and tests'. Made only by [`Direct::plain`] or [`Direct::shared`], so every place that makes one names its
/// type and the spawn-site scan sees it (docs/adr/ADR-031.md, 设计修正 2, E). It has no `Default`, which code could reach
/// without naming it.
#[derive(Debug, Clone, Copy)]
pub struct Direct {
    _made_by_plain: (),
}

impl Direct {
    /// A [`Direct`].
    pub const fn plain() -> Direct {
        Direct { _made_by_plain: () }
    }

    /// A [`Direct`], shared.
    pub fn shared() -> Arc<dyn Spawner> {
        Arc::new(Direct::plain())
    }
}

impl Spawner for Direct {
    fn helper(&self, command: &mut Command, stderr: HelperStderr) -> io::Result<Helper> {
        Helper::spawn(command.stderr(stderr.stdio()))
    }

    fn host(&self, command: &mut Command) -> io::Result<WaitOnly> {
        command.spawn().map(WaitOnly)
    }
}

/// The spawner of code that starts nothing (a compat replay judges recorded output): it refuses every start.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStart;

impl NoStart {
    /// [`NoStart`], shared.
    pub fn shared() -> Arc<dyn Spawner> {
        Arc::new(NoStart)
    }
}

impl Spawner for NoStart {
    fn helper(&self, _command: &mut Command, _stderr: HelperStderr) -> io::Result<Helper> {
        Err(io::Error::other("this code starts no process"))
    }

    fn host(&self, _command: &mut Command) -> io::Result<WaitOnly> {
        Err(io::Error::other("this code starts no process"))
    }
}

/// How long [`Shared::end`] waits, by default, for a killed leader to exit before it leaves it to whoever owns it.
pub const END_WAIT: Duration = Duration::from_secs(1);

/// A helper's managed handle: its standard streams, taken from the child, and its [`Shared`] state.
#[derive(Debug)]
pub struct Helper {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    pub shared: Arc<Shared>,
}

impl Helper {
    /// Spawn `command` in a process group of its own and own it: private to this module, so only [`Direct`] starts a
    /// helper this way and every other start goes through a [`Spawner`] (CLT-1202). When its pidfd cannot be had, the
    /// child is ended as the adapters' supervisor has always ended it (M1T07R-103): its group killed, then its leader,
    /// then reaped within [`END_WAIT`], its pid and group id fixed until then because it is not reaped (CLT-700).
    fn spawn(command: &mut Command) -> io::Result<Helper> {
        Helper::spawn_by(
            command,
            &|pid| Ok(pidfd_open(pid, PidfdFlags::empty())?),
            END_WAIT,
        )
    }

    /// [`Helper::spawn`] with `open` opening the child's pidfd (a test's seam for its failure).
    fn spawn_by(
        command: &mut Command,
        open: &dyn Fn(Pid) -> io::Result<OwnedFd>,
        bound: Duration,
    ) -> io::Result<Helper> {
        let mut child = command.process_group(0).spawn()?;
        let pid = Pid::from_child(&child);
        let pidfd = match open(pid) {
            Ok(fd) => fd,
            Err(e) => {
                let _ = kill_process_group(pid, Signal::KILL);
                let _ = child.kill();
                let end = Instant::now() + bound;
                while matches!(child.try_wait(), Ok(None)) && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(2));
                }
                // Not reaped in time: a thread of its own sees it exit, cleans its group while the zombie still holds the
                // group's id, then reaps it, so it never stays a zombie of this process (CLT-1258, CLN4-54); with no
                // thread for it, it does until this process ends (a stated residual).
                if matches!(child.try_wait(), Ok(None)) {
                    let _ = std::thread::Builder::new()
                        .name("hoh-reap".to_owned())
                        .spawn(move || reap_child_later(child));
                }
                return Err(e);
            }
        };
        Ok(Helper::own(&mut child, pid, pidfd))
    }

    /// The handle of `child` (`pid`, with `pidfd`): its streams taken, and the std handle let go of (it neither
    /// waits nor kills on drop; the leader is reaped through its pidfd).
    fn own(child: &mut Child, pid: Pid, pidfd: OwnedFd) -> Helper {
        Helper {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            shared: Arc::new(Shared {
                pid,
                pidfd,
                state: Mutex::new(Reaped {
                    status: None,
                    reaped: false,
                }),
            }),
        }
    }
}

/// What a helper's lock guards: whether it was reaped and with what status (numeric group signals are allowed only
/// while it was not).
#[derive(Debug)]
struct Reaped {
    status: Option<ExitStatus>,
    reaped: bool,
}

/// A helper's state, shared by whoever handles it (the library that started it, a client's gate): its pidfd, its
/// process group (the leader's pid), and whether it was reaped.
#[derive(Debug)]
pub struct Shared {
    pid: Pid,
    pidfd: OwnedFd,
    state: Mutex<Reaped>,
}

/// A raw wait status from a `waitid` answer.
fn exit_status(status: &WaitIdStatus) -> Option<ExitStatus> {
    if let Some(code) = status.exit_status() {
        return Some(ExitStatus::from_raw((code & 0xff) << 8));
    }
    status
        .terminating_signal()
        .map(|signal| ExitStatus::from_raw(signal & 0x7f))
}

impl Shared {
    /// Its leader's pid (its process group's id).
    pub fn pid(&self) -> u32 {
        self.pid.as_raw_pid().unsigned_abs()
    }

    /// Its pidfd, for a wait that polls it.
    pub fn pidfd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.pidfd.as_fd()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Reaped> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Send `signal` to the leader through its pidfd: no lock (the pidfd names this process, also once it exited).
    pub fn signal_leader(&self, signal: Signal) -> io::Result<()> {
        Ok(pidfd_send_signal(&self.pidfd, signal)?)
    }

    /// SIGKILL the leader through its pidfd.
    pub fn kill(&self) {
        let _ = self.signal_leader(Signal::KILL);
    }

    /// Send `signal` to its process group by number, only while the leader is not reaped (its group id is then still
    /// its own); whether it was sent.
    pub fn signal_group(&self, signal: Signal) -> bool {
        let state = self.locked();
        !state.reaped && kill_process_group(self.pid, signal).is_ok()
    }

    /// Its exit status if it exited, without consuming it (or the status published when it was reaped).
    pub fn observe(&self) -> io::Result<Option<ExitStatus>> {
        let state = self.locked();
        if state.reaped {
            return Ok(state.status);
        }
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
        Ok(waitid(WaitId::PidFd(self.pidfd.as_fd()), options)?.and_then(|s| exit_status(&s)))
    }

    /// Reap it if it exited: the one step that consumes its status, publishes it and revokes numeric group signals.
    /// `None` while it runs.
    pub fn reap(&self) -> io::Result<Option<ExitStatus>> {
        let mut state = self.locked();
        if state.reaped {
            return Ok(state.status);
        }
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG;
        let Some(status) = waitid(WaitId::PidFd(self.pidfd.as_fd()), options)? else {
            return Ok(None);
        };
        state.status = exit_status(&status);
        state.reaped = true;
        Ok(state.status)
    }

    /// Whether it was reaped.
    pub fn reaped(&self) -> bool {
        self.locked().reaped
    }

    /// Wait until its leader exited (not reaping it), at most `limit`, no lock held: whether it did.
    pub fn exited_within(&self, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        loop {
            match self.observe() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(2)),
                _ => return false,
            }
        }
    }

    /// End it: kill its leader and its group, wait at most `limit` for the leader to exit, then see it exited, kill
    /// what is left of its group, and reap it. Its status if reaped; `None` when it did not exit in time (its owner
    /// keeps it to reap later).
    pub fn end(&self, limit: Duration) -> Option<ExitStatus> {
        self.kill();
        self.signal_group(Signal::KILL);
        if !self.exited_within(limit) {
            return None;
        }
        self.signal_group(Signal::KILL);
        self.reap().ok().flatten()
    }

    /// [`Shared::end`] within `limit`; a leader that did not exit by then (a call SIGKILL does not end) is left to a
    /// reaper thread of its own that goes on in the same order, so it is never left unreaped in a long-lived process
    /// (CLT-702). When no such thread can be made, it stays a zombie until this process ends (stated residual).
    pub fn end_or_reap_later(self: &Arc<Self>, limit: Duration) -> Option<ExitStatus> {
        if let Some(status) = self.end(limit) {
            return Some(status);
        }
        let shared = self.clone();
        let _ = std::thread::Builder::new()
            .name("hoh-reap".to_owned())
            .spawn(
                move || while shared.end(Duration::from_secs(60)).is_none() && !shared.reaped() {},
            );
        None
    }
}

impl Drop for Shared {
    /// The last owner let it go (A2's bounded drop, CLT-1257): a leader not yet reaped is killed with what is left of
    /// its group, waited for at most [`END_WAIT`] and reaped; one that has not exited by then goes to a reaper thread of
    /// its own, which sees it exit, kills what is left of its group while the zombie holds the group's id, and reaps
    /// it. With no thread (or no duplicate of its pidfd) for that, it stays a zombie until this process ends (a stated
    /// residual).
    fn drop(&mut self) {
        self.let_go(END_WAIT);
    }
}

impl Shared {
    /// [`Drop`]'s work with `bound` for the wait: ended within it, or handed to a reaper thread ([`Shared::hand_over`]).
    fn let_go(&self, bound: Duration) {
        if self.reaped() || self.end(bound).is_some() || self.reaped() {
            return;
        }
        self.hand_over();
    }

    /// Hand the leader to a reaper thread of its own ([`reap_later`]); once that thread runs, this handle counts as
    /// reaped and never signals the group by number again. Whether it was handed over.
    fn hand_over(&self) -> bool {
        let Ok(pidfd) = self.pidfd.try_clone() else {
            return false;
        };
        let pid = self.pid;
        let made = std::thread::Builder::new()
            .name("hoh-reap".to_owned())
            .spawn(move || reap_later(pid, &pidfd));
        if made.is_ok() {
            self.locked().reaped = true;
        }
        made.is_ok()
    }
}

/// Wait, without consuming it, until the process `id` names has exited: whether it did (an error other than EINTR,
/// ECHILD say, is no exit seen).
fn exited<'a>(id: impl Fn() -> WaitId<'a>) -> bool {
    let seen = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    loop {
        match waitid(id(), seen) {
            Ok(Some(_)) => return true,
            Err(rustix::io::Errno::INTR) => {}
            _ => return false,
        }
    }
}

/// The wrapping failure's reaper: keep the unreaped leader's group identity until its after-exit cleanup.
fn reap_child_later(mut child: Child) {
    let pid = Pid::from_child(&child);
    if exited(|| WaitId::Pid(pid)) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
    let _ = child.wait();
}

/// A leader its handle let go of, still running: wait for its exit on `pidfd` without consuming it, kill what is left of
/// its group (the zombie still holds the group's id), then reap it, consuming the status (A2's order).
fn reap_later(pid: Pid, pidfd: &OwnedFd) {
    // Only once its exit is seen (and not consumed): its group's id is then still the zombie's (CLN4-53).
    if !exited(|| WaitId::PidFd(pidfd.as_fd())) {
        return;
    }
    let _ = kill_process_group(pid, Signal::KILL);
    while let Err(rustix::io::Errno::INTR) =
        waitid(WaitId::PidFd(pidfd.as_fd()), WaitIdOptions::EXITED)
    {}
}

/// A host, as its starter holds it: it can be waited for and its stderr read, nothing else; dropping it does
/// nothing (docs/adr/ADR-031.md, 设计修正 2, A: the host is never killed by the client).
#[derive(Debug)]
pub struct WaitOnly(Child);

impl WaitOnly {
    /// Its pid.
    pub fn id(&self) -> u32 {
        self.0.id()
    }

    /// Its stderr, when it was piped.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.0.stderr.take()
    }

    /// Its exit status if it exited (reaping it then).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.0.try_wait()
    }

    /// Wait for it to exit and reap it.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.0.wait()
    }
}

#[cfg(test)]
mod tests;
