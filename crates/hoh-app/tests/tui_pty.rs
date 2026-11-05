//! The TUI shell in a real pseudo-terminal (check tui-pty; ADR-014). Each test records `stty -g` before `hoh` starts
//! and compares it after `hoh` has exited:
//! - the Workspace opens offline, and opening it without pressing Enter writes nothing;
//! - an input submitted with Enter is in the project's history and Ctrl+R finds it in the next session, without
//!   sending anything;
//! - Ctrl+C then "脱离" exits 0;
//! - SIGTERM exits 143, SIGHUP 129 and SIGINT 130; a panic exits 101 with its message after the terminal is restored.
//!
//! In every case the alternate screen is entered and left, and the terminal attributes are those of before. The
//! names carry no TEST id: TEST-001-01/02 (the host parts) are M2-T13's.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;

use pty::{Pty, R, exit_within, find, signal, spawn_in, tree};
use rustix::process::Signal;
use std::path::Path;
use std::process::Child;
use std::time::Duration;

const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const WAIT: Duration = Duration::from_secs(10);

struct Session {
    pty: Pty,
    child: Child,
    baseline: String,
}

/// `hoh` started in a 100x30 PTY in `project`, once its Workspace is drawn (project name, mode, offline host).
fn open(project: &Path, home: &Path, env: &[(&str, &str)]) -> R<Session> {
    let pty = Pty::new(30, 100)?;
    let baseline = pty.stty()?;
    let child = spawn_in(&pty, project, home, &[], env)?;
    let name = project
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("project name")?;
    pty.wait_for(name, 0, WAIT)?;
    pty.wait_for("AUTO", 0, WAIT)?;
    pty.wait_for("host 离线", 0, WAIT)?;
    Ok(Session {
        pty,
        child,
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
