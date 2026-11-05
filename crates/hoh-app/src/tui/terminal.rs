//! Entering and leaving the full-screen terminal, in one place (ADR-014 decision 2). Each step to undo has its own
//! flag, set before the step is taken and cleared only once it is undone, so [`restore`] undoes what is left (the
//! screen first, raw mode last), a failed step stays marked and the next call tries it again, and nothing is undone
//! twice (M1T08R-01). It is called on a normal exit (whose result decides the exit code), by the guard on the way
//! out, and by the panic hook before the panic message. Raw mode is crossterm's: it keeps the termios from before and
//! writes it back, so `stty -g` is what it was.

use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

/// Raw mode is on.
static RAW: AtomicBool = AtomicBool::new(false);
/// The alternate screen and bracketed paste are (or may be) on, the cursor may be hidden.
static SCREEN: AtomicBool = AtomicBool::new(false);

/// Restores the terminal when dropped (a retry if [`Guard::finish`] was not reached or failed).
#[derive(Debug)]
pub struct Guard(());

impl Guard {
    /// Restore now; the result decides the exit code.
    pub fn finish(self) -> io::Result<()> {
        restore()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = restore();
    }
}

/// Raw mode, the alternate screen and bracketed paste; restored (as far as it got) if a step fails.
pub fn enter() -> io::Result<Guard> {
    enable_raw_mode()?;
    RAW.store(true, Ordering::SeqCst);
    SCREEN.store(true, Ordering::SeqCst);
    if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste) {
        let _ = restore();
        return Err(e);
    }
    Ok(Guard(()))
}

/// Leave the alternate screen, show the cursor and give back the terminal's attributes, as far as still marked.
pub fn restore() -> io::Result<()> {
    undo(
        &SCREEN,
        || {
            execute!(
                io::stdout(),
                DisableBracketedPaste,
                LeaveAlternateScreen,
                Show
            )
        },
        &RAW,
        disable_raw_mode,
    )
}

/// Undo the marked steps, `screen` before `raw`; a step that fails stays marked and the first error is returned.
pub fn undo(
    screen: &AtomicBool,
    leave: impl FnOnce() -> io::Result<()>,
    raw: &AtomicBool,
    cooked: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let mut first = None;
    if screen.load(Ordering::SeqCst) {
        match leave() {
            Ok(()) => screen.store(false, Ordering::SeqCst),
            Err(e) => first = Some(e),
        }
    }
    if raw.load(Ordering::SeqCst) {
        match cooked() {
            Ok(()) => raw.store(false, Ordering::SeqCst),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    first.map_or(Ok(()), Err)
}

/// Restore the terminal before the current panic hook prints its message.
pub fn restore_on_panic() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Err(e) = restore() {
            eprintln!("hoh：终端未能恢复：{e}；可运行 stty sane 或 reset");
        }
        previous(info);
    }));
}
