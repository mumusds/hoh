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
/// submitted one by one (M1T08R-20). The gap starts once hoh is known to have read the first part, so it is waited
/// for (M1T08R-30, -33).
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

/// A long paste is drawn at once: 128 KiB in the single-line editor shows its end within seconds (it took minutes
/// when the view was found by dropping graphemes from the front one by one, all that time without looking at the
/// signal flags; M1T08R-31).
#[test]
fn a_long_paste_is_drawn_at_once() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
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
            });
        });
        Ok(hoh_app::host::Started { pid, gate, ended })
    }
}

/// Whether process `pid` is alive (signal 0).
fn alive(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
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
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
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
    let host_pid = std::process::id();
    let run = || query("SELECT state || ' ' || revision FROM runs WHERE run_id = 'R'");
    let before = run()?;
    let unchanged = |step: &str| -> R {
        assert!(
            alive(host_pid) && alive(agent),
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
    if let Some(pgid) = i32::try_from(agent)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
    }
    Ok(())
}

/// The `hoh __host <project>` processes of `project`, by their command lines.
fn hosts_of(project: &Path) -> Vec<u32> {
    let wanted = format!(
        "{}\0__host\0{}\0",
        env!("CARGO_BIN_EXE_hoh"),
        project.display()
    );
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c == wanted.as_bytes())
        })
        .collect()
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
        for pid in hosts_of(&self.0) {
            signal(pid, Signal::TERM);
        }
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while !hosts_of(&self.0).is_empty() && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        for pid in hosts_of(&self.0) {
            signal(pid, Signal::KILL);
        }
    }
}

#[test]
fn a_host_started_by_a_detach_in_a_terminal_outlives_its_hang_up() -> R {
    // CR-010 decision 2 (REQ-005 behaviour 2): supplementary detach evidence, no TEST id (AC-005-01 is M2-T13's).
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
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
    assert!(alive(pid), "the host is gone after the hang-up");
    assert_eq!((session, tty), (pid, 0), "its own session, no terminal");
    if let Some(p) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        rustix::process::kill_process(p, rustix::process::Signal::TERM)?;
    }
    let end = std::time::Instant::now() + WAIT;
    while alive(pid) && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid), "the host did not stop on SIGTERM");
    Ok(())
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
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
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
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
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
