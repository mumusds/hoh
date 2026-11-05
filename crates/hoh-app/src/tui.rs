//! `hoh` with a terminal on stdin and stdout: the Workspace shell (REQ-001; ADR-014). Signals are registered before
//! the terminal changes, so a SIGTERM, SIGHUP or SIGINT at any point after that restores it; the loop polls input in
//! short slices and looks at the signal flag between them. Leaving only closes this client.

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
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// How long one wait for input lasts before the signal flag is looked at again.
const TICK: Duration = Duration::from_millis(100);

/// What ended the loop.
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
            eprintln!("hoh：无法登记信号处理：{e}");
            return code(ClientEvent::TerminalIo);
        }
    }
    let name = project.file_name().map_or_else(
        || project.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut app = App::new(name, History::load(project));
    terminal::restore_on_panic();
    let result = terminal::enter().and_then(|guard| {
        let end = event_loop(&mut app, &signal);
        drop(guard);
        end
    });
    match result {
        Ok(End::Left(Exit::Detach)) => 0,
        Ok(End::Signal(sig)) => signal_code(sig),
        // A hangup also makes the terminal fail: the signal decides.
        Err(_) if signal.load(Ordering::SeqCst) != 0 => signal_code(signal.load(Ordering::SeqCst)),
        Err(e) => {
            eprintln!("hoh：终端读写失败，TUI 已退出：{e}");
            code(ClientEvent::TerminalIo)
        }
    }
}

fn event_loop(app: &mut App, signal: &AtomicUsize) -> std::io::Result<End> {
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
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
        if event::poll(TICK)? {
            match event::read()? {
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
            }
        }
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
