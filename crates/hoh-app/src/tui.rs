//! `hoh` with a terminal on stdin and stdout: the Workspace shell (REQ-001; ADR-014). Signals are registered before
//! the terminal changes, so a SIGTERM, SIGHUP or SIGINT at any point after that restores it. One thread does it all:
//! the loop waits for input in poll(2), in short slices, looks at the signal flags between them, reads only what poll
//! said is there and decodes it itself (`input`), and ends when the terminal hangs up. Leaving only closes this
//! client.

mod app;
mod editor;
mod history;
mod input;
mod terminal;
#[cfg(test)]
mod tests;

use crate::exit::{ClientEvent, Ending, exit_code};
use app::{App, Exit};
use history::History;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, KeyEventKind};
use rustix::event::{PollFd, PollFlags, Timespec};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use std::io::Stdin;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// How long one wait for input lasts before the signal flags are looked at again (a signal also ends it early).
const TICK: Duration = Duration::from_millis(100);

/// How long the loop waits for the rest of an escape sequence once its start was read: then a lone ESC is the Esc key.
const ESC_WAIT: Duration = Duration::from_millis(30);

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
    let resized = Arc::new(AtomicBool::new(false));
    if let Err(e) = signal_hook::flag::register(SIGWINCH, Arc::clone(&resized)) {
        say!("hoh：无法登记信号处理：{e}");
        return code(ClientEvent::TerminalIo);
    }
    let name = project.file_name().map_or_else(
        || project.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut app = App::new(name, History::load(project));
    terminal::restore_on_panic();
    let (guard, entered) = terminal::enter();
    let end = entered.and_then(|()| event_loop(&mut app, &signal, &resized));
    terminal::debug_restore_gate();
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

fn event_loop(app: &mut App, signal: &AtomicUsize, resized: &AtomicBool) -> std::io::Result<End> {
    let mut terminal = Terminal::new(CrosstermBackend::new(terminal::Output::new()?))?;
    let stdin = std::io::stdin();
    let mut decoder = input::Decoder::default();
    let mut buf = [0u8; 4096];
    let mut dirty = true;
    loop {
        let sig = signal.load(Ordering::SeqCst);
        if sig != 0 {
            return Ok(End::Signal(sig));
        }
        if resized.swap(false, Ordering::SeqCst) {
            dirty = true;
        }
        if dirty {
            terminal.draw(|f| app.draw(f))?;
            dirty = false;
            debug_panic();
        }
        let idle = match wait(&stdin, if decoder.pending() { ESC_WAIT } else { TICK })? {
            Wait::Input => {
                // poll said there is input (or end of file): this read does not block.
                let n = match rustix::io::read(&stdin, &mut buf) {
                    Ok(n) => n,
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(e) => return Err(e.into()),
                };
                if n == 0 {
                    return Err(hangup());
                }
                decoder.feed(&buf[..n]);
                false
            }
            Wait::Idle => true,
            Wait::Interrupted => continue,
        };
        while let Some(event) = decoder.next(idle) {
            match event {
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
                _ => {}
            }
        }
    }
}

/// How a wait for input ended.
enum Wait {
    /// There is something to read.
    Input,
    /// Nothing came within the time.
    Idle,
    /// A signal came: its flag is to be looked at.
    Interrupted,
}

/// Wait up to `timeout` for input on the terminal (stdin, which bare `hoh` requires to be one), without reading
/// anything. A hung-up terminal (its window or connection closed) is an error.
fn wait(stdin: &Stdin, timeout: Duration) -> std::io::Result<Wait> {
    let mut fds = [PollFd::new(stdin, PollFlags::IN)];
    let limit = Timespec {
        tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(timeout.subsec_nanos()),
    };
    match rustix::event::poll(&mut fds, Some(&limit)) {
        Ok(0) => Ok(Wait::Idle),
        Ok(_)
            if fds[0]
                .revents()
                .intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) =>
        {
            Err(hangup())
        }
        Ok(_) => Ok(Wait::Input),
        Err(rustix::io::Errno::INTR) => Ok(Wait::Interrupted),
        Err(e) => Err(e.into()),
    }
}

fn hangup() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "终端已断开")
}

/// Debug builds only: `HOH_DEBUG_TUI_PANIC` panics after a draw, so the PTY test can see the terminal restored
/// before the message (ADR-014 decision 8). Release builds have no such entry.
fn debug_panic() {
    #[cfg(debug_assertions)]
    if std::env::var_os("HOH_DEBUG_TUI_PANIC").is_some() {
        panic!("HOH_DEBUG_TUI_PANIC: a panic asked for by the PTY test");
    }
}
