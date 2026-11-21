//! The TUI shell's editor, history, keys and screen (ADR-014), without a terminal.

use super::app::{App, Exit, Request, mode_of};
use super::editor::Editor;
use super::history::History;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        assert_eq!(app.key(key(KeyCode::Char(c))), None);
    }
}

/// The screen as text, one string per row.
fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal.draw(|f| app.draw(f)).expect("draw");
    let buffer = terminal.backend().buffer();
    // A wide character fills two cells; the second is a placeholder and is skipped.
    (0..height)
        .map(|y| {
            let mut row = String::new();
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                row.push_str(symbol);
                x += u16::try_from(symbol.width().max(1)).unwrap_or(1);
            }
            row
        })
        .collect()
}

fn app_in(dir: &std::path::Path) -> App {
    App::new("demo".to_owned(), History::load(dir))
}

#[test]
fn the_editor_moves_by_grapheme_and_measures_display_width() {
    let mut e = Editor::default();
    e.insert("a中e\u{301}b");
    assert_eq!(e.text(), "a中e\u{301}b");
    assert_eq!(e.cursor_column(), 5, "a=1, 中=2, e+combining=1, b=1");
    e.left();
    e.left();
    assert_eq!(
        e.cursor_column(),
        3,
        "the combining mark moves with its letter"
    );
    e.backspace();
    assert_eq!(e.text(), "ae\u{301}b");
    e.home();
    assert_eq!(e.cursor_column(), 0);
    e.delete();
    assert_eq!(e.text(), "e\u{301}b");
    e.end();
    e.insert("x");
    assert_eq!(e.text(), "e\u{301}bx");
    assert_eq!(e.take(), "e\u{301}bx");
    assert!(e.is_empty());
    assert_eq!(e.cursor_column(), 0);
}

#[test]
fn pasted_line_breaks_become_spaces_in_the_single_line_editor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.paste("one\r\ntwo\nthree\rfour");
    assert_eq!(app.editor_text(), "one two three four");
}

#[test]
fn history_lives_in_the_project_and_survives_bad_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let empty = History::load(dir.path());
    assert!(empty.entries().is_empty());
    assert!(empty.notice().is_none());
    assert!(!dir.path().join(".hoh").exists(), "loading creates nothing");
    let mut h = History::load(dir.path());
    h.append("hello").expect("append");
    h.append("说 \"hi\"\n").expect("append");
    let file = dir.path().join(".hoh/input-history.jsonl");
    // The `.hoh/` it made is kept out of git (docs/adr/ADR-027.md decision 3).
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".hoh/.gitignore")).expect("read"),
        hoh_core::state_dir::IGNORE_ALL
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "{\"text\":\"hello\"}\n{\"text\":\"说 \\\"hi\\\"\\n\"}\n"
    );
    let mut bytes = std::fs::read(&file).expect("read");
    bytes.extend_from_slice(b"not json\n{\"other\":1}\n\xff\n{\"text\":\"last\"}\n");
    std::fs::write(&file, bytes).expect("write");
    let again = History::load(dir.path());
    assert_eq!(again.entries(), ["hello", "说 \"hi\"\n", "last"]);
    assert!(
        again.notice().is_some_and(|n| n.contains('3')),
        "three lines skipped: {:?}",
        again.notice()
    );
}

#[test]
fn an_unreadable_history_is_reported_and_the_shell_still_opens() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".hoh/input-history.jsonl"))
        .expect("a directory in its place");
    let h = History::load(dir.path());
    assert!(h.entries().is_empty());
    assert!(h.notice().is_some());
    let app = App::new("demo".to_owned(), h);
    assert!(
        screen(&app, 100, 30)
            .iter()
            .any(|row| row.contains("输入历史"))
    );
}

#[test]
fn a_history_mode_warning_is_visible_and_does_not_prevent_input() {
    use std::os::unix::fs::PermissionsExt as _;
    assert!(
        !rustix::process::geteuid().is_root(),
        "file modes require a non-root test user"
    );
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join(".hoh");
    std::fs::create_dir(&dir).expect("state dir");
    std::fs::write(dir.join(".gitignore"), "*\n").expect("ignore");
    std::fs::write(dir.join("input-history.jsonl"), "not json\n").expect("history");
    let mut app = app_in(root.path());
    // Write/search are allowed, but the no-follow read descriptor for mode tightening is refused. Real I/O,
    // no chmod privilege or global fault switch: appending to the existing history file can still succeed.
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let restore = Restore(dir.clone());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o333)).expect("mode");
    typed(&mut app, "first");
    app.key(key(KeyCode::Enter));
    typed(&mut app, "second");
    app.key(key(KeyCode::Enter));
    drop(restore);
    assert_eq!(History::load(root.path()).entries(), ["first", "second"]);
    let rows = screen(&app, 180, 30).join("\n");
    assert!(rows.contains("权限未能收紧"), "{rows}");
    assert!(
        rows.contains("无法解析"),
        "the earlier history notice remains: {rows}"
    );
    assert!(!rows.contains("输入历史未能保存"), "{rows}");
}

#[test]
fn enter_records_the_input_and_asks_to_start_a_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    assert_eq!(app.key(key(KeyCode::Enter)), None);
    assert!(
        !dir.path().join(".hoh").exists(),
        "an empty Enter records nothing"
    );
    assert_eq!(app.take_requests(), [], "and asks for nothing");
    typed(&mut app, "hi");
    assert_eq!(app.key(key(KeyCode::Enter)), None);
    assert_eq!(app.editor_text(), "");
    assert_eq!(History::load(dir.path()).entries(), ["hi"]);
    let rows = screen(&app, 100, 30);
    assert!(rows.iter().any(|r| r.contains("你：hi")), "{rows:#?}");
    // Unattached, the text is the goal of a new run (docs/adr/ADR-031.md decision 13).
    assert_eq!(app.take_requests(), [Request::Start("hi".to_owned())]);
}

#[test]
fn up_and_down_browse_the_history_and_restore_the_draft() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut h = History::load(dir.path());
    h.append("first").expect("append");
    h.append("second").expect("append");
    let mut app = App::new("demo".to_owned(), h);
    typed(&mut app, "draft");
    app.key(key(KeyCode::Up));
    assert_eq!(app.editor_text(), "second");
    app.key(key(KeyCode::Up));
    assert_eq!(app.editor_text(), "first");
    app.key(key(KeyCode::Up));
    assert_eq!(app.editor_text(), "first", "the oldest stays");
    app.key(key(KeyCode::Down));
    assert_eq!(app.editor_text(), "second");
    app.key(key(KeyCode::Down));
    assert_eq!(app.editor_text(), "draft");
}

#[test]
fn ctrl_r_fills_the_editor_and_neither_enter_nor_esc_sends() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut h = History::load(dir.path());
    for line in ["hello", "other", "help me"] {
        h.append(line).expect("append");
    }
    let mut app = App::new("demo".to_owned(), h);
    typed(&mut app, "draft");
    app.key(ctrl('r'));
    typed(&mut app, "hel");
    assert_eq!(app.editor_text(), "help me", "the newest match first");
    app.key(ctrl('r'));
    assert_eq!(app.editor_text(), "hello", "Ctrl+R again: an older one");
    app.key(ctrl('r'));
    assert_eq!(app.editor_text(), "hello", "no older match: it stays");
    assert!(screen(&app, 100, 30).iter().any(|r| r.contains("反向搜索")));
    app.key(key(KeyCode::Enter));
    assert_eq!(app.editor_text(), "hello", "Enter ends the search");
    assert_eq!(
        History::load(dir.path()).entries().len(),
        3,
        "and sends nothing"
    );
    assert_eq!(
        app.take_requests(),
        [],
        "Enter that ends the search sends nothing"
    );
    assert!(!screen(&app, 100, 30).iter().any(|r| r.contains("你：")));
    // Esc gives back the draft from before the search.
    app.key(ctrl('c'));
    typed(&mut app, "mine");
    app.key(ctrl('r'));
    typed(&mut app, "oth");
    assert_eq!(app.editor_text(), "other");
    app.key(key(KeyCode::Backspace));
    assert_eq!(app.editor_text(), "other", "a shorter query still matches");
    app.key(key(KeyCode::Esc));
    assert_eq!(app.editor_text(), "mine");
}

#[test]
fn ctrl_c_clears_first_and_then_offers_to_detach() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    typed(&mut app, "x");
    assert_eq!(app.key(ctrl('c')), None);
    assert_eq!(app.editor_text(), "");
    assert!(!screen(&app, 100, 30).iter().any(|r| r.contains("脱离")));
    assert_eq!(app.key(ctrl('c')), None);
    let rows = screen(&app, 100, 30);
    assert!(rows.iter().any(|r| r.contains("脱离")), "{rows:#?}");
    assert!(
        !rows
            .iter()
            .any(|r| r.contains("暂停") || r.contains("取消任务")),
        "no run to pause or cancel"
    );
    assert_eq!(app.key(key(KeyCode::Esc)), None);
    assert!(!screen(&app, 100, 30).iter().any(|r| r.contains("脱离")));
    app.key(ctrl('c'));
    assert_eq!(app.key(key(KeyCode::Enter)), Some(Exit::Detach));
}

#[test]
fn the_workspace_shows_the_project_the_mode_and_a_connected_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path());
    let rows = screen(&app, 100, 30);
    let top = &rows[0];
    for part in ["demo", "AUTO", "run 无", "baseline 无", "host 已连接"] {
        assert!(top.contains(part), "{part:?} in {top:?}");
    }
    // Narrow terminals still show the editor and the status line.
    let rows = screen(&app, 40, 8);
    assert!(rows.iter().any(|r| r.contains("host 已连接")));
}

/// A line wider than the editor shows its end, with the cursor right after it (whole graphemes dropped in front).
#[test]
fn a_long_input_keeps_its_end_and_the_cursor_on_screen() {
    for (long, tail) in [
        (format!("{}END", "a".repeat(100)), "END"),
        (format!("{}尾", "中".repeat(30)), "尾"),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_in(dir.path());
        app.paste(&long);
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).expect("terminal");
        terminal.draw(|f| app.draw(f)).expect("draw");
        let cursor = terminal.get_cursor_position().expect("cursor");
        let rows = screen(&app, 40, 10);
        let row = rows[usize::from(cursor.y)].trim_end().to_owned();
        assert!(row.ends_with(tail), "{row:?}");
        assert_eq!(
            usize::from(cursor.x),
            row.width(),
            "the cursor right after the text: {row:?}"
        );
        assert!(cursor.x < 40);
    }
}

/// An insert that joins the graphemes around the cursor (a zero-width joiner between two emoji, M1T08R-03) leaves
/// the cursor on a boundary of the whole text, so moving, deleting and drawing in a narrow editor stay safe.
#[test]
fn an_edit_that_joins_graphemes_keeps_the_cursor_on_a_boundary() {
    let mut e = Editor::default();
    e.insert("👩👩");
    e.left();
    e.insert("\u{200d}");
    assert_eq!(e.text(), "👩\u{200d}👩");
    assert_eq!(
        e.cursor_column(),
        e.text().width(),
        "the cursor is after the joined grapheme"
    );
    for width in 0..8 {
        let (shown, column) = e.view(width);
        assert!(column <= shown.width());
    }
    e.backspace();
    assert_eq!(e.text(), "", "one grapheme, one backspace");
    // A delete that lets a combining mark join the letter before it: a zero-width space (a control) kept them apart.
    e.insert("e\u{200b}\u{301}");
    e.home();
    e.right();
    e.delete();
    assert_eq!(e.text(), "e\u{301}");
    assert_eq!(e.cursor_column(), 1);
    let (shown, _) = e.view(1);
    assert!(shown.is_empty() || shown == "e\u{301}");
    e.left();
    assert_eq!(e.cursor_column(), 0);
}

/// A history whose last line has no line break (a crash mid-write, an edit by hand) does not swallow the next entry
/// (M1T08R-02).
#[test]
fn an_entry_after_an_unterminated_last_line_stays_its_own_line() {
    for (tail, before) in [
        ("{\"text\":\"a\"}", vec!["a"]),
        ("{\"text\":\"trunc", vec![]),
        ("garbage", vec![]),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join(".hoh/input-history.jsonl");
        std::fs::create_dir_all(file.parent().expect("dir")).expect("dir");
        std::fs::write(&file, tail).expect("write");
        let mut h = History::load(dir.path());
        h.append("b").expect("append");
        let mut want: Vec<&str> = before.clone();
        want.push("b");
        assert_eq!(History::load(dir.path()).entries(), want, "after {tail:?}");
    }
}

/// Restoring undoes the screen, then raw mode even if the screen failed; a raw mode that fails stays marked, its
/// error is returned, and the next call tries it again (M1T08R-01).
#[test]
fn a_restore_step_that_fails_is_reported_and_retried() {
    use super::terminal::undo;
    use std::sync::atomic::{AtomicBool, Ordering};
    let raw = AtomicBool::new(true);
    let order = std::cell::RefCell::new(Vec::new());
    let failed = undo(
        || {
            order.borrow_mut().push("screen");
            Err(std::io::Error::other("screen EIO"))
        },
        &raw,
        || {
            order.borrow_mut().push("raw");
            Err(std::io::Error::other("raw EIO"))
        },
    );
    assert_eq!(
        failed.map_err(|e| e.to_string()),
        Err("screen EIO".to_owned()),
        "the first error"
    );
    assert_eq!(
        *order.borrow(),
        ["screen", "raw"],
        "raw mode is tried although the screen failed"
    );
    assert!(raw.load(Ordering::SeqCst), "still to undo");
    assert!(undo(|| Ok(()), &raw, || Ok(())).is_ok());
    assert!(!raw.load(Ordering::SeqCst));
    assert!(
        undo(
            || Ok(()),
            &raw,
            || panic!("raw mode is not given back twice")
        )
        .is_ok()
    );
}

/// A writer that takes at most `limit` bytes per write and fails once at each byte count in `fail_at`.
struct Flaky {
    out: Vec<u8>,
    limit: usize,
    fail_at: Vec<usize>,
    interrupt_at: Vec<usize>,
}

impl Flaky {
    fn new(limit: usize) -> Flaky {
        Flaky {
            out: Vec::new(),
            limit,
            fail_at: Vec::new(),
            interrupt_at: Vec::new(),
        }
    }
}

impl std::io::Write for Flaky {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.interrupt_at.first() == Some(&self.out.len()) {
            self.interrupt_at.remove(0);
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if self.fail_at.first() == Some(&self.out.len()) {
            self.fail_at.remove(0);
            return Err(std::io::Error::other("EIO"));
        }
        let n = bytes.len().min(self.limit);
        self.out.extend_from_slice(&bytes[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The control sequences a VT-style parser carries out from `bytes` (test oracle): ESC starts a sequence, aborting
/// one under way; CAN aborts; a CSI ends at its final byte (0x40..=0x7e); other bytes in the ground state are text.
/// It models only what the shell sends, CSI sequences and the ground state; OSC and DCS, where terminals differ on
/// CAN, are not modelled and the shell sends none (M1T08R-11).
fn carried_out(bytes: &[u8]) -> Vec<String> {
    enum State {
        Ground,
        Escape,
        Csi(Vec<u8>),
    }
    let mut state = State::Ground;
    let mut done = Vec::new();
    for &b in bytes {
        state = match (state, b) {
            (_, 0x1b) => State::Escape,
            (_, 0x18) => State::Ground,
            (State::Escape, b'[') => State::Csi(Vec::new()),
            (State::Escape, _) | (State::Ground, _) => State::Ground,
            (State::Csi(mut p), b) if (0x40..=0x7e).contains(&b) => {
                p.push(b);
                done.push(String::from_utf8_lossy(&p).into_owned());
                State::Ground
            }
            (State::Csi(mut p), b) => {
                p.push(b);
                State::Csi(p)
            }
        };
    }
    done
}

/// Each restore sequence takes effect exactly once, whatever fails in between: one that went out is never sent again
/// (a second "leave the alternate screen" puts the saved cursor back over what was printed since; M1T08R-07); one cut
/// short is sent again from its start after a CAN, which ends what the terminal was still parsing, so a diagnostic
/// printed in between (after `settle`) neither breaks it nor is swallowed by it (M1T08R-08).
#[test]
fn restore_sequences_take_effect_once_whatever_fails_in_between() {
    use super::terminal::{Steps, close, open, settle_on};
    for (fail_at, label) in [(27, "inside the leave"), (32, "at the start of the cursor")] {
        let steps = Steps::new();
        let mut out = Flaky::new(3);
        open(&mut out, &steps).expect("entered");
        // close: paste off 16..24, leave 24..32, cursor 32..38 in writes of 3 bytes (24, 27, 30, 32, 35, 38).
        out.fail_at = vec![fail_at];
        out.interrupt_at = vec![19];
        assert!(
            close(&mut out, &steps).is_err(),
            "{label}: the first try fails"
        );
        settle_on(&mut out, &steps);
        out.out.extend_from_slice("hoh：终端未能恢复".as_bytes());
        close(&mut out, &steps).expect("the retry finishes");
        assert!(
            out.fail_at.is_empty() && out.interrupt_at.is_empty(),
            "{label}: every fault was hit"
        );
        assert_eq!(
            carried_out(&out.out),
            ["?1049h", "?2004h", "?2004l", "?1049l", "?25h"],
            "{label}: {:?}",
            String::from_utf8_lossy(&out.out)
        );
        // Nothing is left: a further restore sends nothing.
        let before = out.out.len();
        close(&mut out, &steps).expect("nothing to send");
        assert_eq!(out.out.len(), before, "{label}");
    }
}

/// An undo is sent only once what it undoes is out (M1T08R-09): an entry that sent nothing leaves nothing to undo,
/// one cut short is ended with a CAN, and a leave never goes to a terminal that never entered the alternate screen.
#[test]
fn only_what_was_entered_is_undone() {
    use super::terminal::{Steps, close, open};
    // Nothing out: nothing to undo.
    let steps = Steps::new();
    let mut out = Flaky::new(64);
    out.fail_at = vec![0];
    assert!(open(&mut out, &steps).is_err());
    close(&mut out, &steps).expect("closed");
    assert_eq!(out.out, b"", "nothing at all is sent");
    // The alternate screen cut short: ended with a CAN, no leave.
    let steps = Steps::new();
    let mut out = Flaky::new(3);
    out.fail_at = vec![3];
    assert!(open(&mut out, &steps).is_err());
    close(&mut out, &steps).expect("closed");
    assert_eq!(out.out, b"\x1b[?\x18");
    assert!(carried_out(&out.out).is_empty());
    // The screen entered, bracketed paste cut short: the paste sequence ended, the screen left, no paste-off.
    let steps = Steps::new();
    let mut out = Flaky::new(3);
    out.fail_at = vec![11];
    assert!(open(&mut out, &steps).is_err());
    close(&mut out, &steps).expect("closed");
    assert_eq!(
        carried_out(&out.out),
        ["?1049h", "?1049l", "?25h"],
        "{:?}",
        String::from_utf8_lossy(&out.out)
    );
}

/// A normal exit tries the restore a bounded number of times (M1T08R-05).
#[test]
fn a_restore_is_retried_a_bounded_number_of_times() {
    use super::terminal::with_retries;
    let mut tries = 0;
    assert!(
        with_retries(3, || {
            tries += 1;
            if tries < 3 {
                Err(std::io::Error::other("EIO"))
            } else {
                Ok(())
            }
        })
        .is_ok()
    );
    assert_eq!(tries, 3);
    let mut tries = 0;
    assert!(
        with_retries(3, || {
            tries += 1;
            Err(std::io::Error::other("EIO"))
        })
        .is_err()
    );
    assert_eq!(tries, 3);
}

/// The exit code and the message of each way out (ADR-014 decision 3): a restore that failed is 74 with a message
/// unless a signal decided; a signal's code wins over a failing terminal.
#[test]
fn every_way_out_has_its_code_and_a_failed_restore_is_not_0() {
    use super::{End, outcome};
    let eio = || std::io::Error::other("EIO");
    assert_eq!(outcome(Ok(End::Left(Exit::Detach)), Ok(()), 0), (0, None));
    let (code, message) = outcome(Ok(End::Left(Exit::Detach)), Err(eio()), 0);
    assert_eq!(code, 74);
    assert!(message.is_some_and(|m| m.contains("EIO") && m.contains("stty sane")));
    assert_eq!(outcome(Ok(End::Signal(15)), Ok(()), 15), (143, None));
    let (code, message) = outcome(Ok(End::Signal(1)), Err(eio()), 1);
    assert_eq!(code, 129);
    assert!(message.is_some());
    assert_eq!(
        outcome(Err(eio()), Ok(()), 1).0,
        129,
        "a hangup also fails the terminal"
    );
    assert_eq!(outcome(Ok(End::Signal(2)), Ok(()), 2), (130, None));
    let (code, message) = outcome(Err(eio()), Ok(()), 0);
    assert_eq!(code, 74);
    assert!(message.is_some_and(|m| m.contains("EIO")));
    // A terminal that failed on the way in and a restore that failed too: both said, with the way out.
    let (code, message) = outcome(Err(eio()), Err(eio()), 0);
    assert_eq!(code, 74);
    assert!(message.is_some_and(|m| m.contains("stty sane")));
    // A signal that comes while a detach is being restored still decides (M1T08R-06).
    assert_eq!(outcome(Ok(End::Left(Exit::Detach)), Ok(()), 1), (129, None));
    let (code, message) = outcome(Ok(End::Left(Exit::Detach)), Err(eio()), 1);
    assert_eq!(code, 129);
    assert!(message.is_some_and(|m| m.contains("stty sane")));
}

/// Every frame gives ratatui a cursor position, the open choice included, so ratatui never hides the cursor and has
/// nothing to show again when it is dropped. Its drop would write that outside the restore's care and print a failure
/// straight to stderr (M1T08R-10). The restore shows the cursor in any case.
#[test]
fn the_cursor_is_never_hidden_by_ratatui() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("terminal");
    terminal.draw(|f| app.draw(f)).expect("draw");
    assert!(terminal.backend().cursor_visible());
    app.key(ctrl('c'));
    terminal.draw(|f| app.draw(f)).expect("draw");
    assert!(terminal.backend().cursor_visible(), "with the choice open");
    let rows = screen(&app, 60, 12);
    let at = terminal.get_cursor_position().expect("cursor");
    assert!(
        rows[usize::from(at.y)].contains("脱离"),
        "on the selected choice: {at:?}"
    );
}

/// A frame cut short leaves the terminal inside one of its sequences: `settle` then ends it with a CAN before a
/// diagnostic; a frame that went out leaves nothing to end (M1T08R-10).
#[test]
fn a_frame_cut_short_is_ended_before_a_diagnostic() {
    use super::terminal::{Output, Steps, settle_on};
    use std::io::Write;
    let steps = Steps::new();
    let mut out = Output::over(Flaky::new(3), &steps);
    out.write_all(b"\x1b[2;5Hhello").expect("buffered");
    out.inner_mut().fail_at = vec![3];
    assert!(out.flush().is_err());
    let mut tty = Flaky::new(64);
    settle_on(&mut tty, &steps);
    assert_eq!(tty.out, b"\x18");
    out.write_all(b"\x1b[3;1Hok").expect("buffered");
    out.flush().expect("the next frame goes out");
    let mut tty = Flaky::new(64);
    settle_on(&mut tty, &steps);
    assert_eq!(tty.out, b"", "nothing left open");
}

/// A signal ends the handling of a batch between two events: what was read after it is not acted on, so a long batch
/// (each Enter writes the history) cannot hold up the exit (M1T08R-21).
#[test]
fn a_signal_ends_a_batch_between_two_events() {
    use super::{End, handle, input::Decoder};
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::new("p".to_owned(), History::load(dir.path()));
    let mut decoder = Decoder::default();
    decoder.feed(b"x\ry\rz\r");
    // The flag is raised once two events (x, Enter) have been handled.
    let looks = std::cell::Cell::new(0);
    let signal = || {
        looks.set(looks.get() + 1);
        if looks.get() > 2 { 15 } else { 0 }
    };
    let mut dirty = false;
    assert_eq!(
        handle(&mut app, &mut decoder, false, signal, &mut dirty),
        Some(End::Signal(15))
    );
    assert!(dirty);
    assert_eq!(History::load(dir.path()).entries(), ["x"]);
    // Without a signal the batch is handled in full.
    let mut decoder = Decoder::default();
    decoder.feed(b"y\rz\r");
    assert_eq!(
        handle(&mut app, &mut decoder, false, || 0, &mut dirty),
        None
    );
    assert_eq!(History::load(dir.path()).entries(), ["x", "y", "z"]);
}

/// The cells a grapheme takes when ratatui draws it (a control character is not drawn).
fn cells(g: &str) -> usize {
    use ratatui::buffer::CellWidth;
    if g.chars().any(char::is_control) {
        0
    } else {
        usize::from(g.cell_width())
    }
}

/// The view of a long line is found walking back from the cursor, once per draw: 64 KiB takes far less than a
/// second, where dropping graphemes from the front one by one took tens of seconds, all that time without looking at
/// the signal flags (M1T08R-31).
#[test]
fn the_view_of_a_long_line_is_found_in_linear_time() {
    let mut e = Editor::default();
    e.insert(&"x".repeat(64 << 10));
    let started = std::time::Instant::now();
    let (shown, column) = e.view(100);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!((shown.len(), column), (99, 99));
}

/// The view and the cursor's column are measured as ratatui draws the line, grapheme by grapheme: the text before
/// the cursor fills as much of the room as it can and the cursor lands on the cell after it, also where a whole
/// string measures narrower than its graphemes drawn one by one (lam-alef, halfwidth sound marks) and for wide,
/// combining, joined and control characters (M1T08R-34).
#[test]
fn the_view_is_measured_as_ratatui_draws_it() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Widget};
    use unicode_segmentation::UnicodeSegmentation;
    for text in [
        "abلاcd",
        "xｶﾞｷﾞy",
        "ab草稿e\u{301}👩\u{200d}👩c草",
        "a\tb\u{7}c",
        "א\u{200d}לב",
    ] {
        let mut e = Editor::default();
        e.insert(text);
        let graphemes = text.graphemes(true).count();
        for left in 0..=graphemes {
            let before = &e.text()[..e.cursor_byte()];
            for width in 1..14 {
                let room = width - 1;
                let (shown, column) = e.view(width);
                let start = e.text().len() - shown.len();
                let drawn: usize = e.text()[start..e.cursor_byte()]
                    .graphemes(true)
                    .map(cells)
                    .sum();
                assert_eq!(column, drawn, "{text:?} left {left} width {width}");
                assert!(column <= room, "{text:?} left {left} width {width}");
                // As much as fits: the grapheme before the shown part would not.
                if let Some(g) = before[..start].graphemes(true).next_back() {
                    assert!(
                        column + cells(g) > room,
                        "{text:?} left {left} width {width}"
                    );
                }
                // The cursor's cell is where ratatui draws the first grapheme after the cursor.
                let next = e.text()[e.cursor_byte()..]
                    .graphemes(true)
                    .find(|g| cells(g) > 0);
                if let Some(g) = next.filter(|g| column + cells(g) <= width) {
                    let mut buf =
                        Buffer::empty(Rect::new(0, 0, u16::try_from(width).unwrap_or(1), 1));
                    Paragraph::new(shown).render(buf.area, &mut buf);
                    let x = u16::try_from(column).unwrap_or(0);
                    assert_eq!(
                        buf[(x, 0)].symbol(),
                        g,
                        "{text:?} left {left} width {width}"
                    );
                }
            }
            e.left();
        }
    }
    // abلاcd with the cursor at the end in five columns: ratatui draws ل and ا in a cell each, so four cells come
    // before the cursor, not the three the whole string measures.
    let mut e = Editor::default();
    e.insert("abلاcd");
    assert_eq!(e.view(5), ("لاcd", 4));
}

/// The prompt and the text are drawn apart: a halfwidth sound mark at the start of the text, which would join the
/// prompt's space into one grapheme three cells wide, stays the text's own, and the cursor lands on the cell of the
/// character after it (M1T08R-38).
#[test]
fn the_prompt_and_the_text_are_drawn_apart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.paste("\u{ff9e}x");
    assert_eq!(app.key(key(KeyCode::Left)), None);
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).expect("terminal");
    terminal.draw(|f| app.draw(f)).expect("draw");
    let cursor = terminal.get_cursor_position().expect("cursor");
    let cell = terminal.backend().buffer()[(cursor.x, cursor.y)]
        .symbol()
        .to_owned();
    assert_eq!(cell, "x", "the cursor is on the x after the mark");
    assert_eq!(
        terminal.backend().buffer()[(1, cursor.y)].symbol(),
        " ",
        "the prompt's space"
    );
}

#[test]
fn a_links_get_answered_403_goes_once_more_to_the_host_found_again() -> crate::client::tests::R {
    // M2T06R-155 (docs/adr/ADR-021.md decision 10): the link holds a host that stopped, and another started (another
    // token) before the reader noticed. A GET is answered by the new host, and the link keeps it.
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let first = runtime.block_on(hoh_app::host::Host::start(crate::client::tests::config(
        &project,
    )?))?;
    let stale = crate::client::running(&project).ok_or("no host")?;
    runtime.block_on(first.shutdown());
    let second = runtime.block_on(crate::client::tests::start_when_free(
        crate::client::tests::config(&project)?,
    ))?;
    let (_events_in, events) = std::sync::mpsc::channel();
    let link = super::Link {
        host: Arc::new(Mutex::new(stale.clone())),
        project: project.clone(),
        run_id: "R".to_owned(),
        events,
        stop: Arc::new(AtomicBool::new(false)),
    };
    let reply = link.get("/v1/runs")?;
    assert_eq!(reply.status, 200);
    assert_ne!(
        link.host().token,
        stale.token,
        "the link keeps the host found again"
    );
    drop(link);
    runtime.block_on(second.shutdown());
    Ok(())
}

#[test]
fn the_top_bar_shows_the_projects_configured_mode() {
    // M2T07R-355 (ADR-014, docs/adr/ADR-022.md decision 6): the mode on the top bar is the project's configured
    // default_mode; with no configuration file, or an invalid one, it is the built-in AUTO.
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(mode_of(dir.path()), "AUTO");
    std::fs::create_dir_all(dir.path().join(".hoh")).expect("mkdir");
    let file = dir.path().join(".hoh/config.toml");
    std::fs::write(
        &file,
        "config_version = \"1.0\"\n[project]\ndefault_mode = \"PLAN\"\n",
    )
    .expect("write");
    assert_eq!(mode_of(dir.path()), "PLAN");
    let app = App::open(dir.path());
    assert!(
        screen(&app, 100, 30)[0].contains("模式 PLAN"),
        "{:?}",
        screen(&app, 100, 30)[0]
    );
    std::fs::write(
        &file,
        "config_version = \"1.0\"\n[project]\ndefault_mode = \"plan\"\n",
    )
    .expect("write");
    assert_eq!(mode_of(dir.path()), "AUTO");
}

/// A run as list_runs gives it (docs/adr/ADR-031.md decision 13): no goal, no blocked reason.
fn listed(run_id: &str, state: &str, created_at: &str) -> serde_json::Value {
    serde_json::json!({
        "run_id": run_id, "state": state,
        "label": if state == "CANCEL_REQUESTED" { "CANCELLING" } else { state },
        "mode": "AUTO", "revision": 1, "resume_state": null, "blocked_reason": null,
        "created_at": created_at, "goal": null,
    })
}

/// A COMPLETED run whose candidate accepted last is `commit`'s, accepted at `seq` (or none: null).
fn completed(run_id: &str, created_at: &str, accepted: Option<(&str, i64)>) -> serde_json::Value {
    let mut run = listed(run_id, "COMPLETED", created_at);
    run["accepted"] = accepted.map_or(serde_json::Value::Null, |(commit, seq)| {
        serde_json::json!({"candidate_id": format!("c-{run_id}"), "git_commit": commit, "seq": seq})
    });
    run
}

#[test]
fn the_top_bar_shows_the_baseline_of_the_candidate_accepted_last_among_completed_runs() {
    // Decision 13: the project's baseline is the git commit of the candidate accepted last (greatest seq) among the
    // COMPLETED runs' ACCEPTED candidates, its first 12 hex digits; "无" without one.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
    // A run not COMPLETED gives no baseline, even with an accepted candidate (the host gives it none).
    let mut running = listed("r", "RUNNING", "2026-11-05T00:00:00Z");
    running["accepted"] = serde_json::json!({"candidate_id": "x", "git_commit": c, "seq": 99});
    app.list(&[
        running,
        completed("c3", "2026-11-04T00:00:00Z", None),
        completed("c2", "2026-11-02T00:00:00Z", Some((&b, 10))),
        completed("c1", "2026-11-03T00:00:00Z", Some((&a, 20))),
    ]);
    let top = screen(&app, 120, 10)[0].clone();
    assert!(top.contains(&format!("baseline {} ", &a[..12])), "{top:?}");
    // The same while attached.
    app.attach("c1", "COMPLETED");
    let top = screen(&app, 120, 10)[0].clone();
    assert!(top.contains(&format!("baseline {} ", &a[..12])), "{top:?}");
    for runs in [
        vec![],
        vec![listed("r", "RUNNING", "2026-11-05T00:00:00Z")],
        vec![completed("c3", "2026-11-04T00:00:00Z", None)],
    ] {
        let mut app = app_in(dir.path());
        app.list(&runs);
        let top = screen(&app, 120, 10)[0].clone();
        assert!(top.contains("baseline 无"), "{runs:?}: {top:?}");
    }
}

#[test]
fn the_workspace_lists_the_runs_not_ended_first_and_the_newest_first_one_line_each() {
    // Decision 13: `<short id>  <label>  <blocked reason?>  <goal cut short>`, the short id the run id's first 8
    // characters; the runs not in a terminal state first, each group newest first; a goal of several lines on one.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    let mut paused = listed("aaaaaaaa-1", "PAUSED", "2026-11-01T00:00:00Z");
    paused["goal"] = "暂停的目标".into();
    let mut blocked = listed("bbbbbbbb-2", "BLOCKED", "2026-11-03T00:00:00Z");
    blocked["blocked_reason"] = "NEED_DECISION".into();
    blocked["goal"] = "一行\n两行".into();
    let mut long = listed("dddddddd-4", "RUNNING", "2026-11-02T00:00:00Z");
    long["goal"] = format!("{}END", "长".repeat(100)).into();
    let cancelling = listed("eeeeeeee-5", "CANCEL_REQUESTED", "2026-10-01T00:00:00Z");
    let done = listed("cccccccc-3", "COMPLETED", "2026-11-04T00:00:00Z");
    let failed = listed("ffffffff-6", "FAILED", "2026-11-05T00:00:00Z");
    app.list(&[done, failed, blocked, long, paused, cancelling]);
    let rows = screen(&app, 80, 20);
    let order: Vec<&str> = rows[1..7].iter().map(|r| &r[..8]).collect();
    assert_eq!(
        order,
        [
            "bbbbbbbb", "dddddddd", "aaaaaaaa", "eeeeeeee", "ffffffff", "cccccccc"
        ],
        "{rows:#?}"
    );
    assert_eq!(
        rows[1].trim_end(),
        "bbbbbbbb  BLOCKED  NEED_DECISION  一行 两行"
    );
    assert_eq!(rows[3].trim_end(), "aaaaaaaa  PAUSED  暂停的目标");
    assert_eq!(rows[4].trim_end(), "eeeeeeee  CANCELLING");
    assert_eq!(rows[6].trim_end(), "cccccccc  COMPLETED");
    let cut = rows[2].trim_end();
    assert!(cut.starts_with("dddddddd  RUNNING  长长"), "{cut:?}");
    assert!(cut.ends_with('…') && !cut.contains("END"), "{cut:?}");
    assert!(cut.width() <= 80, "{cut:?}");
}

#[test]
fn a_slash_command_is_the_workspaces_own_and_resume_chooses_among_the_runs_not_ended() {
    // Decision 13, E11: `/resume` opens the selector of the runs not ended (the one of attach); any other `/…` is
    // "M3 前未提供"; neither asks the host for anything. Esc closes the selector; Enter on a run attaches it.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.list(&[
        listed("cccccccc-3", "COMPLETED", "2026-11-04T00:00:00Z"),
        listed("bbbbbbbb-2", "PAUSED", "2026-11-02T00:00:00Z"),
        listed("aaaaaaaa-1", "RUNNING", "2026-11-01T00:00:00Z"),
    ]);
    typed(&mut app, "/plan x");
    app.key(key(KeyCode::Enter));
    assert!(
        screen(&app, 80, 20)
            .iter()
            .any(|r| r.contains("M3 前未提供"))
    );
    typed(&mut app, "/resume");
    app.key(key(KeyCode::Enter));
    let rows = screen(&app, 80, 20);
    assert!(rows.iter().any(|r| r.contains("选择 Run")), "{rows:#?}");
    let offered: Vec<&String> = rows
        .iter()
        .filter(|r| r.contains("aaaaaaaa") || r.contains("bbbbbbbb") || r.contains("cccccccc"))
        .collect();
    assert_eq!(offered.len(), 2, "{rows:#?}");
    assert!(offered[0].contains("bbbbbbbb") && offered[1].contains("aaaaaaaa"));
    assert_eq!(app.key(key(KeyCode::Esc)), None, "Esc only closes it");
    assert!(!screen(&app, 80, 20).iter().any(|r| r.contains("选择 Run")));
    assert_eq!(app.take_requests(), []);
    typed(&mut app, "/resume");
    app.key(key(KeyCode::Enter));
    app.key(key(KeyCode::Down));
    app.key(key(KeyCode::Down));
    app.key(key(KeyCode::Up));
    app.key(key(KeyCode::Down));
    assert_eq!(app.key(key(KeyCode::Enter)), None);
    assert_eq!(
        app.take_requests(),
        [Request::Attach("aaaaaaaa-1".to_owned())]
    );
    assert!(!screen(&app, 80, 20).iter().any(|r| r.contains("选择 Run")));
    // With no run to resume there is nothing to choose.
    let mut app = app_in(dir.path());
    app.list(&[listed("cccccccc-3", "COMPLETED", "2026-11-04T00:00:00Z")]);
    typed(&mut app, "/resume");
    app.key(key(KeyCode::Enter));
    let rows = screen(&app, 80, 20);
    assert!(
        rows.iter().any(|r| r.contains("没有可附着的 Run")),
        "{rows:#?}"
    );
    assert!(!rows.iter().any(|r| r.contains("选择 Run")));
}

#[test]
fn attachs_selector_leaves_on_esc_or_on_the_ctrl_c_choice() {
    // Decision 12: the selector `hoh attach` opens with two runs on a terminal leaves with Esc, or with Ctrl+C's
    // "脱离"; there is no run to pause or cancel.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.list(&[
        listed("bbbbbbbb-2", "PAUSED", "2026-11-02T00:00:00Z"),
        listed("aaaaaaaa-1", "RUNNING", "2026-11-01T00:00:00Z"),
    ]);
    app.pick();
    assert_eq!(app.key(key(KeyCode::Esc)), Some(Exit::Detach));
    app.pick();
    assert_eq!(app.key(ctrl('c')), None);
    let rows = screen(&app, 80, 20);
    assert!(rows.iter().any(|r| r.contains("脱离")), "{rows:#?}");
    assert!(!rows.iter().any(|r| r.contains("取消任务")));
    assert_eq!(app.key(key(KeyCode::Enter)), Some(Exit::Detach));
    app.pick();
    assert_eq!(app.key(key(KeyCode::Enter)), None);
    assert_eq!(
        app.take_requests(),
        [Request::Attach("bbbbbbbb-2".to_owned())]
    );
}

#[test]
fn enter_while_attached_sends_the_text_as_input_slash_included() {
    // Decision 13 (Enter, attached): SendInput with the text, whatever it starts with; an Enter after one that starts
    // a run, before the loop has done it, goes to the run it starts.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    typed(&mut app, "a");
    app.key(key(KeyCode::Enter));
    typed(&mut app, "/resume");
    app.key(key(KeyCode::Enter));
    assert_eq!(
        app.take_requests(),
        [
            Request::Start("a".to_owned()),
            Request::Input("/resume".to_owned())
        ]
    );
    app.attach("R", "RUNNING");
    typed(&mut app, "b");
    app.key(key(KeyCode::Enter));
    assert_eq!(app.take_requests(), [Request::Input("b".to_owned())]);
    assert_eq!(History::load(dir.path()).entries(), ["a", "/resume", "b"]);
}

/// The status bar: the last row of a screen of `height` rows.
fn status(app: &App, height: u16) -> String {
    screen(app, 100, height)[usize::from(height) - 1].clone()
}

#[test]
fn the_attached_runs_status_shows_its_attempts_tracked_degraded_and_says_which_and_why() {
    // M2T06R-560 (docs/adr/ADR-021.md decision 8d): an attempt tracked by the degraded /proc scan shows in the run's
    // status: the status bar counts the run's such attempts (getRun's `degraded_tracking` at the attach, then each
    // attempt.tracking event with `proc`, an attempt counted once), and the event's line says which attempt and why. One
    // tracked by its cgroup is not degraded.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.attach("R", "RUNNING");
    assert!(
        !status(&app, 10).contains("跟踪降级"),
        "{:?}",
        status(&app, 10)
    );
    app.tracking(&serde_json::json!([{"attempt_id": "T1/attempt-1", "reason": "the host has no delegated cgroup"}]));
    assert!(
        status(&app, 10).contains("事件 0 · #0 · 跟踪降级 1 │"),
        "{:?}",
        status(&app, 10)
    );
    for (seq, attempt, tracking) in [
        (1, "T2/attempt-1", "cgroup"),
        (2, "T3/attempt-1", "proc"),
        (3, "T1/attempt-1", "proc"),
    ] {
        app.event(
            &serde_json::json!({"seq": seq, "run_id": "R", "kind": "attempt.tracking",
            "payload": {"attempt_id": attempt, "tracking": tracking, "reason": "no cgroup"}}),
        );
    }
    assert!(
        status(&app, 10).contains("事件 3 · #3 · 跟踪降级 2 │"),
        "{:?}",
        status(&app, 10)
    );
    let rows = screen(&app, 100, 10);
    assert!(
        rows.iter().any(|r| r.contains("#2 attempt.tracking")
            && r.contains("T3/attempt-1")
            && r.contains("proc（no cgroup）")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.trim_end() == "#1 attempt.tracking"),
        "a cgroup one is an event like any other: {rows:?}"
    );
    // Attached again: the count is the new run's.
    app.attach("S", "RUNNING");
    assert!(
        !status(&app, 10).contains("跟踪降级"),
        "{:?}",
        status(&app, 10)
    );
}

#[test]
fn the_status_bar_counts_each_event_of_the_attached_run_once_and_shows_the_latest_seq() {
    // Decision 13: "事件 <events received> · #<latest seq>"; the client keeps seq strictly increasing, so an event
    // seen again (a stream opened again after the last one) is not counted twice.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    assert!(!status(&app, 10).contains("事件"), "unattached: no count");
    app.attach("R", "RUNNING");
    assert!(
        status(&app, 10).contains("事件 0 · #0"),
        "{:?}",
        status(&app, 10)
    );
    for seq in [1, 2, 2, 4, 3, 7] {
        app.event(
            &serde_json::json!({"seq": seq, "run_id": "R", "kind": "task.ready", "payload": {}}),
        );
    }
    assert!(
        status(&app, 10).contains("事件 4 · #7"),
        "{:?}",
        status(&app, 10)
    );
    assert!(
        app.event(
            &serde_json::json!({"seq": 8, "run_id": "R", "kind": "run.paused", "payload": {}})
        ),
        "a run event: its label is read again"
    );
    assert!(
        !app.event(
            &serde_json::json!({"seq": 8, "run_id": "R", "kind": "run.paused", "payload": {}})
        )
    );
}

#[test]
fn the_acceptance_of_an_input_says_whether_it_answered_a_question() {
    // Decision 13 (Enter, attached): once the acceptance event of the client's SendInput comes (its payload names the
    // command), the message bar says "已答复 <question_id>", or "已排队（M3-T04 投递）" when it answered none.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.attach("R", "BLOCKED");
    app.awaiting("c1");
    app.event(
        &serde_json::json!({"seq": 3, "kind": "run.result", "payload": {"command_id": "c1"}}),
    );
    assert!(!screen(&app, 100, 20).iter().any(|r| r.contains("已答复")));
    app.event(&serde_json::json!({"seq": 4, "kind": "input.accepted",
        "payload": {"command_id": "c1", "question_id": "q-7"}}));
    assert!(
        screen(&app, 100, 20)
            .iter()
            .any(|r| r.contains("已答复 q-7"))
    );
    app.awaiting("c2");
    app.event(
        &serde_json::json!({"seq": 5, "kind": "input.accepted", "payload": {"command_id": "c2"}}),
    );
    let rows = screen(&app, 100, 20);
    assert!(
        rows.iter().any(|r| r.contains("已排队（M3-T04 投递）")),
        "{rows:#?}"
    );
    app.event(&serde_json::json!({"seq": 6, "kind": "x", "payload": {"command_id": "c2"}}));
    assert_eq!(
        screen(&app, 100, 20)
            .iter()
            .filter(|r| r.contains("已排队"))
            .count(),
        1,
        "said once"
    );
}

/// A run's host as a script: getRun's successive answers (the last repeats) and the answers to the commands, which
/// it records.
struct Scripted {
    runs: std::cell::RefCell<std::collections::VecDeque<serde_json::Value>>,
    replies: std::cell::RefCell<
        std::collections::VecDeque<Result<serde_json::Value, crate::client::Failure>>,
    >,
    sent: std::cell::RefCell<Vec<serde_json::Value>>,
}

impl Scripted {
    fn new(
        runs: Vec<serde_json::Value>,
        replies: Vec<Result<serde_json::Value, crate::client::Failure>>,
    ) -> Scripted {
        Scripted {
            runs: std::cell::RefCell::new(runs.into()),
            replies: std::cell::RefCell::new(replies.into()),
            sent: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl super::Port for Scripted {
    fn run(&self) -> Result<Option<serde_json::Value>, crate::client::Failure> {
        let mut runs = self.runs.borrow_mut();
        Ok(if runs.len() > 1 {
            runs.pop_front()
        } else {
            runs.front().cloned()
        })
    }

    fn submit(
        &self,
        command: serde_json::Value,
    ) -> Result<serde_json::Value, crate::client::Failure> {
        self.sent.borrow_mut().push(command);
        self.replies
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err(crate::client::Failure::unreachable("no more replies")))
    }
}

fn refused(status: u16, code: &str, details: serde_json::Value) -> crate::client::Failure {
    crate::client::Failure {
        status: Some(status),
        details,
        ..crate::client::Failure::new(code, format!("{code} said"), 1)
    }
}

#[test]
fn an_input_to_an_incomplete_run_resumes_it_after_and_a_refused_resume_names_the_stop() {
    // Decisions 13 and 16 (the same as `--resume`): SendInput{text, target_role: null, REPLY} at the run's revision,
    // then, the run being INCOMPLETE, ResumeRun at the revision the input left; a resume refused because the stop
    // still holds is said with that stop.
    let port = Scripted::new(
        vec![serde_json::json!({"run_id": "R", "state": "INCOMPLETE", "revision": 4})],
        vec![
            Ok(
                serde_json::json!({"command_id": "c1", "run_id": "R", "revision": 5, "status": "ACCEPTED"}),
            ),
            Err(refused(
                409,
                "ILLEGAL_TRANSITION",
                serde_json::json!({"state": "INCOMPLETE", "stop": "deadline"}),
            )),
        ],
    );
    let sent = super::input(&port, "R", "继续");
    let commands = port.sent.borrow();
    assert_eq!(commands.len(), 2, "{commands:#?}");
    assert_eq!(
        (
            &commands[0]["kind"],
            &commands[0]["expected_revision"],
            &commands[0]["run_id"],
            &commands[0]["payload"]
        ),
        (
            &serde_json::json!("SendInput"),
            &serde_json::json!(4),
            &serde_json::json!("R"),
            &serde_json::json!({"text": "继续", "target_role": null, "input_kind": "REPLY"})
        )
    );
    assert_eq!(
        (&commands[1]["kind"], &commands[1]["expected_revision"]),
        (&serde_json::json!("ResumeRun"), &serde_json::json!(5))
    );
    assert_eq!(sent.awaiting.as_deref(), Some("c1"));
    assert!(
        sent.said
            .iter()
            .any(|s| s.contains("未恢复") && s.contains("deadline")),
        "{:?}",
        sent.said
    );
}

#[test]
fn an_input_to_a_working_run_is_sent_alone_and_again_at_a_fresh_revision() {
    let port = Scripted::new(
        vec![
            serde_json::json!({"run_id": "R", "state": "RUNNING", "revision": 2}),
            serde_json::json!({"run_id": "R", "state": "RUNNING", "revision": 3}),
        ],
        vec![
            Err(refused(409, "STALE_REVISION", serde_json::Value::Null)),
            Ok(
                serde_json::json!({"command_id": "c2", "run_id": "R", "revision": 4, "status": "ACCEPTED"}),
            ),
        ],
    );
    let sent = super::input(&port, "R", "x");
    let commands = port.sent.borrow();
    let revisions: Vec<(&serde_json::Value, &serde_json::Value)> = commands
        .iter()
        .map(|c| (&c["kind"], &c["expected_revision"]))
        .collect();
    assert_eq!(
        revisions,
        [
            (&serde_json::json!("SendInput"), &serde_json::json!(2)),
            (&serde_json::json!("SendInput"), &serde_json::json!(3))
        ]
    );
    assert_eq!(sent.awaiting.as_deref(), Some("c2"));
    assert!(sent.said.is_empty(), "{:?}", sent.said);
}

#[test]
fn a_refused_input_is_said_with_its_code_and_nothing_follows() {
    let port = Scripted::new(
        vec![serde_json::json!({"run_id": "R", "state": "INCOMPLETE", "revision": 2})],
        vec![Err(refused(
            422,
            "CAPABILITY_UNSUPPORTED",
            serde_json::Value::Null,
        ))],
    );
    let sent = super::input(&port, "R", "x");
    assert_eq!(
        port.sent.borrow().len(),
        1,
        "no ResumeRun after a refused input"
    );
    assert_eq!(sent.awaiting, None);
    assert!(
        sent.said
            .iter()
            .any(|s| s.contains("CAPABILITY_UNSUPPORTED")),
        "{:?}",
        sent.said
    );
}

/// A host that answers `/healthz` and serves one event stream of `events` (SSE blocks), then keeps it open; how many
/// event streams were asked for. Named by the project's `host.json`, so a link that looks for its host again finds
/// this one (never starts one).
fn sse_host(
    project: &std::path::Path,
    socket: &std::path::Path,
    events: &'static [&'static str],
) -> crate::client::tests::R<std::sync::Arc<std::sync::atomic::AtomicUsize>> {
    use std::io::{BufRead as _, Write as _};
    let listener = std::os::unix::net::UnixListener::bind(socket)?;
    std::fs::create_dir_all(project.join(".hoh"))?;
    std::fs::write(
        project.join(".hoh/host.json"),
        serde_json::json!({"pid": std::process::id(), "socket": socket, "token": "t"}).to_string(),
    )?;
    let streams = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = streams.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let counted = counted.clone();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(&stream);
                let mut first = String::new();
                let _ = reader.read_line(&mut first);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let mut out = &stream;
                if first.contains("/healthz") {
                    let _ = out.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    );
                    return;
                }
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = out.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                );
                for block in events {
                    let _ = write!(out, "{:x}\r\n{block}\r\n", block.len());
                }
                let _ = out.flush();
                std::thread::sleep(std::time::Duration::from_secs(5));
            });
        }
    });
    Ok(streams)
}

#[test]
fn an_event_the_link_cannot_read_is_said_with_its_id_and_nothing_after_it_is_followed()
-> crate::client::tests::R {
    // docs/adr/ADR-031.md decision 10, in the TUI: an event whose data does not parse is never skipped silently; the
    // message bar names its SSE id, and the link follows no further (no event after it, no new stream).
    let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let socket = dir.path().join("s");
    let streams = sse_host(
        &project,
        &socket,
        &[
            "id: 1\ndata: {\"seq\":1,\"run_id\":\"R\",\"kind\":\"task.created\",\"payload\":{}}\n\n",
            "id: 2\ndata: {\"seq\":2,\"run_id\":\n\n",
            "id: 3\ndata: {\"seq\":3,\"run_id\":\"R\",\"kind\":\"task.ready\",\"payload\":{}}\n\n",
        ],
    )?;
    let conn = super::Conn::new(
        &project,
        crate::client::HostInfo {
            pid: std::process::id(),
            socket: socket.clone(),
            token: "t".to_owned(),
        },
    );
    let link = conn.link("R");
    let mut app = app_in(dir.path());
    app.attach("R", "RUNNING");
    let said = |app: &App| {
        screen(app, 100, 20)
            .iter()
            .any(|r| r.contains("事件 2 的数据无法解析"))
    };
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !said(&app) && std::time::Instant::now() < end {
        super::take_events(&mut app, &link);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let rows = screen(&app, 100, 20);
    assert!(said(&app), "{rows:#?}");
    // Nothing after it: event 3 is not shown, and the link asks for no new stream.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    super::take_events(&mut app, &link);
    let rows = screen(&app, 100, 20);
    assert!(
        rows.iter().any(|r| r.contains("#1 task.created")),
        "{rows:#?}"
    );
    assert!(!rows.iter().any(|r| r.contains("task.ready")), "{rows:#?}");
    assert!(
        rows.last().is_some_and(|r| r.contains("事件 1 · #1")),
        "{rows:#?}"
    );
    assert_eq!(streams.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(link);
    Ok(())
}

#[test]
fn the_status_bar_says_that_enter_sends() {
    // Decision 13: Enter sends (StartRun unattached, SendInput attached), so the hint says so, not that it only records.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    for state in ["unattached", "attached"] {
        let bar = status(&app, 10);
        assert!(bar.contains("Enter 发送"), "{state}: {bar:?}");
        assert!(!bar.contains("Enter 记录"), "{state}: {bar:?}");
        app.attach("R", "RUNNING");
    }
}

#[test]
fn test_047_02_settings_diff_restore_and_cancel() {
    let dir = tempfile::Builder::new()
        .prefix("m3t03-settings-")
        .tempdir()
        .unwrap();
    let path = dir.path().join(".hoh/prompts/developer.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "custom developer\n").unwrap();
    let mut app = App::open(dir.path());
    app.attach("R", "RUNNING");
    typed(&mut app, "/settings developer");
    app.key(key(KeyCode::Enter));
    let shown = screen(&app, 110, 30).join("\n");
    assert!(shown.contains("设置：developer"), "{shown}");
    assert!(shown.contains("--- builtin"), "{shown}");
    assert!(shown.contains("+++ project"), "{shown}");
    assert!(
        app.take_requests().is_empty(),
        "settings is local even when attached"
    );
    app.key(key(KeyCode::Esc));
    assert!(path.exists(), "closing settings never restores");
    typed(&mut app, "/settings developer");
    app.key(key(KeyCode::Enter));
    app.key(key(KeyCode::Char('r')));
    assert!(!path.exists(), "restore deletes the project override");
    assert!(screen(&app, 110, 30).join("\n").contains("已恢复默认"));
}

#[test]
fn test_047_03_settings_exposes_drift_and_both_diffs() {
    let dir = tempfile::Builder::new()
        .prefix("m3t03-drift-ui-")
        .tempdir()
        .unwrap();
    let mut old = hoh_core::resource_bundle::Bundle::current();
    old.files.insert(
        "resources/prompts/developer.md".to_owned(),
        "old developer\n".to_owned(),
    );
    hoh_core::prompt_files::save_override_based_on(
        dir.path(),
        ".hoh/prompts/developer.md",
        "custom\n",
        "developer",
        &old,
    )
    .unwrap();
    let mut app = App::open(dir.path());
    typed(&mut app, "/settings developer");
    app.key(key(KeyCode::Enter));
    let shown = screen(&app, 120, 400).join("\n");
    assert!(shown.contains("漂移"), "{shown}");
    assert!(shown.contains("--- old-builtin"));
    assert!(shown.contains("+++ new-builtin"));
    assert!(shown.contains("+++ override"));
}
