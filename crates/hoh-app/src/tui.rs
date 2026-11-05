//! `hoh` with a terminal on stdin and stdout: the Workspace shell (REQ-001; ADR-014). Signals are registered before
//! the terminal changes, so a SIGTERM, SIGHUP or SIGINT at any point after that restores it. Terminal events are read
//! on a thread of their own; the loop waits for them in short slices, looks at the signal flag between them, and ends
//! when the terminal hangs up. Leaving only closes this client.

mod app;
mod editor;
mod history;
mod terminal;
#[cfg(test)]
mod tests;

use crate::exit::{ClientEvent, Ending, exit_code};
use app::{App, Exit};
use history::History;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use rustix::event::{PollFd, PollFlags, Timespec};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long one wait for input lasts before the signal flag is looked at again.
const TICK: Duration = Duration::from_millis(100);

/// What ended the loop.
#[derive(Debug, PartialEq, Eq)]
enum End {
    Left(Exit),
    Signal(usize),
}

fn code(event: ClientEvent) -> u8 {
    exit_code(Ending::Client(event)).unwrap_or(1)
}

fn signal_code(signal: usize) -> u8 {
    match i32::try_from(signal) {
        Ok(SIGTERM) => code(ClientEvent::Sigterm),
        Ok(SIGHUP) => code(ClientEvent::Sighup),
        _ => code(ClientEvent::Sigint),
    }
}

/// Run the shell for the project at `project`; the exit code.
pub fn run(project: &Path) -> u8 {
    let signal = Arc::new(AtomicUsize::new(0));
    for sig in [SIGTERM, SIGHUP, SIGINT] {
        let value = usize::try_from(sig).unwrap_or(usize::MAX);
        if let Err(e) = signal_hook::flag::register_usize(sig, Arc::clone(&signal), value) {
            say!("hoh：无法登记信号处理：{e}");
            return code(ClientEvent::TerminalIo);
        }
    }
    let name = project.file_name().map_or_else(
        || project.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut app = App::new(name, History::load(project));
    terminal::restore_on_panic();
    let (guard, entered) = terminal::enter();
    let end = entered.and_then(|()| event_loop(&mut app, &signal));
    debug_restore_delay();
    let restored = guard.finish();
    // Loaded after the restore: a signal that came while restoring decides too (M1T08R-06).
    let (code, message) = outcome(end, restored, signal.load(Ordering::SeqCst));
    if let Some(message) = message {
        terminal::settle();
        say!("hoh：{message}");
    }
    code
}

/// The exit code of a way out and what to say on stderr (ADR-014 decision 3): a received signal decides the code on
/// every path (a hangup also makes the terminal fail; one may come while a detach is restored); otherwise a failing
/// terminal or a restore that did not complete is 74, never 0.
fn outcome(
    end: std::io::Result<End>,
    restored: std::io::Result<()>,
    signal: usize,
) -> (u8, Option<String>) {
    let not_restored = restored
        .err()
        .map(|e| format!("终端未能恢复：{e}；可运行 stty sane 或 reset"));
    let signal = match end {
        Ok(End::Signal(sig)) => sig,
        _ => signal,
    };
    if signal != 0 {
        return (signal_code(signal), not_restored);
    }
    match end {
        Ok(End::Signal(_)) => (signal_code(signal), not_restored),
        Ok(End::Left(Exit::Detach)) => match not_restored {
            None => (0, None),
            Some(m) => (code(ClientEvent::TerminalIo), Some(m)),
        },
        Err(e) => (
            code(ClientEvent::TerminalIo),
            Some(match not_restored {
                None => format!("终端读写失败，TUI 已退出：{e}"),
                Some(m) => format!("终端读写失败，TUI 已退出：{e}；{m}"),
            }),
        ),
    }
}

fn event_loop(app: &mut App, signal: &AtomicUsize) -> std::io::Result<End> {
    let mut terminal = Terminal::new(CrosstermBackend::new(terminal::Output::new()?))?;
    // Dropped when the loop ends, on every path: reading stops before the terminal is handed back.
    let input = Input::start()?;
    let mut dirty = true;
    loop {
        let sig = signal.load(Ordering::SeqCst);
        if sig != 0 {
            return Ok(End::Signal(sig));
        }
        if dirty {
            terminal.draw(|f| app.draw(f))?;
            dirty = false;
            debug_panic();
        }
        match input.events.recv_timeout(TICK) {
            Ok(event) => match event? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(exit) = app.key(key) {
                        return Ok(End::Left(exit));
                    }
                    dirty = true;
                }
                Event::Paste(text) => {
                    app.paste(&text);
                    dirty = true;
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            },
            // A reader caught inside crossterm by a hang-up says nothing more; the loop sees the hang-up itself.
            Err(RecvTimeoutError::Timeout) if hung_up() => return Err(hangup()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(std::io::Error::other("终端输入的读取已停止"));
            }
        }
    }
}

/// How long the reader waits on the terminal before it looks at its stop flag, and for a resize, again.
const READ_TICK: Duration = Duration::from_millis(20);

/// How long an ending loop waits for the reader to stop. Only a reader caught inside crossterm by a hang-up at that
/// very moment takes longer, and a hung-up terminal has no input left to keep.
const STOP_WAIT: Duration = Duration::from_millis(300);

/// Terminal input, read on a thread of its own (ADR-014 decision 1).
///
/// crossterm 0.29 never returns from a read once the terminal is at end of file, as a hung-up one is: it retries the
/// zero-byte read forever, whatever the timeout. So the loop never waits inside crossterm: it waits on this channel
/// and stays free to see a signal or a hang-up (M1QA-01). The reader in turn waits in poll(2), not in crossterm, and
/// asks crossterm only for what is there to take; so it can be stopped, and it is, when this is dropped, before the
/// terminal is handed back: what is typed from then on stays for the shell (M1T08R-12).
struct Input {
    events: Receiver<std::io::Result<Event>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Input {
    fn start() -> std::io::Result<Input> {
        let (tx, events) = sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let reader = std::thread::Builder::new()
            .name("hoh-tui-input".to_owned())
            .spawn(move || read_input(&tx, &stopped))?;
        Ok(Input {
            events,
            stop,
            reader: Some(reader),
        })
    }
}

impl Drop for Input {
    /// Stop the reader and wait for it, up to [`STOP_WAIT`]. Events still in the channel are dropped, so a reader
    /// waiting to send can see the flag.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let end = Instant::now() + STOP_WAIT;
            while !reader.is_finished() && Instant::now() < end {
                while self.events.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(2));
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}

/// The reader: the terminal's events, until `stop` is set or the terminal fails or hangs up.
fn read_input(tx: &SyncSender<std::io::Result<Event>>, stop: &AtomicBool) {
    let stdin = std::io::stdin();
    let tick = Timespec {
        tv_sec: 0,
        tv_nsec: i64::from(READ_TICK.subsec_nanos()),
    };
    while !stop.load(Ordering::SeqCst) {
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        let ready = match rustix::event::poll(&mut fds, Some(&tick)) {
            Ok(n) => n > 0,
            Err(rustix::io::Errno::INTR) => false,
            Err(e) => {
                let _ = tx.send(Err(e.into()));
                return;
            }
        };
        if ready && gone(fds[0].revents()) {
            let _ = tx.send(Err(hangup()));
            return;
        }
        // What crossterm has now, without waiting: the input poll saw, or a resize it noted (crossterm hears of
        // SIGWINCH itself, not through this poll). The flag is looked at before each take, so nothing is read once
        // the loop has ended.
        let mut taken = false;
        while !stop.load(Ordering::SeqCst) {
            match event::poll(Duration::ZERO) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
            let event = event::read();
            let failed = event.is_err();
            if tx.send(event).is_err() || failed {
                return;
            }
            taken = true;
        }
        // Input poll saw that crossterm did not take (it waits for its own readiness edges): wait a tick rather than
        // spin on the same readiness.
        if ready && !taken {
            std::thread::sleep(READ_TICK);
        }
    }
}

/// Whether poll(2) flags say the terminal is gone: its window or connection was closed.
fn gone(flags: PollFlags) -> bool {
    flags.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL)
}

fn hangup() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "终端已断开")
}

/// Whether the terminal has hung up. crossterm reads stdin, which is the terminal here (bare `hoh` needs one);
/// poll(2) reports the hang-up on it without reading.
fn hung_up() -> bool {
    let stdin = std::io::stdin();
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    matches!(rustix::event::poll(&mut fds, Some(&now)), Ok(1..)) && gone(fds[0].revents())
}

/// Debug builds only: `HOH_DEBUG_TUI_RESTORE_DELAY_MS` holds the restore back that long once the loop has ended,
/// saying so on stderr, so the PTY test can type into that window and find its input left for the shell
/// (M1T08R-12). Release builds have no such entry.
fn debug_restore_delay() {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOH_DEBUG_TUI_RESTORE_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        say!("HOH_DEBUG_TUI_RESTORE_DELAY");
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// Debug builds only: `HOH_DEBUG_TUI_PANIC` panics after a draw, so the PTY test can see the terminal restored
/// before the message (ADR-014 decision 8). Release builds have no such entry.
fn debug_panic() {
    #[cfg(debug_assertions)]
    if std::env::var_os("HOH_DEBUG_TUI_PANIC").is_some() {
        panic!("HOH_DEBUG_TUI_PANIC: a panic asked for by the PTY test");
    }
}
