//! Entering and leaving the full-screen terminal, in one place (ADR-014 decision 2).
//!
//! Every escape sequence the shell sends to enter or leave is a [`Step`]. Rules:
//! - An undo is armed only once what it undoes is entirely out, so a terminal that never entered the alternate screen
//!   is never told to leave it (M1T08R-09).
//! - A sequence that went out is never sent again: a second "leave the alternate screen" puts the saved cursor back
//!   over what was printed since (M1T08R-07).
//! - A sequence cut short is sent again from its start after a CAN (0x18). [`settle`] sends that CAN before any
//!   diagnostic, also after a frame cut short, so text never lands inside a cut sequence (M1T08R-08/10). This relies on
//!   CAN only for what the shell sends: CSI sequences, which a CAN ends (VT100 and its successors, xterm, VTE, tmux),
//!   and the ground state, where it does nothing. Inside an OSC or a DCS terminals differ (tmux applies part of an OSC
//!   title, and its DCS goes on swallowing text), so the shell sends none of those (M1T08R-11).
//! - Raw mode has its own flag and is given back even when the screen fails (M1T08R-01).
//!
//! The sequences go to a duplicate of stdout without a buffer. The TUI draws through [`Output`], whose buffer is
//! dropped unwritten, so nothing of the TUI waits in the process's stdout buffer to be flushed after the restore. The
//! guard exists before the first step, so a partial entry is undone too (M1T08R-05). [`restore`] is called on a
//! normal exit (bounded retries; the result decides the exit code), by the guard on the way out, and by the panic
//! hook (retries, then the message). Raw mode is crossterm's: it keeps the termios from before and writes it back, so
//! `stty -g` is what it was.

use ratatui::crossterm::Command;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cancel: ends an escape sequence under way, ignored otherwise.
const CAN: u8 = 0x18;

/// One escape sequence: still to send, and whether a try of it was cut short (the terminal may be inside it).
#[derive(Debug)]
pub struct Step {
    pending: AtomicBool,
    cut: AtomicBool,
}

impl Step {
    pub const fn new() -> Step {
        Step {
            pending: AtomicBool::new(false),
            cut: AtomicBool::new(false),
        }
    }

    fn arm(&self) {
        self.cut.store(false, Ordering::SeqCst);
        self.pending.store(true, Ordering::SeqCst);
    }

    fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }

    fn is_cut(&self) -> bool {
        self.cut.load(Ordering::SeqCst)
    }
}

/// The sequences of entering and of leaving, in their orders; and the frames, whose last one may have been cut.
#[derive(Debug)]
pub struct Steps {
    enter_screen: Step,
    enter_paste: Step,
    paste_off: Step,
    leave_screen: Step,
    show: Step,
    frame: Step,
}

impl Steps {
    pub const fn new() -> Steps {
        Steps {
            enter_screen: Step::new(),
            enter_paste: Step::new(),
            paste_off: Step::new(),
            leave_screen: Step::new(),
            show: Step::new(),
            frame: Step::new(),
        }
    }

    fn all(&self) -> [&Step; 6] {
        [
            &self.enter_screen,
            &self.enter_paste,
            &self.paste_off,
            &self.leave_screen,
            &self.show,
            &self.frame,
        ]
    }
}

static STEPS: Steps = Steps::new();
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

/// Send `bytes` for `step` if it is pending: from the start, after a CAN if an earlier try was cut short. It is done
/// once all is out; a try that fails after some bytes marks it cut.
fn send(out: &mut impl Write, step: &Step, bytes: &[u8]) -> io::Result<()> {
    if !step.is_pending() {
        return Ok(());
    }
    let mut data = Vec::with_capacity(bytes.len() + 1);
    if step.is_cut() {
        data.push(CAN);
    }
    data.extend_from_slice(bytes);
    let mut sent = 0;
    while let Some(rest) = data.get(sent..).filter(|rest| !rest.is_empty()) {
        let failure = match out.write(rest) {
            Ok(0) => io::ErrorKind::WriteZero.into(),
            Ok(n) => {
                sent += n;
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => e,
        };
        if sent > 0 {
            step.cut.store(true, Ordering::SeqCst);
        }
        return Err(failure);
    }
    out.flush()?;
    step.cut.store(false, Ordering::SeqCst);
    step.pending.store(false, Ordering::SeqCst);
    Ok(())
}

/// Enter the alternate screen, then bracketed paste; each undo is armed once its entry is entirely out (the cursor
/// with the screen: ratatui hides it there).
pub fn open(out: &mut impl Write, steps: &Steps) -> io::Result<()> {
    steps.enter_screen.arm();
    send(out, &steps.enter_screen, &ansi(EnterAlternateScreen))?;
    steps.leave_screen.arm();
    steps.show.arm();
    steps.enter_paste.arm();
    send(out, &steps.enter_paste, &ansi(EnableBracketedPaste))?;
    steps.paste_off.arm();
    Ok(())
}

/// Give up an entry that did not finish (a CAN ends it if it was cut), then bracketed paste off, normal screen and
/// cursor shown, as far as armed; stops at the first error (later steps wait for the earlier ones).
pub fn close(out: &mut impl Write, steps: &Steps) -> io::Result<()> {
    send(out, &steps.enter_screen, b"")?;
    send(out, &steps.enter_paste, b"")?;
    send(out, &steps.paste_off, &ansi(DisableBracketedPaste))?;
    send(out, &steps.leave_screen, &ansi(LeaveAlternateScreen))?;
    send(out, &steps.show, &ansi(Show))
}

/// Before text that is not ours goes to the terminal: a CAN if a sequence was cut short (best effort).
pub fn settle_on(out: &mut impl Write, steps: &Steps) {
    if steps.all().iter().any(|s| s.is_cut()) {
        let _ = out.write_all(&[CAN]).and_then(|()| out.flush());
    }
}

/// [`settle_on`] the terminal, before a diagnostic.
pub fn settle() {
    if let Ok(mut out) = terminal_out() {
        settle_on(&mut out, &STEPS);
    }
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
    let entered = enable_raw_mode().and_then(|()| {
        RAW.store(true, Ordering::SeqCst);
        open(&mut terminal_out()?, &STEPS)
    });
    (guard, entered)
}

/// Leave the alternate screen, show the cursor and give back the terminal's attributes, as far as still to do.
pub fn restore() -> io::Result<()> {
    undo(
        || {
            if STEPS.all().iter().any(|s| s.is_pending()) {
                close(&mut terminal_out()?, &STEPS)
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

/// Restore the terminal (with retries) before the current panic hook prints its message.
pub fn restore_on_panic() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Err(e) = with_retries(3, restore) {
            settle();
            say!("hoh：终端未能恢复：{e}；可运行 stty sane 或 reset");
        }
        previous(info);
    }));
}

/// Where the TUI draws: a duplicate of stdout with a buffer of its own, written out on `flush` (ratatui flushes
/// after each frame). What is still buffered when it is dropped is discarded: it was meant for the alternate
/// screen, and after the restore it would land on the normal one.
#[derive(Debug)]
pub struct Output<'a, W> {
    out: W,
    buffer: Vec<u8>,
    steps: &'a Steps,
}

impl Output<'static, File> {
    pub fn new() -> io::Result<Self> {
        Ok(Output::over(terminal_out()?, &STEPS))
    }
}

impl<'a, W: Write> Output<'a, W> {
    pub fn over(out: W, steps: &'a Steps) -> Self {
        Output {
            out,
            buffer: Vec::new(),
            steps,
        }
    }

    #[cfg(test)]
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.out
    }
}

impl<W: Write> Write for Output<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    /// A frame that fails is not written again; if part of it is out, the frame step is marked cut, so [`settle`]
    /// ends what it left open before a diagnostic. The error follows.
    fn flush(&mut self) -> io::Result<()> {
        let frame = std::mem::take(&mut self.buffer);
        let mut sent = 0;
        while let Some(rest) = frame.get(sent..).filter(|rest| !rest.is_empty()) {
            let failure = match self.out.write(rest) {
                Ok(0) => io::ErrorKind::WriteZero.into(),
                Ok(n) => {
                    sent += n;
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => e,
            };
            if sent > 0 {
                self.steps.frame.cut.store(true, Ordering::SeqCst);
            }
            return Err(failure);
        }
        self.steps.frame.cut.store(false, Ordering::SeqCst);
        self.out.flush()
    }
}
