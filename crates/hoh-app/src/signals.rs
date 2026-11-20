//! The client's one signal state (docs/adr/ADR-031.md, 设计修正（客户端信号）): a client command registers it once,
//! before any wait ([`register`]), and every stage after uses the same one ([`current`]). Every wait the client makes
//! looks at it while it waits ([`wait`]), and every exit consults it first ([`State::exit`]): a received SIGTERM,
//! SIGINT or SIGHUP decides the exit code (143, 130, 129) and the message. The state also holds the client's one start
//! gate, which every process the client starts goes through and which a signal exit stops (设计修正 2, A), and the
//! client's one stderr writer, made as the state is registered (设计修正 2, C; rule 6).

use crate::exit::{ClientEvent, client_code};
use crate::owned::{Gate, Stopped};
use crate::stderr::{MakeSink, Writer};
use hoh_core::spawn::Spawner;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// How often a wait looks at the state (the shell's tick).
pub const LOOK: Duration = Duration::from_millis(100);

/// How long a signal exit's stop takes at most, from its start, for the helper starts in flight to end and the
/// helpers it killed to be reaped (设计修正 2, D: the first two rows share it).
pub const HELPERS_WAIT: Duration = Duration::from_millis(1500);

/// How many pids the exit names of the helpers it could not confirm ended.
const NAMED: usize = 8;

/// The signal state of a client process: the number of the exit signal that came (0 for none), whether the window was
/// resized, the run the client is about (for the message), and whether a stage already said why it ended.
pub struct State {
    signal: Arc<AtomicUsize>,
    resized: Arc<AtomicBool>,
    run: Mutex<Option<String>>,
    said: AtomicBool,
    /// The start gate every process the client starts goes through (设计修正 2, A).
    gate: Arc<Gate>,
    /// The client's one stderr writer (设计修正 2, C).
    writer: Arc<Writer>,
    /// The unit whose `systemctl --user start` is running, if one is (设计修正 2, A: the unit mode).
    unit: Mutex<Option<String>>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// The process's signal state, registered now if it is not yet (and never a second time); 74 when the handlers cannot
/// be registered, or when the stderr writer's thread cannot be made (nothing is written then; a signal that came once
/// the handlers were in decides the code instead: 设计修正 2, C).
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
    let state = assemble(
        signal,
        resized,
        crate::stderr::fd2(),
        &crate::stderr::thread("hoh-stderr"),
    )?;
    let _ = STATE.set(state);
    STATE
        .get()
        .ok_or_else(|| client_code(ClientEvent::TerminalIo))
}

/// The signal state of `signal` and `resized` with its stderr writer, made by `spawn` writing to what `make` makes; or,
/// with no thread for the writer, the exit code: the signal's when one came, else 74.
fn assemble(
    signal: Arc<AtomicUsize>,
    resized: Arc<AtomicBool>,
    make: MakeSink,
    spawn: crate::stderr::Spawn<'_>,
) -> Result<State, u8> {
    match Writer::start(make, spawn) {
        Ok(writer) => Ok(State::writing(signal, resized, writer)),
        Err(_) => Err(match signal.load(Ordering::SeqCst) {
            0 => client_code(ClientEvent::TerminalIo),
            signal => code(signal),
        }),
    }
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
    fn writing(signal: Arc<AtomicUsize>, resized: Arc<AtomicBool>, writer: Arc<Writer>) -> State {
        State {
            gate: Arc::new(Gate::watching(signal.clone())),
            signal,
            resized,
            run: Mutex::new(None),
            said: AtomicBool::new(false),
            writer,
            unit: Mutex::new(None),
        }
    }

    /// The client's one stderr writer.
    pub fn writer(&self) -> &Arc<Writer> {
        &self.writer
    }

    /// Unit `name`'s `systemctl --user start` runs until the guard is dropped: a stop that begins meanwhile says the
    /// start was interrupted and its result is unknown (设计修正 2, A and D).
    pub fn unit_starting(&self, name: &str) -> UnitStart<'_> {
        *self.unit.lock().unwrap_or_else(PoisonError::into_inner) = Some(name.to_owned());
        UnitStart(self)
    }

    /// The spawner of everything the client starts: its start gate (设计修正 2, A).
    pub fn spawner(&self) -> Arc<dyn Spawner> {
        self.gate.clone()
    }

    /// Have the stop also kill every descendant of this process, which becomes their subreaper (doctor, 设计修正 2, B).
    pub fn adopt_descendants(&self) -> std::io::Result<()> {
        self.gate.adopt_descendants()
    }

    /// Stop the start gate within [`HELPERS_WAIT`]: nothing is started from now on, every helper the client started is
    /// killed (its group and its leader) and reaped, and what could not be confirmed is reported (设计修正 2, A, A2,
    /// D), with the unit whose start was running as the stop began. The host is never signalled (REQ-001 failure mode
    /// 3).
    pub fn stop(&self) -> (Stopped, Option<String>) {
        // The unit start seen and the gate closed under the unit's lock: a start that marks itself after this finds
        // the gate closed and starts nothing.
        let unit = {
            let unit = self.unit.lock().unwrap_or_else(PoisonError::into_inner);
            self.gate.refuse();
            unit.clone()
        };
        // Where a test sees that the stop began (debug builds only).
        crate::debug_hold::mark("stopping", "");
        (self.gate.stop(HELPERS_WAIT), unit)
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

    /// Say `text` on stderr as why the client ends (rule 2's sentence: first among the client's own words at the
    /// exit, 设计修正 2, C): the exit adds nothing to it. Said once it is queued.
    pub fn say(&self, text: &str) {
        self.writer.front(text);
        self.mark_said();
    }

    /// A stage said why the client ends (the follower's conclusion), or says nothing on purpose (the shell, which only
    /// restores the terminal).
    pub fn mark_said(&self) {
        self.said.store(true, Ordering::SeqCst);
    }

    /// The exit code of a client that ends with `code` (rule 2 of the design correction): a received signal's instead,
    /// whatever ended it. Unless a stage said why, it is said: the run and how to attach again when there is one, else
    /// that the client ends `before` anything (“没有提交命令”, “没有附着”).
    /// The exit writes, after them, only what the stop could not finish (设计修正 2, D), and the writer's exit writes
    /// all of it within its bound (C).
    pub fn exit(&self, code: u8, before: &str) -> u8 {
        // Nothing the client owns outlives it, whatever ends it (设计修正 2, B; at a normal end its helpers have ended,
        // so this is quick).
        let (stopped, unit) = self.stop();
        if self.received().is_some() {
            // Where a test holds a signal exit once the stop is done (debug builds only).
            crate::debug_hold::at("exit");
        }
        // Rule 2's sentence, once, for a signal that came by now; looked for again while the exit writes (CLT-1201).
        let sentence = || {
            (self.received().is_some() && !self.said.swap(true, Ordering::SeqCst)).then(|| {
                let run = self
                    .run
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                match run {
                    Some(run) => format!(
                        "hoh: 收到信号，已脱离 Run {run}（它在后台继续）；用 hoh attach {run} 重新附着"
                    ),
                    None => format!("hoh: 收到信号，{before}"),
                }
            })
        };
        if let Some(text) = sentence() {
            self.writer.front(&text);
        }
        let end = std::time::Instant::now() + crate::stderr::EXIT_WAIT;
        self.writer.close(&unfinished(&stopped, unit.as_deref()));
        self.writer.wait(end, &sentence);
        // A signal that came as the writing ended: its sentence in what is left of the bound, and its code.
        if let Some(text) = sentence() {
            self.writer.put_front(&text);
            self.writer.wait(end, &sentence);
        }
        self.received().unwrap_or(code)
    }

    /// [`wait`] with this state.
    pub fn wait<T: Send + 'static>(
        &self,
        op: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, u8> {
        wait_with(Some(self), &thread, op)
    }
}

/// Marks unit `name`'s start as running while it lives ([`State::unit_starting`]).
pub struct UnitStart<'a>(&'a State);

impl Drop for UnitStart<'_> {
    fn drop(&mut self) {
        *self.0.unit.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// What the exit says of a stop that did not finish (设计修正 2, D), and nothing for one that did: helper starts still
/// in flight at its bound, helpers not confirmed ended (their pids, the first [`NAMED`]), a unit start interrupted
/// (`unit`). Never a positive statement.
fn unfinished(stopped: &Stopped, unit: Option<&str>) -> Vec<String> {
    let mut said = Vec::new();
    match stopped.helpers_in_flight {
        0 => {}
        1 => said.push("hoh: 一次启动仍未返回，退出后可能出现进程（pid 未知）".to_owned()),
        n => said.push(format!(
            "hoh: {n} 次启动仍未返回，退出后可能出现进程（pid 未知）"
        )),
    }
    if !stopped.unconfirmed.is_empty() {
        let mut pids: Vec<String> = stopped
            .unconfirmed
            .iter()
            .take(NAMED)
            .map(u32::to_string)
            .collect();
        if stopped.unconfirmed.len() > NAMED {
            pids.push("…".to_owned());
        }
        said.push(format!(
            "hoh: 有 {} 个辅助进程没能在 {:.1} s 内确认结束（pid {}）",
            stopped.unconfirmed.len(),
            HELPERS_WAIT.as_secs_f64(),
            pids.join(", ")
        ));
    }
    if stopped.sweep_incomplete && stopped.unconfirmed.is_empty() {
        said.push("hoh: 没能确认 doctor 的后代都已结束（扫描不完整）".to_owned());
    }
    if let Some(unit) = unit {
        said.push(format!(
            "hoh: unit {unit} 的启动被打断，结果未知；见 systemctl --user status {unit}"
        ));
    }
    said
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
        captured().0
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

    /// The sink of one end of a socket pair, as many times as it is made.
    fn sink(ours: std::os::unix::net::UnixStream) -> MakeSink {
        let file = Arc::new(std::fs::File::from(std::os::fd::OwnedFd::from(ours)));
        Arc::new(move || Box::new(file.try_clone().expect("a duplicate of the socket")))
    }

    /// A state writing to one end of a socket pair; the other end, to read what it said.
    fn captured() -> (State, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let state = assemble(
            Arc::default(),
            Arc::default(),
            sink(ours),
            &crate::stderr::thread("hoh-stderr"),
        )
        .expect("the writer");
        (state, theirs)
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
    fn no_writer_thread_ends_the_registration_with_74_and_writes_nothing() {
        // 设计修正 2, C (CLT-900, CLT-1003; verification 15): no thread for the stderr writer as the state is
        // registered: nothing is written (not even straight to a stderr that may be full) and the client ends with 74;
        // a signal that came once the handlers were in keeps its own code.
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let signal = Arc::new(AtomicUsize::new(0));
        let refused = assemble(signal.clone(), Arc::default(), sink(ours), &no_thread);
        assert_eq!(refused.err(), Some(74));
        signal.store(SIGTERM as usize, Ordering::SeqCst);
        let (ours, _) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        assert_eq!(
            assemble(signal, Arc::default(), sink(ours), &no_thread).err(),
            Some(143)
        );
        assert_eq!(waiting(&theirs), "");
    }

    #[test]
    fn the_exit_says_only_what_the_stop_did_not_finish() {
        // 设计修正 2, D: a stop that finished says nothing; one that did not says each unfinished item, never a
        // positive statement.
        assert!(unfinished(&Stopped::default(), None).is_empty());
        let stopped = Stopped {
            helpers_in_flight: 1,
            unconfirmed: (1..=10).collect(),
            hosts: vec![77],
            hosts_in_flight: 1,
            sweep_incomplete: true,
        };
        assert_eq!(
            unfinished(&stopped, Some("hoh-host@abc.service")),
            [
                "hoh: 一次启动仍未返回，退出后可能出现进程（pid 未知）",
                "hoh: 有 10 个辅助进程没能在 1.5 s 内确认结束（pid 1, 2, 3, 4, 5, 6, 7, 8, …）",
                "hoh: unit hoh-host@abc.service 的启动被打断，结果未知；见 systemctl --user status hoh-host@abc.service",
            ]
        );
        // doctor's sweep incomplete with no pid to name (CLT-1200).
        let incomplete = Stopped {
            sweep_incomplete: true,
            ..Stopped::default()
        };
        assert_eq!(
            unfinished(&incomplete, None),
            ["hoh: 没能确认 doctor 的后代都已结束（扫描不完整）"]
        );
    }

    #[test]
    fn a_unit_start_running_as_the_stop_begins_is_said_and_one_that_ended_is_not() {
        // 设计修正 2, A and D (CLT-753, CLT-1008): the stop sees the unit start that runs as it begins.
        let (state, _) = captured();
        drop(state.unit_starting("hoh-host@a.service"));
        assert_eq!(state.stop().1, None);
        let (state, said) = captured();
        let _running = state.unit_starting("hoh-host@b.service");
        state.signal().store(SIGTERM as usize, Ordering::SeqCst);
        assert_eq!(state.exit(69, "没有提交命令"), 143);
        assert_eq!(
            waiting(&said),
            "hoh: 收到信号，没有提交命令\nhoh: unit hoh-host@b.service 的启动被打断，结果未知；见 systemctl --user \
             status hoh-host@b.service\n"
        );
    }
}
