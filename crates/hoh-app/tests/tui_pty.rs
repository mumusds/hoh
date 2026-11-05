//! The TUI shell in a real pseudo-terminal (check tui-pty; ADR-014). Each test records `stty -g` before `hoh` starts
//! and compares it after `hoh` has exited:
//! - the Workspace opens offline, and opening it without pressing Enter writes nothing;
//! - an input submitted with Enter is in the project's history and Ctrl+R finds it in the next session, without
//!   sending anything;
//! - Ctrl+C then "脱离" exits 0;
//! - SIGTERM exits 143, SIGHUP 129 and SIGINT 130; a panic exits 101 with its message after the terminal is restored.
//! - a terminal that is closed under it (its window, an SSH connection) ends it: 129, or 74 when it was not the
//!   controlling terminal.
//!
//! In every case the alternate screen is entered and left, and the terminal attributes are those of before. The
//! names carry no TEST id: TEST-001-01/02 (the host parts) are M2-T13's.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;

use pty::{
    Hoh, Pty, R, exit_within, find, in_write, signal, spawn_in, spawn_without_ctty_in, tree,
};
use rustix::process::Signal;
use std::path::Path;
use std::time::Duration;

const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const WAIT: Duration = Duration::from_secs(10);

/// Fields drop in order: `hoh` is stopped before its PTY is closed.
struct Session {
    child: Hoh,
    pty: Pty,
    baseline: String,
}

/// `hoh` started in a 100x30 PTY in `project`, once its Workspace is drawn (project name, mode, offline host).
fn open(project: &Path, home: &Path, env: &[(&str, &str)]) -> R<Session> {
    let pty = Pty::new(30, 100)?;
    let baseline = pty.stty()?;
    // From here on a failure drops `child`, which stops `hoh`.
    let child = spawn_in(&pty, project, home, &[], env)?;
    let name = project
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("project name")?;
    pty.wait_for(name, 0, WAIT)?;
    pty.wait_for("AUTO", 0, WAIT)?;
    pty.wait_for("host 离线", 0, WAIT)?;
    Ok(Session {
        child,
        pty,
        baseline,
    })
}

impl Session {
    /// The exit code once `hoh` ends; also checks the terminal was restored (attributes, alternate screen, cursor).
    fn ended(mut self) -> R<Option<i32>> {
        let status = exit_within(&mut self.child, WAIT)?;
        let out = self.pty.output();
        let entered = find(&out, ENTER_ALT, 0).ok_or("the alternate screen was never entered")?;
        let left = find(&out, LEAVE_ALT, entered).ok_or("the alternate screen was not left")?;
        // Left exactly once: a second leave would restore the saved cursor over what was printed after the first
        // (the panic message, say).
        assert_eq!(
            find(&out, LEAVE_ALT, left + 1),
            None,
            "the alternate screen was left twice"
        );
        assert!(
            find(&out, SHOW_CURSOR, left).is_some(),
            "the cursor was not shown again after leaving the alternate screen"
        );
        assert_eq!(self.pty.stty()?, self.baseline, "terminal attributes");
        Ok(status.code())
    }

    /// Ctrl+C on an empty editor, then "脱离".
    fn detach(mut self) -> R<Option<i32>> {
        let from = self.pty.output().len();
        self.pty.send(b"\x03")?;
        self.pty.wait_for("脱离", from, WAIT)?;
        self.pty.send(b"\r")?;
        self.ended()
    }
}

#[test]
fn the_workspace_opens_offline_and_writes_nothing_until_enter() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let before = tree(project.path())?;
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.send("草稿 draft".as_bytes())?;
    s.pty.wait_for("草稿draft", 0, WAIT)?;
    // Ctrl+C clears the editor first; the second opens the choice.
    let from = s.pty.output().len();
    s.pty.send(b"\x03")?;
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pty::squash(&s.pty.output()[from..]).contains("脱离"),
        "the first Ctrl+C must only clear a non-empty editor"
    );
    assert_eq!(s.detach()?, Some(0));
    assert!(tree(project.path())? == before, "the project changed");
    Ok(())
}

#[test]
fn submitted_input_is_kept_and_found_again_in_the_next_session() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let history = project.path().join(".hoh/input-history.jsonl");
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.send(b"hello\r")?;
    s.pty.wait_for("未发送", 0, WAIT)?;
    assert_eq!(std::fs::read_to_string(&history)?, "{\"text\":\"hello\"}\n");
    assert_eq!(s.detach()?, Some(0));

    let mut s = open(project.path(), home.path(), &[])?;
    let from = s.pty.output().len();
    s.pty.send(b"\x12")?;
    s.pty.send(b"hel")?;
    s.pty.wait_for("hello", from, WAIT)?;
    // Enter ends the search with the match in the editor; it sends nothing.
    s.pty.send(b"\r")?;
    std::thread::sleep(Duration::from_millis(300));
    assert!(!pty::squash(&s.pty.output()[from..]).contains("未发送"));
    assert_eq!(std::fs::read_to_string(&history)?, "{\"text\":\"hello\"}\n");
    s.pty.send(b"\x03")?; // clears the editor
    assert_eq!(s.detach()?, Some(0));
    Ok(())
}

#[test]
fn sigterm_restores_the_terminal_and_exits_143() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.send(b"draft")?;
    s.pty.wait_for("draft", 0, WAIT)?;
    signal(&s.child, Signal::TERM)?;
    assert_eq!(s.ended()?, Some(143));
    assert!(
        !project.path().join(".hoh").exists(),
        "a draft is not history"
    );
    Ok(())
}

#[test]
fn sighup_restores_the_terminal_and_exits_129() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let s = open(project.path(), home.path(), &[])?;
    signal(&s.child, Signal::HUP)?;
    assert_eq!(s.ended()?, Some(129));
    Ok(())
}

/// A resized window is redrawn at once, without a key: the input reader also hears of a resize while no input
/// comes (M1T08R-12 moved its wait from crossterm to poll(2)).
#[test]
fn a_resize_redraws_the_workspace_without_a_key() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let s = open(project.path(), home.path(), &[])?;
    std::thread::sleep(Duration::from_millis(300));
    let from = s.pty.output().len();
    s.pty.resize(40, 120)?;
    s.pty.wait_for("host 离线", from, Duration::from_secs(2))?;
    assert_eq!(s.detach()?, Some(0));
    Ok(())
}

/// Text typed into the restore window, held open by the debug-build gate (`HOH_DEBUG_TUI_RESTORE_GATE`): the test
/// sees hoh waiting at the gate, types, and only then opens it, so the input is known to come before the restore
/// (M1T08R-15). The exit code and what was left unread on the terminal.
fn type_into_the_restore_window(
    mut s: Session,
    gate: &Path,
    text: &[u8],
) -> R<(Option<i32>, Vec<u8>)> {
    s.pty.wait_for("HOH_DEBUG_TUI_RESTORE_GATE", 0, WAIT)?;
    s.pty.send(text)?;
    // Still at the gate once the input is typed: a gate left shut too long aborts hoh (M1T08R-22).
    assert!(
        s.child.0.try_wait()?.is_none(),
        "hoh left the gate before the input was typed"
    );
    std::fs::write(gate, b"")?;
    let status = exit_within(&mut s.child, WAIT)?;
    assert_eq!(s.pty.stty()?, s.baseline, "terminal attributes");
    Ok((status.code(), s.pty.unread_input(Duration::from_secs(2))?))
}

/// The gate the restore-window tests rely on cannot be outwaited: left shut, it aborts hoh after 10 s, so input typed
/// too late fails those tests instead of passing on a terminal hoh no longer reads (M1T08R-22).
#[test]
fn a_restore_gate_left_shut_aborts_hoh() -> R {
    use std::os::unix::process::ExitStatusExt;
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let gate = home.path().join("gate");
    let gate_env = gate.to_str().ok_or("gate path")?;
    let mut s = open(
        project.path(),
        home.path(),
        &[("HOH_DEBUG_TUI_RESTORE_GATE", gate_env)],
    )?;
    let from = s.pty.output().len();
    s.pty.send(b"\x03")?;
    s.pty.wait_for("脱离", from, WAIT)?;
    s.pty.send(b"\r")?;
    s.pty.wait_for("HOH_DEBUG_TUI_RESTORE_GATE", 0, WAIT)?;
    let status = exit_within(&mut s.child, Duration::from_secs(20))?;
    assert_eq!(status.signal(), Some(6), "{status:?}");
    s.pty.wait_for("not opened within 10 s", 0, WAIT)?;
    Ok(())
}

/// What is typed once the user has chosen to leave belongs to the shell: hoh reads nothing once its loop has ended,
/// so input typed while it restores the terminal stays unread (M1T08R-12).
#[test]
fn input_typed_while_the_tui_restores_the_terminal_is_left_for_the_shell() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let gate = home.path().join("gate");
    let gate_env = gate.to_str().ok_or("gate path")?;
    let mut s = open(
        project.path(),
        home.path(),
        &[("HOH_DEBUG_TUI_RESTORE_GATE", gate_env)],
    )?;
    let from = s.pty.output().len();
    s.pty.send(b"\x03")?;
    s.pty.wait_for("脱离", from, WAIT)?;
    s.pty.send(b"\r")?;
    let (code, unread) = type_into_the_restore_window(s, &gate, b"echo NEXT\n")?;
    assert_eq!(code, Some(0));
    assert_eq!(
        String::from_utf8_lossy(&unread),
        "echo NEXT\n",
        "hoh read the input typed after leaving"
    );
    Ok(())
}

/// An unfinished escape sequence (`ESC [` and no more) leaves nothing waiting on the terminal: a SIGTERM still ends
/// hoh at once, and what is typed while it restores the terminal stays for the shell (M1T08R-13).
#[test]
fn an_unfinished_escape_sequence_holds_up_neither_the_exit_nor_the_next_input() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let gate = home.path().join("gate");
    let gate_env = gate.to_str().ok_or("gate path")?;
    let mut s = open(
        project.path(),
        home.path(),
        &[("HOH_DEBUG_TUI_RESTORE_GATE", gate_env)],
    )?;
    s.pty.send(b"\x1b[")?;
    std::thread::sleep(Duration::from_millis(200));
    signal(&s.child, Signal::TERM)?;
    let (code, unread) = type_into_the_restore_window(s, &gate, b"echo NEXT\n")?;
    assert_eq!(code, Some(143));
    assert_eq!(
        String::from_utf8_lossy(&unread),
        "echo NEXT\n",
        "hoh read the input typed after the signal"
    );
    Ok(())
}

/// A signal that comes while hoh is held in a write (its terminal's output stopped, as Ctrl+S or a stalled link
/// does) ends it before it reads anything typed after the signal (M1T08R-19).
#[test]
fn a_signal_while_output_is_held_leaves_later_input_for_the_shell() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.hold_output(true)?;
    // A key to draw, and the draw is held.
    s.pty.send(b"a")?;
    in_write(&s.child, WAIT)?;
    signal(&s.child, Signal::TERM)?;
    s.pty.send(b"echo NEXT\n")?;
    s.pty.hold_output(false)?;
    let status = exit_within(&mut s.child, WAIT)?;
    assert_eq!(status.code(), Some(143));
    assert_eq!(s.pty.stty()?, s.baseline, "terminal attributes");
    assert_eq!(
        String::from_utf8_lossy(&s.pty.unread_input(Duration::from_secs(2))?),
        "echo NEXT\n",
        "hoh read the input typed after the signal"
    );
    Ok(())
}

/// A slow link may split a sequence: what follows `ESC [` within half a second still completes it, so Up recalls
/// the last input instead of typing `A`; a paste whose start marker comes in two parts is still one paste, not lines
/// submitted one by one (M1T08R-20).
#[test]
fn a_sequence_split_by_a_slow_link_is_still_one_key() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let history = project.path().join(".hoh/input-history.jsonl");
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.send(b"one\r")?;
    s.pty.wait_for("未发送", 0, WAIT)?;
    for (start, rest) in [
        (&b"\x1b["[..], &b"A\r"[..]),
        (b"\x1b[20", b"0~line1\rline2\x1b[201~\r"),
    ] {
        s.pty.send(start)?;
        std::thread::sleep(Duration::from_millis(150));
        s.pty.send(rest)?;
    }
    assert_eq!(s.detach()?, Some(0));
    assert_eq!(
        std::fs::read_to_string(&history)?,
        "{\"text\":\"one\"}\n{\"text\":\"one\"}\n{\"text\":\"line1 line2\"}\n"
    );
    Ok(())
}

/// A panic restores the terminal from its hook, before anything is unwound; by then nothing reads the terminal any
/// more, so what is typed while the hook restores it stays for the shell (M1T08R-14).
#[test]
fn input_typed_while_a_panic_restores_the_terminal_is_left_for_the_shell() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let gate = home.path().join("gate");
    let gate_env = gate.to_str().ok_or("gate path")?;
    let s = open(
        project.path(),
        home.path(),
        &[
            ("HOH_DEBUG_TUI_PANIC", "1"),
            ("HOH_DEBUG_TUI_RESTORE_GATE", gate_env),
        ],
    )?;
    let (code, unread) = type_into_the_restore_window(s, &gate, b"echo NEXT\n")?;
    assert_eq!(code, Some(101));
    assert_eq!(
        String::from_utf8_lossy(&unread),
        "echo NEXT\n",
        "hoh read the input typed during the panic"
    );
    Ok(())
}

/// A lone ESC is the Esc key once no more input follows it: Esc ends the reverse search (ADR-014 decision 1).
#[test]
fn a_lone_esc_is_the_esc_key() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let mut s = open(project.path(), home.path(), &[])?;
    let from = s.pty.output().len();
    s.pty.send(b"\x12")?;
    let from = s.pty.wait_for("反向搜索", from, WAIT)?;
    s.pty.send(b"\x1b")?;
    s.pty.wait_for("Enter 记录", from, WAIT)?;
    assert_eq!(s.detach()?, Some(0));
    Ok(())
}

/// A burst of keys larger than one read (4 KiB) is taken in full: 5000 characters and Enter make one 5000-character
/// line in the history, with nothing left behind waiting for another key (M1T08R-25).
#[test]
fn a_burst_of_keys_is_taken_in_full() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let history = project.path().join(".hoh/input-history.jsonl");
    let mut s = open(project.path(), home.path(), &[])?;
    let line = "x".repeat(5000);
    s.pty.send(format!("{line}\r").as_bytes())?;
    // The message of so long a line fills the screen, so the history file says when it was submitted.
    let want = format!("{{\"text\":\"{line}\"}}\n");
    let end = std::time::Instant::now() + WAIT;
    while std::fs::read_to_string(&history).unwrap_or_default() != want
        && std::time::Instant::now() < end
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read_to_string(&history)?, want);
    assert_eq!(s.detach()?, Some(0));
    Ok(())
}

/// Closing the terminal hangs it up, and hoh's input then reads end of file. As its controlling terminal the kernel
/// also sends SIGHUP: 129. As only its stdio there is no signal: the terminal failed, 74. Either way hoh ends within
/// a few seconds instead of spinning on the hung-up input (M1QA-01). The terminal is gone, so there is nothing to
/// compare `stty -g` with.
#[test]
fn a_terminal_closed_under_the_tui_ends_it() -> R {
    for ctty in [true, false] {
        let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
        let mut pty = Pty::new(30, 100)?;
        // Declared after the PTY, so dropped before it: a failure stops `hoh` first.
        let mut child = if ctty {
            spawn_in(&pty, project.path(), home.path(), &[], &[])?
        } else {
            spawn_without_ctty_in(&pty, project.path(), home.path())?
        };
        pty.wait_for("host 离线", 0, WAIT)?;
        pty.hang_up()?;
        let status = exit_within(&mut child, Duration::from_secs(3))
            .map_err(|e| format!("ctty={ctty}: {e}"))?;
        assert_eq!(
            status.code(),
            Some(if ctty { 129 } else { 74 }),
            "ctty={ctty}"
        );
    }
    Ok(())
}

#[test]
fn a_panic_restores_the_terminal_before_its_message() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let s = open(project.path(), home.path(), &[("HOH_DEBUG_TUI_PANIC", "1")])?;
    s.pty.wait_for("HOH_DEBUG_TUI_PANIC", 0, WAIT)?;
    let out = s.pty.output();
    let left = find(&out, LEAVE_ALT, 0).ok_or("the alternate screen was not left")?;
    let message = find(&out, b"HOH_DEBUG_TUI_PANIC", 0).ok_or("no panic message")?;
    assert!(
        left < message,
        "the message must come after the terminal is restored"
    );
    assert_eq!(s.ended()?, Some(101));
    Ok(())
}

#[test]
fn sigint_from_outside_restores_the_terminal_and_exits_130() -> R {
    // In raw mode Ctrl+C is a key; a SIGINT can still come from another process.
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let s = open(project.path(), home.path(), &[])?;
    signal(&s.child, Signal::INT)?;
    assert_eq!(s.ended()?, Some(130));
    Ok(())
}

#[test]
fn a_test_that_fails_early_leaves_no_hoh_behind() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let s = open(project.path(), home.path(), &[])?;
    let pid = s.child.0.id();
    assert!(
        s.pty
            .wait_for("never drawn", 0, Duration::from_millis(200))
            .is_err()
    );
    drop(s);
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "hoh {pid} is still there"
    );
    Ok(())
}
