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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::Duration;

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
    let events = events()?;
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
        match events.recv_timeout(TICK) {
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
            Err(RecvTimeoutError::Timeout) if hung_up() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "终端已断开",
                ));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(std::io::Error::other("终端输入的读取已停止"));
            }
        }
    }
}

/// Terminal events, read on a thread of their own. crossterm 0.29 never returns from a read once the terminal is at
/// end of file, as a hung-up one is: it retries the zero-byte read forever, whatever the timeout. So the loop waits
/// on this channel, never inside crossterm, and stays free to see a signal or the hang-up (M1QA-01). The thread ends
/// with the process.
fn events() -> std::io::Result<Receiver<std::io::Result<Event>>> {
    let (tx, rx) = sync_channel(64);
    std::thread::Builder::new()
        .name("hoh-tui-input".to_owned())
        .spawn(move || {
            loop {
                let event = event::read();
                let failed = event.is_err();
                if tx.send(event).is_err() || failed {
                    break;
                }
            }
        })?;
    Ok(rx)
}

/// Whether the terminal has hung up: its window or connection was closed. crossterm reads stdin, which is the
/// terminal here (bare `hoh` needs one); poll(2) reports the hang-up on it without reading.
fn hung_up() -> bool {
    let stdin = std::io::stdin();
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    matches!(rustix::event::poll(&mut fds, Some(&now)), Ok(1..))
        && fds[0]
            .revents()
            .intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL)
}

/// Debug builds only: `HOH_DEBUG_TUI_PANIC` panics after a draw, so the PTY test can see the terminal restored
/// before the message (ADR-014 decision 8). Release builds have no such entry.
fn debug_panic() {
    #[cfg(debug_assertions)]
    if std::env::var_os("HOH_DEBUG_TUI_PANIC").is_some() {
        panic!("HOH_DEBUG_TUI_PANIC: a panic asked for by the PTY test");
    }
}
