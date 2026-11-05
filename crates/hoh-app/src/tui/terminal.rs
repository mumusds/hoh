//! Entering and leaving the full-screen terminal, in one place (ADR-014 decision 2). [`restore`] is idempotent and
//! is called on a normal exit, on a signal (by the caller) and by the panic hook, before the panic message is printed.
//! Raw mode is crossterm's: it keeps the termios from before and writes it back, so `stty -g` is what it was.

use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::sync::atomic::{AtomicBool, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Restores the terminal when dropped.
#[derive(Debug)]
pub struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Raw mode, the alternate screen and bracketed paste; restored (as far as it got) if a step fails.
pub fn enter() -> std::io::Result<Guard> {
    enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    if let Err(e) = execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste
    ) {
        restore();
        return Err(e);
    }
    Ok(Guard(()))
}

/// Leave the alternate screen, show the cursor and give back the terminal's attributes; does nothing the second time.
pub fn restore() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            Show
        );
        let _ = disable_raw_mode();
    }
}

/// Restore the terminal before the current panic hook prints its message.
pub fn restore_on_panic() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}
