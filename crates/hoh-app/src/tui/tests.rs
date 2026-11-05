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
