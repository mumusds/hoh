//! Entering and leaving the full-screen terminal, in one place (ADR-014 decision 2).
//!
//! Every step to undo is tracked on its own and cleared only once it is undone. The three escape sequences of the
//! screen (bracketed paste off, normal screen, cursor shown) also record how much of each is out. So [`restore`]
//! undoes what is left: a sequence that went out is never sent again (a second "leave the alternate screen" puts the
//! saved cursor back over what was printed since), one cut short goes on from where it stopped, a failed step is
//! retried by the next call, and raw mode is given back even when the screen fails (M1T08R-01/07).
//!
//! The sequences go to a duplicate of stdout without a buffer, and the TUI draws through [`Output`], whose buffer is
//! dropped unwritten. Nothing of the TUI's waits in the process's stdout buffer to be flushed after the restore.
//!
//! [`restore`] is called on a normal exit (bounded retries; the result decides the exit code), by the guard on the
//! way out, and by the panic hook before the panic message. Raw mode is crossterm's: it keeps the termios from
//! before and writes it back, so `stty -g` is what it was.

use ratatui::crossterm::Command;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// One escape sequence of the restore: still to send, and how many of its bytes are out.
#[derive(Debug)]
pub struct Step {
    pending: AtomicBool,
    sent: AtomicUsize,
}

impl Step {
    pub const fn new() -> Step {
        Step {
            pending: AtomicBool::new(false),
            sent: AtomicUsize::new(0),
        }
    }

    /// To be sent, from the start.
    pub fn arm(&self) {
        self.sent.store(0, Ordering::SeqCst);
        self.pending.store(true, Ordering::SeqCst);
    }

    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }
}

/// Bracketed paste off, normal screen, cursor shown: in this order.
static SCREEN: [Step; 3] = [Step::new(), Step::new(), Step::new()];
/// Raw mode is on.
static RAW: AtomicBool = AtomicBool::new(false);

fn ansi(command: impl Command) -> Vec<u8> {
    let mut text = String::new();
    let _ = command.write_ansi(&mut text);
    text.into_bytes()
}

/// A duplicate of stdout without a buffer.
fn terminal_out() -> io::Result<File> {
    Ok(File::from(io::stdout().as_fd().try_clone_to_owned()?))
}

/// Restores the terminal when dropped (a retry if [`Guard::finish`] was not reached or failed). It exists from the
/// first step of [`enter`], so a partial entry is undone too (M1T08R-05).
#[derive(Debug)]
pub struct Guard(());

impl Guard {
    /// Restore now, with up to three tries; the result decides the exit code.
    pub fn finish(self) -> io::Result<()> {
        with_retries(3, restore)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = restore();
    }
}

/// Raw mode, the alternate screen and bracketed paste. The guard comes back also when a step failed: it undoes what
/// was done.
pub fn enter() -> (Guard, io::Result<()>) {
    let guard = Guard(());
    let entered = (|| {
        enable_raw_mode()?;
        RAW.store(true, Ordering::SeqCst);
        for step in &SCREEN {
            step.arm();
        }
        execute!(terminal_out()?, EnterAlternateScreen, EnableBracketedPaste)
    })();
    (guard, entered)
}

/// Leave the alternate screen, show the cursor and give back the terminal's attributes, as far as still to do.
pub fn restore() -> io::Result<()> {
    let sequences = [
        ansi(DisableBracketedPaste),
        ansi(LeaveAlternateScreen),
        ansi(Show),
    ];
    let sequences = sequences.each_ref().map(Vec::as_slice);
    undo(
        || {
            if SCREEN.iter().any(Step::is_pending) {
                send(&mut terminal_out()?, &SCREEN, &sequences)
            } else {
                Ok(())
            }
        },
        &RAW,
        disable_raw_mode,
    )
}

/// Undo the screen, then raw mode (tried also when the screen failed; it stays marked if it fails); the first error.
pub fn undo(
    screen: impl FnOnce() -> io::Result<()>,
    raw: &AtomicBool,
    cooked: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let screen = screen();
    let raw = if raw.load(Ordering::SeqCst) {
        cooked().map(|()| raw.store(false, Ordering::SeqCst))
    } else {
        Ok(())
    };
    screen.and(raw)
}

/// Write what is left of each pending step, in order; a step is done once all its bytes are out. Stops at the
/// first error, so later steps wait for the earlier ones.
pub fn send(out: &mut impl Write, steps: &[Step], sequences: &[&[u8]]) -> io::Result<()> {
    for (step, bytes) in steps.iter().zip(sequences) {
        if !step.is_pending() {
            continue;
        }
        let mut sent = step.sent.load(Ordering::SeqCst);
        while let Some(rest) = bytes.get(sent..).filter(|rest| !rest.is_empty()) {
            match out.write(rest) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    sent += n;
                    step.sent.store(sent, Ordering::SeqCst);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        out.flush()?;
        step.pending.store(false, Ordering::SeqCst);
    }
    Ok(())
}

/// `attempt` until it succeeds, at most `tries` times; the last error.
pub fn with_retries(tries: usize, mut attempt: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut last = Ok(());
    for _ in 0..tries.max(1) {
        last = attempt();
        if last.is_ok() {
            break;
        }
    }
    last
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

/// Where the TUI draws: a duplicate of stdout with a buffer of its own, written out on `flush` (ratatui flushes
/// after each frame). What is still buffered when it is dropped is discarded: it was meant for the alternate
/// screen, and after the restore it would land on the normal one.
#[derive(Debug)]
pub struct Output {
    out: File,
    buffer: Vec<u8>,
}

impl Output {
    pub fn new() -> io::Result<Output> {
        Ok(Output {
            out: terminal_out()?,
            buffer: Vec::new(),
        })
    }
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    /// A frame that fails is not written again (part of it may be out); the next frame and the error follow.
    fn flush(&mut self) -> io::Result<()> {
        let frame = std::mem::take(&mut self.buffer);
        self.out.write_all(&frame)
    }
}
