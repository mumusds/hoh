//! The TUI shell's editor, history, keys and screen (ADR-014), without a terminal.

use super::app::{App, Exit};
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
fn enter_records_the_input_and_says_it_was_not_sent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    assert_eq!(app.key(key(KeyCode::Enter)), None);
    assert!(
        !dir.path().join(".hoh").exists(),
        "an empty Enter records nothing"
    );
    typed(&mut app, "hi");
    assert_eq!(app.key(key(KeyCode::Enter)), None);
    assert_eq!(app.editor_text(), "");
    assert_eq!(History::load(dir.path()).entries(), ["hi"]);
    let rows = screen(&app, 100, 30);
    assert!(rows.iter().any(|r| r.contains("hi")));
    assert!(
        rows.iter().any(|r| r.contains("host 离线，未发送")),
        "{rows:#?}"
    );
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
    assert!(!screen(&app, 100, 30).iter().any(|r| r.contains("未发送")));
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
fn the_workspace_shows_the_project_the_mode_and_an_offline_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path());
    let rows = screen(&app, 100, 30);
    let top = &rows[0];
    for part in ["demo", "AUTO", "run 无", "baseline 未知", "host 离线"] {
        assert!(top.contains(part), "{part:?} in {top:?}");
    }
    // Narrow terminals still show the editor and the status line.
    let rows = screen(&app, 40, 8);
    assert!(rows.iter().any(|r| r.contains("host 离线")));
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
