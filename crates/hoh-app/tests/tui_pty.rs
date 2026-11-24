//! The TUI shell in a real pseudo-terminal (check tui-pty; ADR-014). Each test records `stty -g` before `hoh` starts
//! and compares it after `hoh` has exited:
//! - the Workspace opens with the project's host connected (bare `hoh` starts one if none runs; each test stops it
//!   with `Daemons`, docs/adr/ADR-031.md decision 13), and opening it without pressing Enter writes no row;
//! - an input submitted with Enter is in the project's history and Ctrl+R finds it in the next session, without
//!   sending anything more;
//! - Ctrl+C then "脱离" exits 0;
//! - SIGTERM exits 143, SIGHUP 129 and SIGINT 130; a panic exits 101 with its message after the terminal is restored.
//! - a terminal that is closed under it (its window, an SSH connection) ends it: 129, or 74 when it was not the
//!   controlling terminal.
//!
//! In every case the alternate screen is entered and left, and the terminal attributes are those of before.
//!
//! TEST-001-01, TEST-001-02 and TEST-005-01 (M2-T13, docs/adr/ADR-031.md decisions 12 to 14) run `hoh` in the M2-T13
//! test world (`world`) and read the screen as a vt100 snapshot: the first two against an in-process host whose runner
//! is the world's scripted Team, TEST-005-01 against the host its own detach starts, with the world's fake CLIs.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod prerequisites;
mod pty;
mod s5;
mod world;

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

/// `hoh` started in a 100x30 PTY in `project`, once its Workspace is drawn (project name, mode, connected host).
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
    pty.wait_for("host 已连接", 0, WAIT)?;
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
        // Its last bytes read before they are looked at (convention v2: a state read after the event that makes it).
        self.pty.drained(WAIT)?;
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
fn a_project_whose_host_socket_would_not_fit_is_refused_before_hoh_starts() -> R {
    // M3C-01: with a long TMPDIR the project's `.hoh/host.sock` passes 107 bytes; the helper says so before `hoh` starts
    // instead of a host that never connects.
    let (home, base) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let project = base.path().join("p".repeat(100));
    std::fs::create_dir_all(&project)?;
    let pty = Pty::new(30, 100)?;
    let Err(e) = spawn_in(&pty, &project, home.path(), &[], &[]) else {
        return Err("hoh was started in a project whose socket path does not fit".into());
    };
    assert!(e.to_string().contains("longer than 107 bytes"), "{e}");
    Ok(())
}

#[test]
fn the_workspace_opens_connected_and_writes_nothing_until_enter() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let before = tree(project.path())?;
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.send("草稿 draft".as_bytes())?;
    s.pty.wait_for("草稿draft", 0, WAIT)?;
    // Ctrl+C clears the editor first; the second opens the choice. Keys are taken in order, so once a marker typed
    // after it is drawn, whatever the Ctrl+C drew is in the output (convention v2: an absence read after the event
    // that would make it, not after a fixed sleep; CIR-253).
    let from = s.pty.output().len();
    s.pty.send(b"\x03")?;
    s.pty.send(b"QZQ")?;
    s.pty.wait_for("QZQ", from, WAIT)?;
    assert!(
        !pty::squash(&s.pty.output()[from..]).contains("脱离"),
        "the first Ctrl+C must only clear a non-empty editor"
    );
    s.pty.send(b"\x03")?; // clears the marker
    assert_eq!(s.detach()?, Some(0));
    // The project held nothing; now only the host's own .hoh/ (whose socket is no file to read). Opening, listing and
    // detaching write no row and record no input (docs/adr/ADR-031.md E11).
    assert!(before.is_empty());
    let names: Vec<std::ffi::OsString> = std::fs::read_dir(project.path())?
        .map(|entry| entry.map(|e| e.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names, [".hoh"], "the project changed");
    assert!(!project.path().join(".hoh/input-history.jsonl").exists());
    for table in ["commands", "runs", "tasks", "input_queue"] {
        assert_eq!(
            scalar(project.path(), &format!("SELECT count(*) FROM {table}"))?,
            "0",
            "{table}"
        );
    }
    Ok(())
}

#[test]
fn submitted_input_is_kept_and_found_again_in_the_next_session() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let history = project.path().join(".hoh/input-history.jsonl");
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.send(b"hello\r")?;
    // The input is the goal of a new run, which the Workspace attaches (docs/adr/ADR-031.md decision 13).
    s.pty.wait_for("已附着", 0, WAIT)?;
    assert_eq!(std::fs::read_to_string(&history)?, "{\"text\":\"hello\"}\n");
    assert_eq!(s.detach()?, Some(0));

    let mut s = open(project.path(), home.path(), &[])?;
    let from = s.pty.output().len();
    s.pty.send(b"\x12")?;
    s.pty.send(b"hel")?;
    s.pty.wait_for("hello", from, WAIT)?;
    // Enter ends the search with the match in the editor; it sends nothing. Keys are taken in order, so once a marker
    // typed after it is drawn, whatever Enter drew is in the output (CIR-253).
    let entered = s.pty.output().len();
    s.pty.send(b"\r")?;
    s.pty.send(b"QZQ")?;
    s.pty.wait_for("QZQ", entered, WAIT)?;
    assert!(!pty::squash(&s.pty.output()[from..]).contains("你："));
    assert_eq!(
        scalar(project.path(), "SELECT count(*) FROM commands")?,
        "1",
        "only the first session's StartRun"
    );
    assert_eq!(std::fs::read_to_string(&history)?, "{\"text\":\"hello\"}\n");
    s.pty.send(b"\x03")?; // clears the editor
    assert_eq!(s.detach()?, Some(0));
    Ok(())
}

#[test]
fn sigterm_restores_the_terminal_and_exits_143() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.send(b"draft")?;
    s.pty.wait_for("draft", 0, WAIT)?;
    signal(&s.child, Signal::TERM)?;
    assert_eq!(s.ended()?, Some(143));
    assert!(
        !project.path().join(".hoh/input-history.jsonl").exists(),
        "a draft is not history"
    );
    Ok(())
}

#[test]
fn sighup_restores_the_terminal_and_exits_129() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
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
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let s = open(project.path(), home.path(), &[])?;
    std::thread::sleep(Duration::from_millis(300));
    let from = s.pty.output().len();
    s.pty.resize(40, 120)?;
    s.pty
        .wait_for("host 已连接", from, Duration::from_secs(2))?;
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
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
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
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
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
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
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
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
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
/// submitted one by one (M1T08R-20). The gap starts once hoh is known to have read the first part, so it is waited
/// for (M1T08R-30, -33).
#[test]
fn a_sequence_split_by_a_slow_link_is_still_one_key() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let history = project.path().join(".hoh/input-history.jsonl");
    let mut s = open(project.path(), home.path(), &[])?;
    s.pty.send(b"one\r")?;
    s.pty.wait_for("已附着", 0, WAIT)?;
    for (start, rest) in [
        (&b"\x1b["[..], &b"A\r"[..]),
        (b"\x1b[20", b"0~line1\rline2\x1b[201~\r"),
    ] {
        s.pty.send_and_wait_until_read(&s.child, start, WAIT)?;
        std::thread::sleep(Duration::from_millis(150));
        // Read before the next step, which starts from an empty queue.
        s.pty.send_and_wait_until_read(&s.child, rest, WAIT)?;
    }
    assert_eq!(s.detach()?, Some(0));
    assert_eq!(
        std::fs::read_to_string(&history)?,
        "{\"text\":\"one\"}\n{\"text\":\"one\"}\n{\"text\":\"line1 line2\"}\n"
    );
    Ok(())
}

/// The read handshake the slow-link test relies on fails when nothing reads the terminal: a process that never
/// reads its input leaves the typed bytes queued (M1T08R-33). Its terminal is not in raw mode, so a whole line is
/// typed: only a finished line counts as queued.
#[test]
fn the_read_handshake_fails_when_nothing_reads() -> R {
    let mut pty = Pty::new(30, 100)?;
    let sleeper = Hoh(std::process::Command::new("sleep")
        .arg("30")
        .stdin(pty.stdio()?)
        .spawn()?);
    let err = pty
        .send_and_wait_until_read(&sleeper, b"ab\n", Duration::from_secs(1))
        .err()
        .ok_or("the handshake passed with nothing reading")?;
    assert!(err.to_string().contains("3 bytes queued, not 0"), "{err}");
    Ok(())
}

/// A panic restores the terminal from its hook, before anything is unwound; by then nothing reads the terminal any
/// more, so what is typed while the hook restores it stays for the shell (M1T08R-14).
#[test]
fn input_typed_while_a_panic_restores_the_terminal_is_left_for_the_shell() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
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
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let mut s = open(project.path(), home.path(), &[])?;
    let from = s.pty.output().len();
    s.pty.send(b"\x12")?;
    let from = s.pty.wait_for("反向搜索", from, WAIT)?;
    s.pty.send(b"\x1b")?;
    s.pty.wait_for("Enter 发送", from, WAIT)?;
    assert_eq!(s.detach()?, Some(0));
    Ok(())
}

/// A burst of keys larger than one read (4 KiB) is taken in full: 5000 characters and Enter make one 5000-character
/// line in the history, with nothing left behind waiting for another key (M1T08R-25).
#[test]
fn a_burst_of_keys_is_taken_in_full() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
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

/// A long paste is drawn at once: 128 KiB in the single-line editor shows its end within seconds (it took minutes
/// when the view was found by dropping graphemes from the front one by one, all that time without looking at the
/// signal flags; M1T08R-31).
#[test]
fn a_long_paste_is_drawn_at_once() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let mut s = open(project.path(), home.path(), &[])?;
    let from = s.pty.output().len();
    s.pty
        .send(format!("\x1b[200~{}PASTE_END\x1b[201~", "x".repeat(128 << 10)).as_bytes())?;
    s.pty.wait_for("PASTE_END", from, WAIT)?;
    // Ctrl+C clears the editor first; the next one offers to leave.
    s.pty.send(b"\x03")?;
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
        // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
        let _daemons = Daemons(project.path().to_path_buf());
        let mut pty = Pty::new(30, 100)?;
        // Declared after the PTY, so dropped before it: a failure stops `hoh` first.
        let mut child = if ctty {
            spawn_in(&pty, project.path(), home.path(), &[], &[])?
        } else {
            spawn_without_ctty_in(&pty, project.path(), home.path())?
        };
        pty.wait_for("host 已连接", 0, WAIT)?;
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
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
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
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let s = open(project.path(), home.path(), &[])?;
    signal(&s.child, Signal::INT)?;
    assert_eq!(s.ended()?, Some(130));
    Ok(())
}

#[test]
fn the_tui_restores_the_terminal_before_it_says_what_its_stop_left_unfinished() -> R {
    // ADR-031, 设计修正 2, D, the TTY order (CLT-454, CLT-955; verification 9): bare `hoh` starts a run and is attached
    // to it; its host is killed, so the link's reader finds the host again and the start probes `systemd-run --version`
    // (a stand-in; a fake runtime directory stands in for the user's manager), held with its ticket taken. SIGTERM: the
    // TUI restores the terminal (attributes, alternate screen), then stops; its exit, held once the stop is done, has
    // written nothing yet; released, it says only that a start had not returned (the TUI never writes rule 2's
    // sentence), after the terminal was restored, and ends with 143.
    let Some(user) = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .filter(|u| Path::new("/var/lib/systemd/linger").join(u).is_file())
        .filter(|_| Path::new("/sys/fs/cgroup/cgroup.controllers").exists())
    else {
        return prerequisites::missing("no linger record for this user, or no cgroup v2, here");
    };
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let _daemons = Daemons(project.path().to_path_buf());
    // On the isolated PATH (`<home>/agents`): it knows only --version, so a scope is refused and the host is started
    // directly; the isolated XDG_RUNTIME_DIR (`<home>/run`) gets the manager's socket path.
    let agents = home.path().join("agents");
    std::fs::create_dir_all(&agents)?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        let fake = agents.join("systemd-run");
        std::fs::write(
            &fake,
            "#!/bin/sh\n[ \"$1\" = --version ] && echo 'systemd 259 (stand-in)' && exit 0\nexit 1\n",
        )?;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::create_dir_all(home.path().join("run/systemd"))?;
    std::fs::File::create(home.path().join("run/systemd/private"))?;
    let holds = home.path().join("holds");
    std::fs::create_dir_all(&holds)?;
    let holds_env = holds.to_str().ok_or("holds path")?;
    let mut s = open(
        project.path(),
        home.path(),
        &[("USER", &user), ("HOH_DEBUG_HOLD_DIR", holds_env)],
    )?;
    let outcome = (|| -> R<(usize, usize)> {
        let from = s.pty.output().len();
        s.pty.send(b"x\r")?;
        s.pty.wait_for("已附着", from, WAIT)?;
        // The next helper start is held with its ticket taken; the host goes, and the link's reader starts another.
        std::fs::write(holds.join("helper-before-spawn"), "")?;
        std::fs::write(holds.join("exit"), "")?;
        let info: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            project.path().join(".hoh/host.json"),
        )?)?;
        let host = info["pid"]
            .as_i64()
            .and_then(|p| i32::try_from(p).ok())
            .and_then(rustix::process::Pid::from_raw)
            .ok_or("no host pid")?;
        rustix::process::kill_process(host, Signal::KILL)?;
        let end = std::time::Instant::now() + Duration::from_secs(30);
        while !holds.join("helper-before-spawn.reached").exists() {
            if std::time::Instant::now() > end {
                return Err("the link's reader never started a helper".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        signal(&s.child, Signal::TERM)?;
        while !holds.join("exit.reached").exists() {
            if std::time::Instant::now() > end {
                return Err("the TUI's exit was never reached".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Held once the stop is done: the terminal is restored, and nothing is said yet.
        s.pty.drained(WAIT)?;
        let out = s.pty.output();
        let left = find(&out, LEAVE_ALT, 0).ok_or("the alternate screen was not left")?;
        assert_eq!(s.pty.stty()?, s.baseline, "terminal attributes at the exit");
        assert_eq!(
            find(&out, "一次启动仍未返回".as_bytes(), 0),
            None,
            "said before the exit"
        );
        std::fs::remove_file(holds.join("exit"))?;
        let said = s.pty.wait_for("一次启动仍未返回", left, WAIT)?;
        Ok((left, said))
    })();
    // Whatever happened, nothing stays held.
    let _ = std::fs::remove_file(holds.join("helper-before-spawn"));
    let _ = std::fs::remove_file(holds.join("exit"));
    let (left, said) = outcome?;
    assert!(left < said, "said before the terminal was restored");
    let out = s.pty.output();
    assert_eq!(
        find(&out, "收到信号".as_bytes(), 0),
        None,
        "rule 2's sentence"
    );
    assert_eq!(s.ended()?, Some(143));
    Ok(())
}

#[test]
fn a_test_that_fails_early_leaves_no_hoh_behind() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    // Bare `hoh` starts the project's host if none runs: stopped however the test ends.
    let _daemons = Daemons(project.path().to_path_buf());
    let s = open(project.path(), home.path(), &[])?;
    let pid = s.child.0.id();
    assert!(
        s.pty
            .wait_for("never drawn", 0, Duration::from_millis(200))
            .is_err()
    );
    drop(s);
    assert!(
        hoh_agents::identity::reaped(pid)?,
        "hoh {pid} is still there"
    );
    Ok(())
}

/// TEST-043-03's world: run R RUNNING with writer task T1, a developer bound with its capabilities.
const ATTACH_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', :p, 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's1', 'READY', 1, 'developer', 1, 1, 1);
";

fn sqlite(db: &Path, script: &str, input: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(input.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// A FakeAgent that works 60 s: the leader of its own process group, held at its start gate; its leader is reaped
/// and its group SIGKILLed when it ends, as the substrate does.
#[derive(Debug)]
struct Sixty;

impl hoh_app::host::Executor for Sixty {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        let _ = answer.send(Ok(()));
        ack
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r go; [ \"$go\" = go ] || exit 0; sleep 60"])
            .process_group(0)
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let gate = hoh_app::host::Gate::new(move || {
            use std::io::Write as _;
            stdin
                .ok_or("no stdin")?
                .write_all(b"go\n")
                .map_err(|e| e.to_string())
        });
        let (end, ended) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = end.send(hoh_app::host::Outcome {
                success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: format!("{status:?}"),
                group_gone: hoh_agents::identity::group_empty(pid),
                class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                observed: Default::default(),
                unknown_effect: false,
                retry_at: None,
            });
        });
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
            ran: None,
        })
    }
}

/// Whether process `pid`, started at `start`, is alive: not ended as a whole by identity::ended. A zombie, which signal
/// 0 still reaches, is not alive, nor is a number taken again since (ADR-022 decision 10, v2; CIR-150's class).
fn alive(pid: u32, start: &str) -> R<bool> {
    // A look that fails is an error, never "alive" nor "gone" (CIR-202, -250).
    Ok(!hoh_agents::identity::ended(pid, Some(start))?)
}

/// `hoh attach R` in a PTY, once the Workspace bound to R is drawn.
fn open_attach(project: &Path, home: &Path) -> R<Session> {
    let pty = Pty::new(30, 100)?;
    let baseline = pty.stty()?;
    let child = spawn_in(&pty, project, home, &["attach", "R"], &[])?;
    pty.wait_for("run R", 0, WAIT)?;
    pty.wait_for("host 已连接", 0, WAIT)?;
    Ok(Session {
        child,
        pty,
        baseline,
    })
}

#[test]
fn test_043_03_the_tui_leaves_on_sighup_sigterm_and_detach_with_the_host_and_agent_alive_and_the_run_unchanged()
-> R {
    // AC-043-03: hoh's TUI attached to run R in a PTY while its FakeAgent works 60 s; the host and Agent pids recorded.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir()?;
    let (project, home) = (dir.path().join("p"), dir.path().join("home"));
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&home)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let config = || {
        let mut config = hoh_app::host::HostConfig::new(&project);
        config.port = 0;
        config.store.wal_probe = Some(hoh_core::persistence::selftest::ProbeCommand {
            program: env!("CARGO_BIN_EXE_hoh").into(),
            args: vec![hoh_core::persistence::selftest::PROBE_ARG.into()],
            env: Vec::new(),
            answer_timeout: WAIT,
        });
        config
    };
    let first = runtime.block_on(hoh_app::host::Host::start(config()))?;
    runtime.block_on(first.shutdown());
    let db = project.join(".hoh/state.db");
    sqlite(
        &db,
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
         c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit()",
        ATTACH_WORLD,
    )?;
    let query = |sql: &str| {
        sqlite(
            &db,
            "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); print('' if r is None or r[0] is None else r[0])",
            sql,
        )
    };
    let mut with_runner = config();
    with_runner.runner = Some(hoh_app::host::RunnerConfig {
        executor: std::sync::Arc::new(Sixty),
        context: hoh_core::decision::Context {
            required_capabilities: [(
                "developer".to_owned(),
                ["structured_events", "command_exec"]
                    .map(str::to_owned)
                    .into_iter()
                    .collect(),
            )]
            .into(),
            ..hoh_core::decision::Context::default()
        },
    });
    let host = runtime.block_on(start_when_free(with_runner))?;
    let end = std::time::Instant::now() + WAIT;
    while query("SELECT state FROM attempts WHERE task_id = 'T1'")? != "RUNNING" {
        if std::time::Instant::now() > end {
            return Err("T1 did not run".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let agent: u32 = query("SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    let agent_start = hoh_agents::identity::start_id(agent)?.ok_or("the Agent is gone")?;
    let host_pid = std::process::id();
    let host_start = hoh_agents::identity::start_id(host_pid)?.ok_or("no start time")?;
    let run = || query("SELECT state || ' ' || revision FROM runs WHERE run_id = 'R'");
    let before = run()?;
    let unchanged = |step: &str| -> R {
        assert!(
            alive(host_pid, &host_start)? && alive(agent, &agent_start)?,
            "{step}: the host or the Agent is gone"
        );
        assert_eq!(run()?, before, "{step}: the run moved");
        Ok(())
    };
    // SIGHUP: the TUI leaves.
    let s = open_attach(&project, &home)?;
    signal(&s.child, Signal::HUP)?;
    assert_eq!(s.ended()?, Some(129));
    unchanged("SIGHUP")?;
    // SIGTERM: it leaves with the terminal restored (the alternate screen left, the attributes back: icanon, echo).
    let s = open_attach(&project, &home)?;
    signal(&s.child, Signal::TERM)?;
    assert_eq!(s.ended()?, Some(143));
    unchanged("SIGTERM")?;
    // Ctrl+C on the idle top level: only the choice of detach, pause and cancel, and the TUI stays until 脱离.
    let mut s = open_attach(&project, &home)?;
    let from = s.pty.output().len();
    s.pty.send(b"\x03")?;
    for choice in ["脱离", "暂停", "取消任务"] {
        s.pty.wait_for(choice, from, WAIT)?;
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(s.child.0.try_wait()?.is_none(), "the TUI left at Ctrl+C");
    s.pty.send(b"\r")?;
    assert_eq!(s.ended()?, Some(0));
    unchanged("detach")?;
    runtime.block_on(host.shutdown());
    // The Agent's group, only while the Agent is still the process started at `agent_start` (ADR-022 decision 10:
    // identity::find holds it by a pidfd and checks its start time, Found::kill signals the group only right after a
    // look that shows it held); one that ended with the host gets nothing.
    let boot = hoh_agents::identity::boot_id()?;
    if let Ok(found) =
        hoh_agents::identity::find(Some(i64::from(agent)), Some(&boot), Some(&agent_start))?
    {
        found.kill();
    }
    Ok(())
}

/// The `hoh __host <project>` processes of `project`, by their command lines (`identity::running_with`: a failed
/// listing is an error, a copy forked and not exec'd is none; ADR-022 decision 10, v2).
fn hosts_of(project: &Path) -> std::io::Result<Vec<u32>> {
    let wanted = format!(
        "{}\0__host\0{}\0",
        env!("CARGO_BIN_EXE_hoh"),
        project.display()
    );
    hoh_agents::identity::running_with(|c| c == wanted.as_bytes())
}

/// Stops the hosts the product started for a project when dropped, however the test ends (docs/adr/ADR-021.md
/// decision 11, M2T06R-62, -154): each found by its command line, SIGTERM, then SIGKILL after 10 s.
struct Daemons(std::path::PathBuf);

impl Drop for Daemons {
    fn drop(&mut self) {
        let signal = |pid: u32, signal: Signal| {
            if let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, signal);
            }
        };
        // Cleanup only: a failed listing stops nothing and asserts nothing.
        for pid in hosts_of(&self.0).unwrap_or_default() {
            signal(pid, Signal::TERM);
        }
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while hosts_of(&self.0).is_ok_and(|h| !h.is_empty()) && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        for pid in hosts_of(&self.0).unwrap_or_default() {
            signal(pid, Signal::KILL);
        }
    }
}

#[test]
fn a_host_started_by_a_detach_in_a_terminal_outlives_its_hang_up() -> R {
    // CR-010 decision 2 (REQ-005 behaviour 2): supplementary detach evidence, no TEST id (AC-005-01 is M2-T13's).
    let dir = tempfile::Builder::new().prefix("hoh").tempdir()?;
    let (project, home) = (dir.path().join("p"), dir.path().join("home"));
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&home)?;
    // The host the detach starts is stopped however the test ends, found by its command line (M2T06R-257).
    let _daemons = Daemons(project.clone());
    let mut pty = Pty::new(30, 100)?;
    let mut client = spawn_in(
        &pty,
        &project,
        &home,
        &["-p", "x", "--detach", "--output", "json"],
        &[],
    )?;
    assert_eq!(exit_within(&mut client, WAIT)?.code(), Some(0));
    let host: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".hoh/host.json"))?)?;
    let pid = u32::try_from(host["pid"].as_u64().ok_or("no pid")?)?;
    let start = hoh_agents::identity::start_id(pid)?.ok_or("the host is gone")?;
    pty.hang_up()?;
    drop(pty);
    std::thread::sleep(Duration::from_millis(500));
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<&str> = stat
        .rsplit_once(") ")
        .ok_or("no stat")?
        .1
        .split(' ')
        .collect();
    let (session, tty): (u32, i64) = (fields[3].parse()?, fields[4].parse()?);
    assert!(alive(pid, &start)?, "the host is gone after the hang-up");
    assert_eq!((session, tty), (pid, 0), "its own session, no terminal");
    // To the host of its start time only (CIR-103's rule), and sent (CIR-250).
    assert!(
        hoh_agents::identity::signal_if_same(pid, &start, rustix::process::Signal::TERM)?,
        "the host was gone before its SIGTERM"
    );
    let end = std::time::Instant::now() + WAIT;
    while alive(pid, &start)? && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid, &start)?, "the host did not stop on SIGTERM");
    Ok(())
}

/// The parent of `pid` (`/proc/<pid>/stat` field 4).
fn parent_of(pid: u32) -> R<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after = stat.rsplit_once(") ").ok_or("no stat")?.1;
    Ok(after.split(' ').nth(1).ok_or("no parent")?.parse()?)
}

/// Holds the project database's write lock until dropped (a `BEGIN IMMEDIATE` of a python3 of its own): a host that
/// starts meanwhile waits for it at its first write, within its busy timeout (5 s), as a slow start would.
struct WriteLock(std::process::Child);

impl WriteLock {
    fn take(db: &Path) -> R<WriteLock> {
        use std::io::BufRead as _;
        let mut child = std::process::Command::new("python3")
            .args([
                "-c",
                "import sqlite3,sys; c=sqlite3.connect(sys.argv[1], isolation_level=None); c.execute('BEGIN IMMEDIATE'); \
                 print('locked', flush=True); sys.stdin.readline(); c.execute('COMMIT')",
            ])
            .arg(db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        // Held before anything can fail, so its drop reaps the child on every path (CLT-152).
        let stdout = child.stdout.take();
        let lock = WriteLock(child);
        let mut line = String::new();
        std::io::BufReader::new(stdout.ok_or("no stdout")?).read_line(&mut line)?;
        if line.trim() != "locked" {
            return Err(format!("the write lock was not taken: {line:?}").into());
        }
        Ok(lock)
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        use std::io::Write as _;
        // Its own unreaped child: committed and ended, or killed.
        if let Some(mut stdin) = self.0.stdin.take() {
            let _ = stdin.write_all(b"\n");
        }
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while matches!(self.0.try_wait(), Ok(None)) && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// CLT-01 (REQ-004 failure mode 2; REQ-001 failure mode 3): `hoh attach <run>` with a terminal takes `sig` while the
/// host it started is not ready yet (the test holds the database's write lock, so the host waits at its first write): it
/// ends at once with `code`, the terminal as it was (the Workspace never entered), and the host goes on in the
/// background and becomes ready once the lock goes.
fn a_signal_while_the_attachs_host_starts_ends_it(sig: Signal, code: i32) -> R {
    let dir = tempfile::Builder::new().prefix("hoh").tempdir()?;
    let (project, home) = (dir.path().join("p"), dir.path().join("home"));
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&home)?;
    let _daemons = Daemons(project.clone());
    // A run, and its host stopped: the attach starts the next one.
    let detach_pty = Pty::new(30, 100)?;
    let mut detach = spawn_in(
        &detach_pty,
        &project,
        &home,
        &["-p", "x", "--detach", "--output", "json"],
        &[],
    )?;
    assert_eq!(exit_within(&mut detach, WAIT)?.code(), Some(0));
    drop(Daemons(project.clone()));
    assert!(hosts_of(&project)?.is_empty(), "the detach's host stopped");
    let db = project.join(".hoh/state.db");
    let run = sqlite(
        &db,
        "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute('SELECT run_id FROM runs').fetchone()[0])",
        "",
    )?;
    let lock = WriteLock::take(&db)?;
    let pty = Pty::new(30, 100)?;
    let baseline = pty.stty()?;
    let mut client = spawn_in(&pty, &project, &home, &["attach", &run], &[])?;
    let attach = client.0.id();
    let end = std::time::Instant::now() + WAIT;
    let host = loop {
        let hosts = hosts_of(&project)?;
        if let [host] = hosts.as_slice()
            && parent_of(*host)? == attach
        {
            break *host;
        }
        if std::time::Instant::now() > end {
            return Err(format!("the attach started no host: {hosts:?}").into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        !project.join(".hoh/host.json").exists(),
        "the host was ready before the signal"
    );
    signal(&client, sig)?;
    let status = exit_within(&mut client, Duration::from_secs(2))?;
    pty.drained(WAIT)?;
    assert_eq!(status.code(), Some(code));
    assert_eq!(
        find(&pty.output(), ENTER_ALT, 0),
        None,
        "the Workspace was entered"
    );
    assert_eq!(pty.stty()?, baseline, "terminal attributes");
    // The host it started goes on: ready once the lock goes.
    drop(lock);
    let end = std::time::Instant::now() + WAIT;
    loop {
        let ready = std::fs::read_to_string(project.join(".hoh/host.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|h| h["pid"].as_u64());
        if ready == Some(u64::from(host)) {
            break;
        }
        if std::time::Instant::now() > end {
            return Err(format!("the attach's host did not go on to be ready: {ready:?}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn a_sigterm_while_the_attachs_host_starts_ends_it_with_143() -> R {
    a_signal_while_the_attachs_host_starts_ends_it(Signal::TERM, 143)
}

#[test]
fn a_sigint_while_the_attachs_host_starts_ends_it_with_130() -> R {
    a_signal_while_the_attachs_host_starts_ends_it(Signal::INT, 130)
}

/// Wait until process `pid` has a handler for `signal` (its /proc status's SigCgt).
fn handles(pid: u32, signal: Signal) -> R {
    let bit = 1u64 << (signal.as_raw() - 1);
    let end = std::time::Instant::now() + WAIT;
    loop {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
        let caught = status
            .lines()
            .find_map(|l| l.strip_prefix("SigCgt:"))
            .ok_or("no SigCgt")?;
        if u64::from_str_radix(caught.trim(), 16)? & bit != 0 {
            return Ok(());
        }
        if std::time::Instant::now() > end {
            return Err(format!("{pid} installed no handler for {signal:?}").into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_signal_while_the_attach_waits_behind_the_start_lock_ends_it_with_the_terminal_untouched() -> R
{
    // ADR-031's design correction (client signals), rules 2 and 3 (CLT-100): `hoh attach <run>` with a terminal, its
    // host's start held behind the project's start lock another client holds, takes SIGTERM: 143 within 2 s, said as
    // REQ-004 says it (nothing was attached), the Workspace never entered, the terminal's attributes as they were.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir()?;
    let (project, home) = (dir.path().join("p"), dir.path().join("home"));
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&home)?;
    let _daemons = Daemons(project.clone());
    let detach_pty = Pty::new(30, 100)?;
    let mut detach = spawn_in(
        &detach_pty,
        &project,
        &home,
        &["-p", "x", "--detach", "--output", "json"],
        &[],
    )?;
    assert_eq!(exit_within(&mut detach, WAIT)?.code(), Some(0));
    drop(Daemons(project.clone()));
    let run = sqlite(
        &project.join(".hoh/state.db"),
        "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute('SELECT run_id FROM runs').fetchone()[0])",
        "",
    )?;
    let lock = std::fs::File::open(&project)?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)?;
    let pty = Pty::new(30, 100)?;
    let baseline = pty.stty()?;
    let mut client = spawn_in(&pty, &project, &home, &["attach", &run], &[])?;
    let outcome = (|| -> R {
        handles(client.0.id(), Signal::TERM)?;
        signal(&client, Signal::TERM)?;
        let status = exit_within(&mut client, Duration::from_secs(2))?;
        pty.drained(WAIT)?;
        assert_eq!(status.code(), Some(143));
        let shown = pty.output();
        assert_eq!(
            find(&shown, ENTER_ALT, 0),
            None,
            "the Workspace was entered"
        );
        let said = String::from_utf8_lossy(&shown);
        assert!(
            said.contains("收到信号") && said.contains("没有附着"),
            "{said}"
        );
        assert_eq!(pty.stty()?, baseline, "terminal attributes");
        Ok(())
    })();
    let _ = rustix::fs::flock(&lock, rustix::fs::FlockOperation::Unlock);
    outcome
}

/// POST `body` to the commands of the project's host named by its `host.json`; the response's status line.
fn post_command(project: &Path, body: &str) -> R<String> {
    use std::io::{Read as _, Write as _};
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".hoh/host.json"))?)?;
    let mut stream =
        std::os::unix::net::UnixStream::connect(info["socket"].as_str().ok_or("no socket")?)?;
    stream.set_read_timeout(Some(WAIT))?;
    write!(
        stream,
        "POST /v1/commands HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nX-HoH-Token: {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        info["token"].as_str().ok_or("no token")?,
        body.len()
    )?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply.lines().next().unwrap_or_default().to_owned())
}

/// Start a host in a project another host of this process just let go. A process that spawns children (a parallel
/// test's agents, or the WAL self-test's probe) shares every open file with each child until it execs, so a lock just
/// released can still be held for that instant: a refusal (69) is tried again for a moment (as tests/host.rs does).
async fn start_when_free(
    config: hoh_app::host::HostConfig,
) -> Result<hoh_app::host::Host, hoh_app::host::StartError> {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match hoh_app::host::Host::start(config.clone()).await {
            Err(e) if e.exit_code == 69 && std::time::Instant::now() < end => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

/// An in-process host's configuration for `project`: an OS-chosen port, the product binary as the WAL probe.
fn in_process(project: &Path) -> hoh_app::host::HostConfig {
    let mut config = hoh_app::host::HostConfig::new(project);
    config.port = 0;
    config.store.wal_probe = Some(hoh_core::persistence::selftest::ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![hoh_core::persistence::selftest::PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: WAIT,
    });
    config
}

fn scalar(project: &Path, sql: &str) -> R<String> {
    sqlite(
        &project.join(".hoh/state.db"),
        "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); print('' if r is None or r[0] is None else r[0])",
        sql,
    )
}

/// Ctrl+C, then the choice `down` places below 脱离, then Enter; waits for `said`.
fn choose(s: &mut Session, down: usize, said: &str) -> R {
    let from = s.pty.output().len();
    s.pty.send(b"\x03")?;
    s.pty.wait_for("取消任务", from, WAIT)?;
    for _ in 0..down {
        s.pty.send(b"\x1b[B")?;
        std::thread::sleep(Duration::from_millis(100));
    }
    s.pty.send(b"\r")?;
    s.pty.wait_for(said, from, Duration::from_secs(20))?;
    Ok(())
}

#[test]
fn the_tuis_pause_and_cancel_choices_send_their_own_commands_and_it_stays_attached() -> R {
    // M2T06R-55 (ADR-021 decision 10): R RUNNING with nothing running. 暂停 sends PauseRun (R reaches PAUSED), then
    // 取消任务 sends CancelRun (R reaches CANCELLED); the TUI stays attached through both and shows the run's events.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir()?;
    let (project, home) = (dir.path().join("p"), dir.path().join("home"));
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&home)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    // The TUI starts a product host if it finds none: stopped however the test ends.
    let _daemons = Daemons(project.clone());
    let host = runtime.block_on(hoh_app::host::Host::start(in_process(&project)))?;
    sqlite(
        &project.join(".hoh/state.db"),
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
         c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit()",
        "INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES
           ('R', :p, 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z');",
    )?;
    let mut s = open_attach(&project, &home)?;
    choose(&mut s, 1, "PauseRun 已受理")?;
    s.pty.wait_for("run.paused", 0, Duration::from_secs(20))?;
    assert_eq!(
        scalar(&project, "SELECT state FROM runs WHERE run_id = 'R'")?,
        "PAUSED"
    );
    choose(&mut s, 2, "CancelRun 已受理")?;
    s.pty
        .wait_for("run.cancelled", 0, Duration::from_secs(20))?;
    assert_eq!(
        scalar(&project, "SELECT state FROM runs WHERE run_id = 'R'")?,
        "CANCELLED"
    );
    assert_eq!(
        scalar(
            &project,
            "SELECT group_concat(kind, ' ') FROM (SELECT kind FROM commands ORDER BY rowid)"
        )?,
        "PauseRun CancelRun"
    );
    assert!(s.child.0.try_wait()?.is_none(), "the TUI left");
    assert_eq!(s.detach()?, Some(0));
    runtime.block_on(host.shutdown());
    Ok(())
}

#[test]
fn an_attached_tui_goes_on_with_its_run_through_a_host_started_again() -> R {
    // M2T06R-04 (REQ-005 behaviour 3, ADR-021 decisions 4 and 10): the TUI attached to R (PAUSED), its host stopped
    // normally and another started (another token). The TUI shows the run's next event, received from the new host,
    // and its pause is accepted there.
    let dir = tempfile::Builder::new().prefix("hoh").tempdir()?;
    let (project, home) = (dir.path().join("p"), dir.path().join("home"));
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&home)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let config = || {
        let mut config = hoh_app::host::HostConfig::new(&project);
        config.port = 0;
        config.store.wal_probe = Some(hoh_core::persistence::selftest::ProbeCommand {
            program: env!("CARGO_BIN_EXE_hoh").into(),
            args: vec![hoh_core::persistence::selftest::PROBE_ARG.into()],
            env: Vec::new(),
            answer_timeout: WAIT,
        });
        config
    };
    // The TUI starts a product host if it finds none (its host stopped): stopped however the test ends.
    let _daemons = Daemons(project.clone());
    let first = runtime.block_on(hoh_app::host::Host::start(config()))?;
    sqlite(
        &project.join(".hoh/state.db"),
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
         c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit()",
        "INSERT INTO runs(run_id, project_id, state, mode, revision, resume_state, created_at) VALUES
           ('R', :p, 'PAUSED', 'AUTO', 1, 'CREATED', '2026-11-07T00:00:00Z');",
    )?;
    let token = |p: &Path| -> R<String> {
        let info: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p.join(".hoh/host.json"))?)?;
        Ok(info["token"].as_str().ok_or("no token")?.to_owned())
    };
    let before = token(&project)?;
    let mut s = open_attach(&project, &home)?;
    runtime.block_on(first.shutdown());
    let second = runtime.block_on(start_when_free(config()))?;
    assert_ne!(
        token(&project)?,
        before,
        "the new host has a token of its own"
    );
    let from = s.pty.output().len();
    let resumed = post_command(
        &project,
        r#"{"schema_version":"1.0","command_id":"00000000-0000-4000-8000-000000000001","idempotency_key":"k-resume","kind":"ResumeRun","run_id":"R","expected_revision":1,"payload":{"reason":"test"}}"#,
    )?;
    assert!(resumed.contains(" 202 "), "{resumed}");
    s.pty
        .wait_for("run.resumed", from, Duration::from_secs(20))?;
    // Ctrl+C, the second choice (暂停), Enter: sent to the new host.
    let from = s.pty.output().len();
    s.pty.send(b"\x03")?;
    s.pty.wait_for("暂停", from, WAIT)?;
    s.pty.send(b"\x1b[B")?;
    std::thread::sleep(Duration::from_millis(100));
    s.pty.send(b"\r")?;
    s.pty
        .wait_for("PauseRun 已受理", from, Duration::from_secs(20))?;
    assert_eq!(
        sqlite(
            &project.join(".hoh/state.db"),
            "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); print(r[0])",
            "SELECT count(*) FROM commands WHERE kind = 'PauseRun'",
        )?,
        "1"
    );
    assert_eq!(s.detach()?, Some(0));
    runtime.block_on(second.shutdown());
    Ok(())
}

/// TEST-001-01's runs, their ids as a host gives them (UUIDs): A PAUSED at revision 7, B COMPLETED; neither has a
/// specification or a Commander binding, so nothing wakes for them.
const RUN_A: &str = "a1f04c7e-2b9d-4e63-8c5a-7d2e9b13f046";
const RUN_B: &str = "b7c2e815-6d4f-4a90-9e3b-15f8a0c4d272";

/// The rows of the commands, runs, tasks and input_queue tables.
const COUNTS: &str = "SELECT (SELECT count(*) FROM commands) || ' ' || (SELECT count(*) FROM runs) || ' ' || \
    (SELECT count(*) FROM tasks) || ' ' || (SELECT count(*) FROM input_queue)";

#[test]
fn test_001_01_the_workspace_shows_the_runs_and_a_sigterm_changes_nothing() -> R {
    // AC-001-01: the project's store holds run A PAUSED at revision 7 and run B COMPLETED; its host runs (in this
    // process, its runner the world's scripted Team: nothing wakes, it takes no work). `hoh` in a 120x40 PTY
    // (TERM=xterm-256color): once the top bar is drawn (within 10 s), 5 s idle, `draft` typed without Enter, 2 s idle,
    // SIGTERM.
    let name = "hoh-tui-runs";
    let world = world::World::named(name)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(world.init_store())?;
    world.exec(&format!(
        "INSERT INTO runs(run_id, project_id, state, mode, revision, resume_state, created_at) VALUES
           ('{RUN_A}', :p, 'PAUSED', 'AUTO', 7, 'RUNNING', '2026-11-15T00:00:01Z'),
           ('{RUN_B}', :p, 'COMPLETED', 'AUTO', 3, NULL, '2026-11-15T00:00:00Z');"
    ))?;
    let team = world::Team::new(|_| Vec::new());
    let host = runtime.block_on(world.team_host(team.clone(), None))?;
    let outcome = (|| -> R {
        // Host readiness precedes the runner's first checkpoint, which brings even PAUSED runs up to the project's
        // team revision. Observe that commit before the baseline: only the TUI and SIGTERM must leave it unchanged.
        let current_revision = world.query(&format!(
            "SELECT max(revision) FROM team_revisions WHERE project_id = '{}' AND state = 'APPLIED'",
            host.project_id
        ))?;
        world.wait(
            &format!("SELECT config_revision FROM runs WHERE run_id = '{RUN_A}'"),
            &current_revision,
            WAIT,
        )?;
        let counts = world.query(COUNTS)?;
        let runs = world.rows("SELECT * FROM runs ORDER BY run_id")?;
        let seq: i64 = world
            .query("SELECT coalesce(max(seq), 0) FROM events")?
            .parse()?;
        let info: serde_json::Value =
            serde_json::from_slice(&std::fs::read(world.project().join(".hoh/host.json"))?)?;
        let host_pid = u32::try_from(info["pid"].as_u64().ok_or("no host pid")?)?;
        let (rows, cols) = (40, 120);
        let mut pty = Pty::new(rows, cols)?;
        let stty = pty.stty()?;
        let mut hoh = world.hoh_pty(&pty, &[])?;
        pty::screen_until(&pty, rows, cols, WAIT, |s| {
            s.lines()
                .next()
                .is_some_and(|top| top.contains(name) && top.contains("host 已连接"))
        })?;
        std::thread::sleep(Duration::from_secs(5));
        pty.send(b"draft")?;
        pty::screen_until(&pty, rows, cols, WAIT, |s| {
            s.lines().nth(usize::from(rows) - 2) == Some("> draft")
        })?;
        std::thread::sleep(Duration::from_secs(2));
        // The screen before SIGTERM: everything written so far read first.
        pty.drained(WAIT)?;
        let screen = pty::snapshot(&pty.output(), rows, cols)?;
        signal(&hoh, Signal::TERM)?;
        let status = exit_within(&mut hoh, WAIT)?;
        pty.drained(WAIT)?;
        let out = pty.output();
        // Exit 143.
        assert_eq!(status.code(), Some(143), "{screen}");
        // The screen before SIGTERM shows the project's name (its top bar) and run A by its short id, PAUSED.
        let top = screen.lines().next().unwrap_or_default();
        assert!(top.contains(&format!(" {name} ")), "{screen}");
        let short = &RUN_A[..8];
        assert!(
            screen
                .lines()
                .any(|l| l.starts_with(short) && l.contains("PAUSED")),
            "no line of run A ({short}, PAUSED):\n{screen}"
        );
        // After the last ESC[?1049h come ESC[?1049l and ESC[?25h.
        let entered = out
            .windows(ENTER_ALT.len())
            .rposition(|w| w == ENTER_ALT)
            .ok_or("the alternate screen was never entered")?;
        assert!(
            find(&out, LEAVE_ALT, entered).is_some(),
            "the alternate screen was not left"
        );
        assert!(
            find(&out, SHOW_CURSOR, entered).is_some(),
            "the cursor was not shown again"
        );
        // `stty -g` byte for byte as before.
        assert_eq!(pty.stty()?, stty, "terminal attributes");
        // The host is the one this process runs (its pid is ours, so a look at that pid proves nothing: M2T13R-355); that
        // it lives on is its answer to the next request, and no other host was started.
        assert_eq!(host_pid, std::process::id());
        let (served, _) = runtime.block_on(world::exchange(&host, "GET", "/v1/runs", None))?;
        assert_eq!(served, 200);
        assert!(world.hosts()?.is_empty(), "{:?}", world.hosts());
        // Nothing written: the four tables' rows as before, the runs as they were (A PAUSED at 7, B COMPLETED), and
        // no run or task transition among the events of the time.
        assert_eq!(
            world.query(COUNTS)?,
            counts,
            "commands runs tasks input_queue"
        );
        assert_eq!(world.rows("SELECT * FROM runs ORDER BY run_id")?, runs);
        assert_eq!(
            world.query(&format!(
                "SELECT state || ' ' || revision FROM runs WHERE run_id = '{RUN_A}'"
            ))?,
            "PAUSED 7"
        );
        assert_eq!(
            world.query(&format!("SELECT state FROM runs WHERE run_id = '{RUN_B}'"))?,
            "COMPLETED"
        );
        let moved = world.rows(&format!(
            "SELECT seq, kind FROM events WHERE seq > {seq} AND (kind LIKE 'run.%' OR kind LIKE 'task.%')"
        ))?;
        assert!(moved.is_empty(), "{moved:?}");
        Ok(())
    })();
    runtime.block_on(host.shutdown());
    outcome?;
    assert!(
        team.turns().is_empty() && team.refused().is_empty(),
        "{team:?}"
    );
    Ok(())
}

#[test]
fn test_001_02_enter_starts_one_run_and_a_detach_sends_nothing_more() -> R {
    // AC-001-02: an empty Git repository with no configuration file (docs/adr/ADR-031.md, implementation record S4: its
    // effective default_mode is the built-in AUTO; a file would make the host write a ConfigureTeam of its own), its host
    // running (in this process, its runner the world's scripted Team). `hoh` in a 100x30 PTY: `hello` and Enter, then
    // Ctrl+C on the empty editor and 脱离; `hoh` again: Ctrl+R and `hel`, then it leaves. The new run's state is not
    // the AC's (CREATED, or BLOCKED for no specification once StartRun adopts one, decision 1).
    let world = world::World::named("hoh-tui-enter")?;
    world.unconfigure()?;
    world.git(&["init", "-q"])?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let team = world::Team::new(|_| Vec::new());
    let host = runtime.block_on(world.team_host(team.clone(), None))?;
    let outcome = (|| -> R {
        assert!(!world.project().join(".hoh/config.toml").exists());
        assert_eq!(
            world.git(&["rev-parse", "--is-inside-work-tree"])?,
            "true\n"
        );
        assert_eq!(world.query("SELECT count(*) FROM runs")?, "0");
        let commands =
            || world.rows("SELECT kind, run_id, payload_json FROM commands ORDER BY rowid");
        let (rows, cols) = (30, 100);
        let editor = usize::from(rows) - 2;
        let top_shows_auto = |s: &str| {
            s.lines()
                .next()
                .is_some_and(|top| top.contains("模式 AUTO"))
        };
        // First session: the top bar shows AUTO; no command before Enter.
        let mut pty = Pty::new(rows, cols)?;
        let mut hoh = world.hoh_pty(&pty, &[])?;
        pty::screen_until(&pty, rows, cols, WAIT, top_shows_auto)?;
        let before = commands()?;
        assert!(before.is_empty(), "a command before Enter: {before:?}");
        pty.send(b"hello")?;
        pty::screen_until(&pty, rows, cols, WAIT, |s| {
            s.lines().nth(editor) == Some("> hello")
        })?;
        pty.send(b"\r")?;
        // One StartRun, which the Workspace attaches (its status bar counts the run's events).
        world.wait("SELECT count(*) FROM commands", "1", WAIT)?;
        pty::screen_until(&pty, rows, cols, WAIT, |s| {
            s.lines()
                .last()
                .is_some_and(|status| status.contains("事件"))
        })?;
        let sent = commands()?;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["kind"], "StartRun");
        let payload: serde_json::Value =
            serde_json::from_str(sent[0]["payload_json"].as_str().ok_or("no payload")?)?;
        assert_eq!(
            payload["goal"].as_str().map(str::as_bytes),
            Some(&b"hello"[..]),
            "{payload}"
        );
        assert_eq!(payload["mode"], "AUTO", "{payload}");
        assert_eq!(payload["interactive"], true, "{payload}");
        let run = sent[0]["run_id"].as_str().ok_or("no run_id")?.to_owned();
        // Ctrl+C on the empty editor, 脱离 chosen: exit 0, the run not cancelled, no command more.
        pty.send(b"\x03")?;
        pty::screen_until(&pty, rows, cols, WAIT, |s| s.contains("> 脱离"))?;
        pty.send(b"\r")?;
        let first = exit_within(&mut hoh, WAIT)?;
        assert_eq!(first.code(), Some(0));
        let state = world.query(&format!("SELECT state FROM runs WHERE run_id = '{run}'"))?;
        assert!(
            !["CANCEL_REQUESTED", "CANCELLED"].contains(&state.as_str()),
            "{state}"
        );
        assert_eq!(commands()?, sent, "a command after the detach");
        drop(hoh);
        drop(pty);
        // Second session: Ctrl+R and `hel` give `hello` in the editor; nothing sent; it leaves.
        let mut pty = Pty::new(rows, cols)?;
        let mut hoh = world.hoh_pty(&pty, &[])?;
        pty::screen_until(&pty, rows, cols, WAIT, top_shows_auto)?;
        pty.send(b"\x12")?;
        pty::screen_until(&pty, rows, cols, WAIT, |s| {
            s.lines()
                .last()
                .is_some_and(|status| status.contains("反向搜索"))
        })?;
        pty.send(b"hel")?;
        let found = pty::screen_until(&pty, rows, cols, WAIT, |s| {
            s.lines()
                .last()
                .is_some_and(|status| status.contains("反向搜索：hel "))
        })?;
        assert_eq!(found.lines().nth(editor), Some("> hello"), "{found}");
        assert_eq!(commands()?, sent, "a command from the search");
        // Ctrl+C ends the search (the editor back to its empty draft), Ctrl+C again opens the choice, 脱离.
        pty.send(b"\x03")?;
        pty::screen_until(&pty, rows, cols, WAIT, |s| {
            s.lines().nth(editor) == Some(">")
        })?;
        pty.send(b"\x03")?;
        pty::screen_until(&pty, rows, cols, WAIT, |s| s.contains("> 脱离"))?;
        pty.send(b"\r")?;
        assert_eq!(exit_within(&mut hoh, WAIT)?.code(), Some(0));
        assert_eq!(commands()?, sent, "commands at the second session's end");
        assert_eq!(
            std::fs::read_to_string(world.project().join(".hoh/input-history.jsonl"))?,
            "{\"text\":\"hello\"}\n"
        );
        let state = world.query(&format!("SELECT state FROM runs WHERE run_id = '{run}'"))?;
        assert!(
            !["CANCEL_REQUESTED", "CANCELLED"].contains(&state.as_str()),
            "{state}"
        );
        Ok(())
    })();
    runtime.block_on(host.shutdown());
    outcome?;
    assert!(team.refused().is_empty(), "{team:?}");
    Ok(())
}

/// The developer attempts of `run` per task (task id, rows) and the fake Developer's starts so far.
fn work_done(world: &world::World, run: &str) -> R<(Vec<serde_json::Value>, usize)> {
    Ok((
        world.rows(&format!(
            "SELECT t.task_id, count(*) AS attempts FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.run_id = '{run}' GROUP BY t.task_id ORDER BY t.task_id"
        ))?,
        world.log("calls.jsonl")?.len(),
    ))
}

#[test]
fn test_005_01_a_detached_run_survives_the_hangup_and_attach_sees_what_happened() -> R {
    // AC-005-01: a committed Git project with its specification, no host, HOME and XDG_RUNTIME_DIR of its own; the fake
    // CLIs on its PATH: the pinned Commander (codex) plans a developer task and moves the run to RUNNING, the Developer
    // (claude) works 20 s per attempt and logs each start, QA reports FAIL each round so the work comes back (a budget
    // of 20 iterations). bash in a PTY runs `hoh -p '完成fixture目标' --detach --output json`; the terminal is then hung
    // up (SIGHUP to its foreground process group, its master closed); 60 s later a new PTY attaches the run while one
    // of its developer attempts is in flight, and detaches within it.
    let world = world::World::new()?;
    world.configure(&world::TEAM, "[budget]\nmax_iterations = 20\n")?;
    world.commit_project(&world::fixture_spec())?;
    world.developer(&world::Developer {
        seconds: 20.0,
        ..world::Developer::default()
    })?;
    world.verifier(
        "qa",
        &world::Verifier {
            report: "FAIL".to_owned(),
            checks: vec![world::fixture_spec()[0].criteria[0].check_id()],
            ..world::Verifier::default()
        },
    )?;
    world.register()?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(world.init_store())?;
    world.seed_spec()?;
    assert!(world.hosts()?.is_empty(), "a host before the detach");
    // bash in a PTY runs the detach between two markers its own arithmetic makes (so its echo of the line is neither).
    let mut pty = Pty::new(30, 120)?;
    let mut shell = world.spawn_pty(
        &pty,
        std::ffi::OsStr::new("bash"),
        &["--noprofile", "--norc"],
        true,
    )?;
    let shell_pid = shell.0.id();
    let (shell_sid, _) = world::session_and_tty(shell_pid)?;
    let from = pty.output().len();
    pty.send(
        "echo \"B$((6*7))X\"; s=$EPOCHREALTIME; hoh -p '完成fixture目标' --detach --output json; \
         echo \"E$((6*7))X $? $s $EPOCHREALTIME\"\n"
            .as_bytes(),
    )?;
    let end = std::time::Instant::now() + Duration::from_secs(30);
    let (shown, ended) = loop {
        let out = String::from_utf8_lossy(&pty.output()[from..]).into_owned();
        if let Some(at) = out.find("E42X ")
            && let Some(len) = out[at..].find("\r\n")
        {
            break (out.clone(), out[at + 5..at + len].to_owned());
        }
        if std::time::Instant::now() > end {
            return Err(format!("the detach did not end: {out:?}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let start = shown.find("B42X\r\n").ok_or("no start marker")? + "B42X\r\n".len();
    let stdout = &shown[start..shown.find("E42X ").ok_or("no end marker")?];
    let fields: Vec<&str> = ended.split(' ').collect();
    let (code, began, finished): (i32, f64, f64) =
        (fields[0].parse()?, fields[1].parse()?, fields[2].parse()?);
    // Exit 0 within 10 s; stdout exactly one JSON object with run_id, command_id, revision and status ACCEPTED.
    assert_eq!(code, 0, "{stdout:?}");
    assert!(finished - began <= 10.0, "{} s", finished - began);
    let accepted: serde_json::Value = serde_json::from_str(stdout.trim())?;
    assert!(accepted.is_object(), "{stdout:?}");
    for key in ["run_id", "command_id", "revision"] {
        assert!(accepted.get(key).is_some(), "{key}: {accepted}");
    }
    assert_eq!(accepted["status"], "ACCEPTED", "{accepted}");
    let run = accepted["run_id"].as_str().ok_or("no run_id")?.to_owned();
    // The host it started: its pid, session and terminal, and the events' latest seq S0.
    let info: serde_json::Value =
        serde_json::from_slice(&std::fs::read(world.project().join(".hoh/host.json"))?)?;
    let host = u32::try_from(info["pid"].as_u64().ok_or("no host pid")?)?;
    let host_start = hoh_agents::identity::start_id(host)?.ok_or("the host is gone")?;
    let s0: i64 = world.query("SELECT max(seq) FROM events")?.parse()?;
    // SIGHUP to the terminal's foreground process group, then its master closed.
    let foreground = i32::try_from(pty.foreground()?)?;
    rustix::process::kill_process_group(
        rustix::process::Pid::from_raw(foreground).ok_or("no group")?,
        Signal::HUP,
    )?;
    pty.hang_up()?;
    let _ = exit_within(&mut shell, WAIT);
    std::thread::sleep(Duration::from_secs(60));
    // The host lives on: no terminal (`ps` shows `?`), a session not the shell's; the run went on (seq past S0).
    assert!(
        !hoh_agents::identity::ended(host, Some(&host_start))?,
        "the host is gone"
    );
    let ps = std::process::Command::new("ps")
        .args(["-o", "tty=", "-p", &host.to_string()])
        .output()?;
    assert_eq!(String::from_utf8(ps.stdout)?.trim(), "?");
    let (host_sid, _) = world::session_and_tty(host)?;
    assert_ne!(host_sid, shell_sid);
    let latest: i64 = world.query("SELECT max(seq) FROM events")?.parse()?;
    assert!(latest > s0, "{latest} <= {s0}");
    // A developer attempt that has just started: attach and detach within its 20 s.
    let (_, calls) = work_done(&world, &run)?;
    let end = std::time::Instant::now() + Duration::from_secs(60);
    while work_done(&world, &run)?.1 == calls {
        if std::time::Instant::now() > end {
            return Err("no developer attempt started within 60 s".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let working = world.query(&format!(
        "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = '{run}' \
         AND t.role = 'developer' AND a.state = 'RUNNING'"
    ))?;
    assert!(!working.is_empty(), "no developer attempt in flight");
    let before = work_done(&world, &run)?;
    let watermark: i64 = world
        .query(&format!(
            "SELECT max(seq) FROM events WHERE run_id = '{run}'"
        ))?
        .parse()?;
    let (rows, cols) = (30, 120);
    let apty = Pty::new(rows, cols)?;
    // The seqs attach receives: its follower appends each to the debug build's trace (HOH_DEBUG_TUI_SEQS, docs/adr/
    // ADR-031.md S5), outside the project.
    let seqs = world.root().join("attach-received-seqs");
    let mut attach = world.spawn_pty_with(
        &apty,
        std::ffi::OsStr::new(world::HOH),
        &["attach", &run],
        true,
        &[("HOH_DEBUG_TUI_SEQS", seqs.as_os_str())],
    )?;
    // Its status bar: `事件 N · #S`, S at least the run's latest seq at the attach.
    let counted = |s: &str| -> Option<(i64, i64)> {
        let status = s.lines().last()?;
        let rest = status.split("事件 ").nth(1)?;
        let (n, rest) = rest.split_once(" · #")?;
        let s = rest.split(' ').next()?;
        Some((n.parse().ok()?, s.parse().ok()?))
    };
    let screen = pty::screen_until(&apty, rows, cols, Duration::from_secs(15), |s| {
        counted(s).is_some_and(|(_, seq)| seq >= watermark)
    })?;
    let (n, s) = counted(&screen).ok_or("no count")?;
    let stored: i64 = world
        .query(&format!(
            "SELECT count(*) FROM events WHERE run_id = '{run}' AND seq <= {s}"
        ))?
        .parse()?;
    assert_eq!(n, stored, "{screen}");
    assert!(
        s >= watermark && watermark > s0,
        "S {s}, watermark {watermark}, S0 {s0}"
    );
    // Detach: Ctrl+C, 脱离.
    let mut apty = apty;
    apty.send(b"\x03")?;
    pty::screen_until(&apty, rows, cols, WAIT, |s| s.contains("> 脱离"))?;
    apty.send(b"\r")?;
    assert_eq!(exit_within(&mut attach, WAIT)?.code(), Some(0));
    // What attach received: strictly increasing, every one of the run's events in (S0, watermark] among them, none
    // that is not one of the run's events; and its status bar counted exactly those up to its S.
    let received: Vec<i64> = std::fs::read_to_string(&seqs)
        .map_err(|e| format!("no trace of the seqs attach received: {e}"))?
        .lines()
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    assert!(
        received.windows(2).all(|w| w[0] < w[1]),
        "not strictly increasing: {received:?}"
    );
    let of_run = |range: &str| -> R<Vec<i64>> {
        let listed = world.query(&format!(
            "SELECT group_concat(seq, ' ') FROM (SELECT seq FROM events WHERE run_id = '{run}' {range} ORDER BY seq)"
        ))?;
        Ok(listed
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?)
    };
    let after_s0 = of_run(&format!("AND seq > {s0} AND seq <= {watermark}"))?;
    assert!(
        !after_s0.is_empty(),
        "no event of the run in (S0, watermark]"
    );
    let missed: Vec<&i64> = after_s0.iter().filter(|q| !received.contains(q)).collect();
    assert!(
        missed.is_empty(),
        "attach did not receive {missed:?} of (S0 {s0}, watermark {watermark}]: {received:?}"
    );
    let all = of_run("")?;
    let foreign: Vec<&i64> = received.iter().filter(|q| !all.contains(q)).collect();
    assert!(foreign.is_empty(), "not the run's events: {foreign:?}");
    assert!(received.contains(&s), "#{s} was not received: {received:?}");
    assert_eq!(
        usize::try_from(n)?,
        received.iter().filter(|q| **q <= s).count(),
        "{screen}"
    );
    // All within the same attempt, still in flight: no attempt row and no Developer start more or fewer.
    assert_eq!(
        world.query(&format!(
            "SELECT state FROM attempts WHERE attempt_id = '{working}'"
        ))?,
        "RUNNING",
        "the attempt ended during the attach"
    );
    assert_eq!(work_done(&world, &run)?, before);
    Ok(())
}

#[test]
fn test_047_02_tui_diff_and_restore_default() -> R {
    let world = world::World::named("m3t03-settings")?;
    world.register()?;
    world.commit_project(&world::fixture_spec())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(world.init_store())?;
    let spec = world.seed_spec()?;
    let project = world.project();
    let file = project.join(".hoh/prompts/developer.md");
    std::fs::create_dir_all(file.parent().ok_or("parent")?)?;
    std::fs::write(&file, "CUSTOM SETTINGS CONTRACT\n")?;
    let resources = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/prompts");
    let before = tree(&resources)?;
    let host = runtime.block_on(world.start_host())?;
    let outcome = (|| -> R {
        let mut pty = Pty::new(40, 120)?;
        let mut child = world.hoh_pty(&pty, &[])?;
        pty::screen_until(&pty, 40, 120, WAIT, |s| s.contains("host 已连接"))?;
        pty.send(b"/settings developer\r")?;
        let shown = pty::screen_until(&pty, 40, 120, WAIT, |s| s.contains("--- builtin"))?;
        assert!(shown.contains("+++ project"), "{shown}");
        assert!(shown.contains("@@ -1,"), "{shown}");
        pty.send(b"r")?;
        pty::screen_until(&pty, 40, 120, WAIT, |s| s.contains("已恢复默认"))?;
        assert!(!file.exists());
        assert_eq!(
            tree(&resources)?,
            before,
            "installed-source resources are immutable"
        );
        signal(&child, Signal::TERM)?;
        assert_eq!(exit_within(&mut child, WAIT)?.code(), Some(143));
        pty.drained(WAIT)?;
        let run = runtime.block_on(world::start_run(&host, "settings", "settings restore"))?;
        world.exec(&format!("UPDATE runs SET state = 'RUNNING' WHERE run_id = '{run}';
          INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate)
          VALUES ('T-settings', '{run}', '{spec}', 'READY', 1, 'developer', 0, 1, 1);"))?;
        world.wait("SELECT count(*) FROM attempts a JOIN prompt_revisions p ON p.revision_id = a.prompt_revision WHERE a.task_id = 'T-settings' AND p.layer = 'builtin'", "1", Duration::from_secs(45))?;
        assert_eq!(tree(&resources)?, before);
        Ok(())
    })();
    runtime.block_on(host.shutdown());
    outcome
}

#[test]
fn test_007_01_slash_and_shift_tab_submit_one_setmode_shape() -> R {
    use serde_json::json;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    for (name, cycle, initial, expected) in [
        ("mode-default", None, "AUTO", vec!["PLAN", "BUILD", "AUTO"]),
        (
            "mode-two",
            Some("[ui]\nmode_cycle = [\"PLAN\", \"AUTO\"]"),
            "PLAN",
            vec!["AUTO", "PLAN"],
        ),
    ] {
        let world = world::World::named(name)?;
        world.configure(&[], cycle.unwrap_or(""))?;
        let host = runtime.block_on(world.start_plain_host(None))?;
        let body = json!({"schema_version":"1.0", "command_id":"start-mode", "idempotency_key":"start-mode",
            "expected_revision":0, "kind":"StartRun", "run_id":null, "payload":{"goal":"mode", "mode":initial, "interactive":true}});
        let (status, accepted) =
            runtime.block_on(world::exchange(&host, "POST", "/v1/commands", Some(&body)))?;
        assert_eq!(status, 202, "{accepted}");
        let run = accepted["run_id"].as_str().ok_or("run_id")?;
        let (rows, cols) = (32, 150);
        let mut pty = Pty::new(rows, cols)?;
        let before = pty.stty()?;
        let mut child = world.hoh_pty(&pty, &["attach", run])?;
        let screen = pty::screen_until(&pty, rows, cols, WAIT, |s| {
            s.lines().next().is_some_and(|top| top.contains(run))
        })?;
        assert!(
            screen
                .lines()
                .next()
                .unwrap_or_default()
                .contains(&format!("模式 {initial}")),
            "{screen}"
        );
        for (i, want) in expected.iter().enumerate() {
            pty.send(if cycle.is_none() && i == 0 {
                b"/plan\r"
            } else {
                b"\x1b[Z"
            })?;
            let marker = format!("sync{i}");
            pty.send(marker.as_bytes())?;
            let screen = pty::screen_until(&pty, rows, cols, WAIT, |s| {
                s.lines().any(|line| line == format!("> {marker}"))
            })?;
            assert_eq!(
                world.query("SELECT count(*) FROM commands WHERE kind = 'SetMode'")?,
                (i + 1).to_string(),
                "{screen}"
            );
            assert_eq!(world.query("SELECT mode FROM runs")?, *want, "{screen}");
            let screen = pty::screen_until(&pty, rows, cols, WAIT, |s| {
                s.lines()
                    .next()
                    .is_some_and(|top| top.contains(&format!("模式 {want}")))
            })?;
            if let Some(evidence) = std::env::var_os("HOH_EVIDENCE_DIR") {
                std::fs::write(
                    Path::new(&evidence).join(format!("test-007-01-{name}-{i}.txt")),
                    screen.as_bytes(),
                )?;
            }
            pty.send(b"\x03")?;
        }
        if cycle.is_none() {
            let revision: i64 = world.query("SELECT revision FROM runs")?.parse()?;
            let body = json!({"schema_version":"1.0", "command_id":"http-plan", "idempotency_key":"http-plan",
                "expected_revision":revision, "kind":"SetMode", "run_id":run,"payload":{"mode":"PLAN"}});
            assert_eq!(
                runtime
                    .block_on(world::exchange(&host, "POST", "/v1/commands", Some(&body)))?
                    .0,
                202
            );
            pty::screen_until(&pty, rows, cols, WAIT, |s| {
                s.lines()
                    .next()
                    .is_some_and(|top| top.contains("模式 PLAN"))
            })?;
            let commands = world
                .rows("SELECT payload_json FROM commands WHERE kind = 'SetMode' ORDER BY rowid")?;
            assert_eq!(commands.len(), 4);
            assert_eq!(commands[0]["payload_json"], commands[3]["payload_json"]);
        }
        let transitions = world.rows("SELECT json_extract(payload_json,'$.from') AS from_mode, json_extract(payload_json,'$.to') AS to_mode FROM events WHERE kind = 'run.mode_changed' ORDER BY seq")?;
        let wanted = if cycle.is_none() {
            vec![
                ("AUTO", "PLAN"),
                ("PLAN", "BUILD"),
                ("BUILD", "AUTO"),
                ("AUTO", "PLAN"),
            ]
        } else {
            vec![("PLAN", "AUTO"), ("AUTO", "PLAN")]
        };
        assert_eq!(
            transitions,
            wanted
                .into_iter()
                .map(|(from, to)| json!({"from_mode":from,"to_mode":to}))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            world.query("SELECT count(*) FROM input_queue")?,
            "0",
            "mode commands never enter agent input"
        );
        signal(&child, Signal::TERM)?;
        assert_eq!(exit_within(&mut child, WAIT)?.code(), Some(143));
        pty.drained(WAIT)?;
        assert_eq!(pty.stty()?, before);
        runtime.block_on(host.shutdown());
    }
    Ok(())
}

#[test]
fn test_008_03_plan_question_is_shown_and_explicit_answer_restores_specifying() -> R {
    use serde_json::json;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let world = world::World::named("m3t04-question")?;
    world.configure(&[], "")?;
    world.commit_project(&[world::Requirement {
        id: "REQ-A".into(),
        title: "待规划需求".into(),
        criteria: Vec::new(),
    }])?;
    let team = world::Team::new(|turn| {
        if turn.state != "CREATED" {
            return Vec::new();
        }
        let mut steps = world::steps("CREATED", "SPECIFYING");
        let mut ask = world::ask("release-choice", "请选择发布方式");
        ask["choices"] = json!([
            {"choice_id":"first","label":"第一种方式"},
            {"choice_id":"second","label":"第二种方式"}]);
        steps.push(ask);
        steps
    });
    let host = runtime.block_on(world.team_host(team, None))?;
    let outcome = (|| -> R {
        world.seed_spec()?;
        let body = json!({"schema_version":"1.0", "command_id":"plan-question", "idempotency_key":"plan-question",
            "expected_revision":0, "kind":"StartRun", "run_id":null,
            "payload":{"goal":"规划发布", "mode":"PLAN", "interactive":true}});
        let (status, accepted) =
            runtime.block_on(world::exchange(&host, "POST", "/v1/commands", Some(&body)))?;
        assert_eq!(status, 202, "{accepted}");
        let run = accepted["run_id"].as_str().ok_or("run_id")?;
        world.staff(run)?;
        world.wait("SELECT count(*) FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.question')='请选择发布方式'", "1", WAIT)?;
        let mut pty = Pty::new(30, 120)?;
        let mut child = world.hoh_pty(&pty, &["attach", run])?;
        let shown = pty::screen_until(&pty, 30, 120, WAIT, |s| {
            s.contains("请选择发布方式") && s.contains("第二种方式")
        })?;
        assert!(shown.contains("第一种方式"), "{shown}");
        if let Some(evidence) = std::env::var_os("HOH_EVIDENCE_DIR") {
            std::fs::write(Path::new(&evidence).join("test-008-03-question.txt"), shown)?;
        }
        // The acceptance scenario explicitly requires a ten-second unanswered interval.
        std::thread::sleep(Duration::from_secs(10));
        assert_eq!(
            world.query("SELECT state||'/'||blocked_reason||'/'||resume_state FROM runs")?,
            "BLOCKED/NEED_DECISION/SPECIFYING"
        );
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='question.answered'")?,
            "0"
        );
        let question = world.query("SELECT json_extract(payload_json,'$.question_id') FROM events WHERE kind='question.asked' ORDER BY seq DESC LIMIT 1")?;
        pty.send(b"\t\x1b[B\r")?;
        world.wait(
            "SELECT count(*) FROM events WHERE kind='question.answered'",
            "1",
            WAIT,
        )?;
        assert_eq!(world.query("SELECT state FROM runs")?, "SPECIFYING");
        let answers =
            world.rows("SELECT payload_json FROM events WHERE kind='question.answered'")?;
        let answer: serde_json::Value =
            serde_json::from_str(answers[0]["payload_json"].as_str().ok_or("payload")?)?;
        assert_eq!(answer["question_id"], question);
        assert_eq!(answer["choice_id"], "second");
        assert_eq!(
            world.query("SELECT count(*) FROM commands WHERE kind='AnswerQuestion'")?,
            "1"
        );
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='decision.applied' AND json_extract(payload_json,'$.kind')='record_assumption'")?, "0");
        signal(&child, Signal::TERM)?;
        assert_eq!(exit_within(&mut child, WAIT)?.code(), Some(143));
        Ok(())
    })();
    runtime.block_on(host.shutdown());
    outcome
}

#[test]
fn test_008_01_running_plan_pause_resume_approval_review() -> R {
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering::SeqCst},
    };
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
        }
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let world = world::World::named("m3t04-running-plan")?;
    world.configure(&[], "")?;
    world.commit_project(&world::fixture_spec())?;
    runtime.block_on(world.init_store())?;
    let spec = world.seed_spec()?;
    let indexed: serde_json::Value = serde_json::from_slice(&std::fs::read(
        world.project().join("docs/spec/index.json"),
    )?)?;
    let ac = indexed["requirements"][0]["acceptance"][0]
        .to_string()
        .replace('\'', "''");
    world.exec(&format!(
        "UPDATE acceptance_criteria SET body='{ac}' WHERE spec_id='{spec}';"
    ))?;
    let setup = runtime.block_on(world.start_plain_host(None))?;
    let run = runtime.block_on(world::start_run(&setup, "start", "规划边界"))?;
    world.staff(&run)?;
    world.exec(&format!("UPDATE runs SET state='RUNNING',mode='BUILD' WHERE run_id='{run}'; INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,cost_estimate) VALUES('work','{run}','{spec}','READY','developer',1,1,1); INSERT INTO task_requirements(task_id,requirement_id,spec_id) VALUES('work','REQ-901','{spec}'); INSERT INTO events(project_id,run_id,event_id,kind,payload_json,created_at) VALUES(:p,'{run}','s7-plan','plan.imported',json_object('run_id','{run}','tasks',json_array(json_object('task_id','work','paths',json_array('src/lib.rs')))),'2026-11-23T00:00:00Z');"))?;
    runtime.block_on(setup.shutdown());
    let source = hoh_core::specindex::Source {
        path: "docs/spec/fixture.md".into(),
        bytes: std::fs::read_to_string(world.project().join("docs/spec/fixture.md"))?
            .replace("fixture 目标已完成", "fixture 目标与边界已完成")
            .into_bytes(),
    };
    let release = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let drop_release = Release(release.clone());
    let r = release.clone();
    let e = entered.clone();
    let team = world::Team::planning_with_review(
        world.project(),
        source,
        |_| vec![],
        Some(Duration::from_secs(60)),
        Some(("work".into(), world.root().join("writer-release"))),
        move |_| {
            e.store(true, SeqCst);
            while !r.load(SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            world::Verdict::Pass
        },
    );
    let host = runtime.block_on(world.team_host(team, None))?;
    let request = |key: &str, kind: &str, payload: serde_json::Value| -> R {
        for _ in 0..20 {
            let rev: i64 = world
                .query(&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?
                .parse()?;
            let body = json!({"schema_version":"1.0","command_id":key,"idempotency_key":key,"expected_revision":rev,"kind":kind,"run_id":run,"payload":payload});
            let (status, reply) =
                runtime.block_on(world::exchange(&host, "POST", "/v1/commands", Some(&body)))?;
            if status == 409 && reply["code"] == "STALE_REVISION" {
                continue;
            }
            assert_eq!(status, 202, "{reply}");
            return Ok(());
        }
        Err("stale command".into())
    };
    let result = (|| -> R {
        world.wait(
            "SELECT count(*) FROM attempts WHERE task_id='work' AND state='RUNNING'",
            "1",
            Duration::from_secs(20),
        )?;
        let product = std::fs::read(world.project().join("src/lib.rs"))?;
        let mut pty = Pty::new(34, 140)?;
        let mut child = world.hoh_pty(&pty, &["attach", &run])?;
        pty::screen_until(&pty, 34, 140, WAIT, |s| s.contains("模式 BUILD"))?;
        pty.send(b"/plan\r")?;
        world.wait(
            "SELECT count(*) FROM events WHERE kind='run.mode_change_pending'",
            "1",
            WAIT,
        )?;
        assert_eq!(
            world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft'")?,
            "0"
        );
        request("pause", "PauseRun", json!({"reason":"规划暂停"}))?;
        std::fs::write(
            world.root().join("writer-release"),
            "release this fixture writer",
        )?;
        world.wait("SELECT state FROM runs", "PAUSED", Duration::from_secs(20))?;
        assert_eq!(
            world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft'")?,
            "0"
        );
        request("resume", "ResumeRun", json!({"reason":"继续规划"}))?;
        world.wait(
            "SELECT count(*) FROM events WHERE kind='spec.draft_completed'",
            "1",
            Duration::from_secs(25),
        )?;
        assert_eq!(
            world.query("SELECT state||'/'||resume_state FROM runs")?,
            "BLOCKED/RUNNING"
        );
        assert_eq!(
            world.query("SELECT count(*) FROM change_requests WHERE state='PROPOSED'")?,
            "1"
        );
        pty::screen_until(&pty, 34, 140, WAIT, |s| s.contains("批准规格"))?;
        pty.send(b"\t\r")?;
        if let Err(e) = world.wait("SELECT count(*) FROM specs WHERE revision=2", "1", WAIT) {
            let shown = pty::screen_until(&pty, 34, 140, WAIT, |_| true)?;
            return Err(format!("{e}; screen={shown}; commands={:?}; questions={:?}",world.rows("SELECT kind,payload_json FROM commands")?,world.rows("SELECT kind,payload_json FROM events WHERE kind LIKE 'question.%' OR kind='input.rejected'")?).into());
        }
        let end = std::time::Instant::now() + Duration::from_secs(20);
        while !entered.load(SeqCst) {
            if std::time::Instant::now() > end {
                return Err("review not dispatched after approval in RUNNING".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        request("build", "SetMode", json!({"mode":"BUILD"}))?;
        let spec2 = world.query("SELECT spec_id FROM specs WHERE revision=2")?;
        world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,cost_estimate) VALUES('new-work','{run}','{spec2}','READY','developer',1,0,1); INSERT INTO task_requirements(task_id,requirement_id,spec_id) VALUES('new-work','REQ-901','{spec2}'); INSERT INTO events(project_id,run_id,event_id,kind,payload_json,created_at) VALUES(:p,'{run}','s7-new-plan','plan.imported',json_object('run_id','{run}','tasks',json_array(json_object('task_id','new-work','paths',json_array('src/lib.rs')))),'2026-11-23T00:00:00Z');"))?;
        world.wait(
            "SELECT blocked_reason FROM tasks WHERE task_id='new-work'",
            "READINESS_NOT_MET",
            WAIT,
        )?;
        assert_eq!(
            world.query("SELECT count(*) FROM attempts WHERE task_id='new-work'")?,
            "0"
        );
        release.store(true, SeqCst);
        world.wait("SELECT count(*) FROM events WHERE kind='spec_review.reported' AND json_extract(payload_json,'$.spec_revision')=2 AND json_extract(payload_json,'$.state')='CURRENT'","1",Duration::from_secs(20))?;
        world.wait(
            "SELECT count(*) FROM attempts WHERE task_id='new-work' AND state='RUNNING'",
            "1",
            Duration::from_secs(20),
        )?;
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='run.transitioned' AND json_extract(payload_json,'$.to')='SPECIFYING'")?,"0");
        assert_eq!(std::fs::read(world.project().join("src/lib.rs"))?, product);
        assert_eq!(
            world
                .git(&["diff", "--name-only", "HEAD~1", "HEAD", "--", "src"])?
                .trim(),
            ""
        );
        request("cancel", "CancelRun", json!({"reason":"fixture done"}))?;
        signal(&child, Signal::TERM)?;
        assert_eq!(exit_within(&mut child, WAIT)?.code(), Some(143));
        Ok(())
    })();
    drop(drop_release);
    runtime.block_on(host.shutdown());
    result
}
