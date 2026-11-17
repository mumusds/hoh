//! The client's one signal state (docs/adr/ADR-031.md, 设计修正（客户端信号）): a client command registers it once,
//! before any wait ([`register`]), and every stage after uses the same one ([`current`]). Every wait the client makes
//! looks at it while it waits ([`wait`]), and every exit consults it first ([`State::exit`]): a received SIGTERM,
//! SIGINT or SIGHUP decides the exit code (143, 130, 129) and the message. The state also holds the client's one start
//! gate, which every process the client starts goes through and which a signal exit stops (设计修正 2, A), and says
//! its messages through one bounded path (rule 6).

use crate::exit::{ClientEvent, client_code};
use crate::owned::{Gate, Stopped};
use hoh_core::spawn::Spawner;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use std::os::fd::AsFd as _;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// How often a wait looks at the state (the shell's tick).
pub const LOOK: Duration = Duration::from_millis(100);

/// How long a signal exit's stop takes at most, from its start, for the helper starts in flight to end and the
/// helpers it killed to be reaped (设计修正 2, D: the first two rows share it).
pub const HELPERS_WAIT: Duration = Duration::from_millis(1500);

/// How long a message waits for stderr (rule 6); not written by then, it is given up.
pub const SAY_WAIT: Duration = Duration::from_millis(500);

/// Where the bounded diagnostics path writes: the process's stderr (fd 2, never through std's lock), or a unit test's
/// descriptor.
#[derive(Clone)]
enum Out {
    Stderr,
    #[cfg(test)]
    Fd(Arc<std::os::fd::OwnedFd>),
}

/// The signal state of a client process: the number of the exit signal that came (0 for none), whether the window was
/// resized, the run the client is about (for the message), and whether a stage already said why it ended.
pub struct State {
    signal: Arc<AtomicUsize>,
    resized: Arc<AtomicBool>,
    run: Mutex<Option<String>>,
    said: AtomicBool,
    /// The start gate every process the client starts goes through (设计修正 2, A).
    gate: Arc<Gate>,
    out: Out,
    /// Messages given up since the last one written (rule 6).
    late: AtomicU64,
}

static STATE: OnceLock<State> = OnceLock::new();

/// The process's signal state, registered now if it is not yet (and never a second time); 74 when the handlers cannot
/// be registered.
pub fn register() -> Result<&'static State, u8> {
    if let Some(state) = STATE.get() {
        return Ok(state);
    }
    let signal = Arc::new(AtomicUsize::new(0));
    for sig in [SIGTERM, SIGHUP, SIGINT] {
        let value = usize::try_from(sig).unwrap_or(usize::MAX);
        if let Err(e) = signal_hook::flag::register_usize(sig, Arc::clone(&signal), value) {
            say!("hoh：无法登记信号处理：{e}");
            return Err(client_code(ClientEvent::TerminalIo));
        }
    }
    let resized = Arc::new(AtomicBool::new(false));
    if let Err(e) = signal_hook::flag::register(SIGWINCH, Arc::clone(&resized)) {
        say!("hoh：无法登记信号处理：{e}");
        return Err(client_code(ClientEvent::TerminalIo));
    }
    let _ = STATE.set(State::with(signal, resized));
    STATE
        .get()
        .ok_or_else(|| client_code(ClientEvent::TerminalIo))
}

/// The process's signal state, if a command registered it.
pub fn current() -> Option<&'static State> {
    STATE.get()
}

/// The spawner of a client's processes: its registered signal state's start gate; with none registered (a unit test of
/// another part), a gate of its own, which nothing stops (设计修正 2, A: library code is always given one).
pub fn spawner() -> Arc<dyn Spawner> {
    current().map_or_else(|| Arc::new(Gate::default()), State::spawner)
}

/// The exit code of signal number `signal`: SIGTERM 143, SIGHUP 129, SIGINT 130 (REQ-004).
pub fn code(signal: usize) -> u8 {
    match i32::try_from(signal) {
        Ok(SIGTERM) => client_code(ClientEvent::Sigterm),
        Ok(SIGHUP) => client_code(ClientEvent::Sighup),
        _ => client_code(ClientEvent::Sigint),
    }
}

impl State {
    fn with(signal: Arc<AtomicUsize>, resized: Arc<AtomicBool>) -> State {
        State::writing(signal, resized, Out::Stderr)
    }

    fn writing(signal: Arc<AtomicUsize>, resized: Arc<AtomicBool>, out: Out) -> State {
        State {
            signal,
            resized,
            run: Mutex::new(None),
            said: AtomicBool::new(false),
            gate: Arc::default(),
            out,
            late: AtomicU64::new(0),
        }
    }

    /// The spawner of everything the client starts: its start gate (设计修正 2, A).
    pub fn spawner(&self) -> Arc<dyn Spawner> {
        self.gate.clone()
    }

    /// Stop the start gate within [`HELPERS_WAIT`]: nothing is started from now on, every helper the client started is
    /// killed (its group and its leader) and reaped, and what could not be confirmed is reported (设计修正 2, A, A2,
    /// D). The host is never signalled (REQ-001 failure mode 3).
    pub fn stop(&self) -> Stopped {
        self.gate.stop(HELPERS_WAIT)
    }

    /// The flag the shell's loop looks at: the number of the signal that came, 0 for none.
    pub fn signal(&self) -> &Arc<AtomicUsize> {
        &self.signal
    }

    /// Whether the window was resized (SIGWINCH).
    pub fn resized(&self) -> &Arc<AtomicBool> {
        &self.resized
    }

    /// The exit code of the signal that came, if one did.
    pub fn received(&self) -> Option<u8> {
        match self.signal.load(Ordering::SeqCst) {
            0 => None,
            signal => Some(code(signal)),
        }
    }

    /// The run this client is about from now on: a signal's message names it.
    pub fn note_run(&self, run_id: &str) {
        *self.run.lock().unwrap_or_else(PoisonError::into_inner) = Some(run_id.to_owned());
    }

    /// Say `text` on stderr, through the bounded path, as why the client ends: the exit adds nothing to it.
    pub fn say(&self, text: &str) {
        self.said(text);
        self.mark_said();
    }

    /// `text` as a line through the one bounded diagnostics path (rule 6): written straight to stderr's descriptor,
    /// never through std's stderr lock (the forwarder may hold it, blocked on a full pipe), by a thread of its own that
    /// is waited for at most [`SAY_WAIT`]; not written by then (or with no thread for it), it is given up and counted,
    /// and the next line said here first says how many were. Whether it was written.
    fn said(&self, text: &str) -> bool {
        say_on(&self.out, &self.late, text, &say_thread)
    }

    /// A stage said why the client ends (the follower's conclusion), or says nothing on purpose (the shell, which only
    /// restores the terminal).
    pub fn mark_said(&self) {
        self.said.store(true, Ordering::SeqCst);
    }

    /// The exit code of a client that ends with `code` (rule 2 of the design correction): a received signal's instead,
    /// whatever ended it. Unless a stage said why, it is said: the run and how to attach again when there is one, else
    /// that the client ends `before` anything (“没有提交命令”, “没有附着”).
    pub fn exit(&self, code: u8, before: &str) -> u8 {
        let Some(signal) = self.received() else {
            return code;
        };
        // Nothing the client owns outlives it (rule 5), and the message never waits on stderr (rule 6).
        self.stop();
        // Where a test holds the exit once the stop is done (debug builds only).
        crate::debug_hold::at("exit");
        if !self.said.swap(true, Ordering::SeqCst) {
            let run = self
                .run
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            self.said(&match run {
                Some(run) => format!(
                    "hoh: 收到信号，已脱离 Run {run}（它在后台继续）；用 hoh attach {run} 重新附着"
                ),
                None => format!("hoh: 收到信号，{before}"),
            });
        }
        signal
    }

    /// [`wait`] with this state.
    pub fn wait<T: Send + 'static>(
        &self,
        op: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, u8> {
        wait_with(Some(self), &thread, op)
    }
}

/// Write all of `bytes` to `out`, straight to its descriptor; whether it all went.
fn write_out(out: &Out, mut bytes: &[u8]) -> bool {
    let stderr = std::io::stderr();
    let fd = match out {
        Out::Stderr => stderr.as_fd(),
        #[cfg(test)]
        Out::Fd(fd) => fd.as_fd(),
    };
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(0) => return false,
            Ok(n) => bytes = &bytes[n..],
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return false,
        }
    }
    true
}

/// [`State::said`] on `out`, counting what is given up in `late`, with threads made by `spawn`.
fn say_on(out: &Out, late: &AtomicU64, text: &str, spawn: Spawn<'_>) -> bool {
    let before = late.swap(0, Ordering::SeqCst);
    let mut line = String::new();
    if before > 0 {
        line.push_str(&format!(
            "hoh: 此前有 {before} 条消息没能及时写到 stderr，已放弃\n"
        ));
    }
    line.push_str(text);
    line.push('\n');
    let (sent, got) = std::sync::mpsc::channel();
    let target = out.clone();
    let written = spawn(Box::new(move || {
        let _ = sent.send(write_out(&target, line.as_bytes()));
    }))
    .is_ok()
        && got.recv_timeout(SAY_WAIT) == Ok(true);
    if !written {
        late.fetch_add(before + 1, Ordering::SeqCst);
    }
    written
}

/// [`Spawn`] of the diagnostics path's writer.
fn say_thread(work: Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("hoh-say".to_owned())
        .spawn(work)
        .map(drop)
}

/// A thread running `work`, or why it could not be made.
type Spawn<'a> = &'a dyn Fn(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()>;

/// [`Spawn`] by `std::thread::Builder`.
fn thread(work: Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("hoh-wait".to_owned())
        .spawn(work)
        .map(drop)
}

/// Make the client wait `op` (finding or starting the host, a getRun, opening the event stream) on a thread of its own
/// while the caller looks at the process's signal state every [`LOOK`] (rule 3): `Err` with a signal's exit code as
/// soon as one came, before `op` answers or as it answers (rule 2: the signal decides even when `op` failed). Without a
/// thread `op` runs here and the state is consulted once it returns (the design correction's exemption). With no state
/// registered (a unit test of another part) `op` just runs. What `op` started goes on; a panic in it is the caller's.
pub fn wait<T: Send + 'static>(op: impl FnOnce() -> T + Send + 'static) -> Result<T, u8> {
    wait_with(current(), &thread, op)
}

/// [`wait`] with `state` and threads made by `spawn`.
fn wait_with<T: Send + 'static>(
    state: Option<&State>,
    spawn: Spawn<'_>,
    op: impl FnOnce() -> T + Send + 'static,
) -> Result<T, u8> {
    let Some(state) = state else {
        return Ok(op());
    };
    if let Some(code) = state.received() {
        return Err(code);
    }
    let slot = Arc::new(Mutex::new(Some(op)));
    let take = |slot: &Mutex<Option<_>>| slot.lock().unwrap_or_else(PoisonError::into_inner).take();
    let (send, got) = std::sync::mpsc::channel();
    let work = {
        let slot = slot.clone();
        Box::new(move || {
            if let Some(op) = take(&slot) {
                let _ = send.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(op)));
            }
        })
    };
    let answered = |done: T| match state.received() {
        Some(code) => Err(code),
        None => Ok(done),
    };
    // A thread that could not be made never took `op`: it runs here.
    if spawn(work).is_err()
        && let Some(op) = take(&slot)
    {
        return answered(op());
    }
    loop {
        if let Some(code) = state.received() {
            return Err(code);
        }
        match got.recv_timeout(LOOK) {
            Ok(Ok(done)) => return answered(done),
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::panic::resume_unwind(Box::new(
                "a client wait's thread ended without an answer",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State::with(Arc::default(), Arc::default())
    }

    fn no_thread(_work: Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(11))
    }

    #[test]
    fn a_wait_whose_op_fails_after_a_signal_ends_with_the_signals_code() {
        // ADR-031's design correction (client signals), rule 2 (CLT-100): a start that fails right after SIGTERM came
        // (the error answering before the next look) ends the client with 143, not with the start's error.
        let state = state();
        let flag = state.signal().clone();
        let failed = wait_with(Some(&state), &thread, move || {
            flag.store(SIGTERM as usize, Ordering::SeqCst);
            Err::<(), &str>("the host did not become ready in 10 s")
        });
        assert_eq!(failed, Err(143));
    }

    #[test]
    fn a_wait_without_a_thread_runs_in_place_and_still_ends_with_the_signals_code() {
        // ADR-031's design correction (client signals), the exemption (CLT-100): no thread for the wait, so `op` runs
        // in place; SIGINT came while it ran and it then failed: the client ends with 130, not with the error.
        let state = state();
        let flag = state.signal().clone();
        let failed = wait_with(Some(&state), &no_thread, move || {
            flag.store(SIGINT as usize, Ordering::SeqCst);
            Err::<(), &str>("another client is starting the host and it did not become ready")
        });
        assert_eq!(failed, Err(130));
        // With no signal, its own answer, in place as on a thread.
        let state = self::state();
        assert_eq!(wait_with(Some(&state), &no_thread, || 7), Ok(7));
        assert_eq!(wait_with(Some(&state), &thread, || 8), Ok(8));
    }

    #[test]
    fn a_wait_ends_at_its_next_look_after_a_signal_and_never_starts_after_one() {
        // Rule 3: a wait that does not answer ends within a look of the signal; one asked after a signal does not run.
        let state = state();
        let flag = state.signal().clone();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag.store(SIGHUP as usize, Ordering::SeqCst);
        });
        let waited = wait_with(Some(&state), &thread, move || {
            let _ = held.recv_timeout(Duration::from_secs(10));
        });
        assert_eq!(waited, Err(129));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        drop(release);
        let ran = Arc::new(AtomicBool::new(false));
        let mark = ran.clone();
        assert_eq!(
            wait_with(Some(&state), &thread, move || mark
                .store(true, Ordering::SeqCst)),
            Err(129)
        );
        assert!(
            !ran.load(Ordering::SeqCst),
            "a wait asked after the signal ran"
        );
    }

    /// A state writing to one end of a socket pair; the other end, to read what it said.
    fn captured() -> (State, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let out = Out::Fd(Arc::new(std::os::fd::OwnedFd::from(ours)));
        (State::writing(Arc::default(), Arc::default(), out), theirs)
    }

    /// What is waiting to be read on `from` (without waiting for more).
    fn waiting(from: &std::os::unix::net::UnixStream) -> String {
        use std::io::Read as _;
        from.set_nonblocking(true).expect("nonblocking");
        let mut said = Vec::new();
        let mut piece = [0u8; 4096];
        while let Ok(n) = (&*from).read(&mut piece) {
            if n == 0 {
                break;
            }
            said.extend_from_slice(&piece[..n]);
        }
        String::from_utf8_lossy(&said).into_owned()
    }

    #[test]
    fn the_exit_takes_a_received_signals_code_and_says_why_once() {
        // Rule 2 (CLT-251): with no signal the exit is the code and says nothing; with one it is the signal's code, said
        // once, in rule 2's words for an attach that attached to nothing, and a second exit says nothing more.
        let (state, said) = captured();
        assert_eq!(state.exit(69, "没有附着"), 69);
        assert_eq!(waiting(&said), "");
        state.signal().store(SIGTERM as usize, Ordering::SeqCst);
        assert_eq!(state.exit(69, "没有附着"), 143);
        assert_eq!(waiting(&said), "hoh: 收到信号，没有附着\n");
        assert_eq!(state.exit(0, "没有附着"), 143);
        assert_eq!(waiting(&said), "");
        // With a run, its id and how to attach again.
        let (state, said) = captured();
        state.note_run("R");
        state.signal().store(SIGINT as usize, Ordering::SeqCst);
        assert_eq!(state.exit(69, "没有提交命令"), 130);
        assert_eq!(
            waiting(&said),
            "hoh: 收到信号，已脱离 Run R（它在后台继续）；用 hoh attach R 重新附着\n"
        );
        assert_eq!(code(SIGHUP as usize), 129);
        assert_eq!(code(SIGINT as usize), 130);
    }

    #[test]
    fn a_message_stderr_does_not_take_is_given_up_within_its_bound_and_counted() {
        // Rule 6 (CLT-201): a line stderr does not take (its buffer full, nobody reading) is given up after at most
        // SAY_WAIT, never waited for, and counted; the next line, once stderr takes it, says how many were given up.
        // Without a thread to write it, it is given up at once.
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        ours.set_nonblocking(true).expect("nonblocking");
        let piece = [b'N'; 4096];
        while std::io::Write::write(&mut &ours, &piece).is_ok() {}
        ours.set_nonblocking(false).expect("blocking");
        let out = Out::Fd(Arc::new(std::os::fd::OwnedFd::from(ours)));
        let late = AtomicU64::new(0);
        let at = std::time::Instant::now();
        assert!(!say_on(&out, &late, "hoh: 收到信号，没有附着", &say_thread));
        let took = at.elapsed();
        assert!(took >= SAY_WAIT && took < SAY_WAIT * 3, "{took:?}");
        assert_eq!(late.load(Ordering::SeqCst), 1);
        let no_thread =
            |_: Box<dyn FnOnce() + Send + 'static>| Err(std::io::Error::from_raw_os_error(11));
        let at = std::time::Instant::now();
        assert!(!say_on(&out, &late, "hoh: 再一条", &no_thread));
        assert!(at.elapsed() < SAY_WAIT, "{:?}", at.elapsed());
        assert_eq!(late.load(Ordering::SeqCst), 2);
        // Read from now on: the next line goes, and says the two given up before it.
        let reader = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut said = Vec::new();
            let _ = (&theirs).read_to_end(&mut said);
            String::from_utf8_lossy(&said).into_owned()
        });
        assert!(say_on(&out, &late, "hoh: 第三条", &say_thread));
        assert_eq!(late.load(Ordering::SeqCst), 0);
        drop(out);
        let said = reader.join().expect("the reader");
        assert!(
            said.contains("hoh: 此前有 2 条消息没能及时写到 stderr，已放弃\nhoh: 第三条\n"),
            "{}",
            said.trim_start_matches('N')
        );
    }
}
