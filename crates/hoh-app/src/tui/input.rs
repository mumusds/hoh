//! Terminal input decoded by HoH itself (ADR-014 decision 1). crossterm 0.29 still draws and switches the terminal's
//! modes, but its event reader is never started: once the terminal is at end of file it retries a zero-byte read
//! forever (M1QA-01), after an unfinished escape sequence it blocks in read(2) for the rest (M1T08R-13), and a read
//! it has entered cannot be called off, so a reader thread could take the shell's next command (M1T08R-12, -14). The
//! TUI loop instead waits in poll(2) itself and reads only what poll said is there; this decoder turns those bytes
//! into the same `Event`s crossterm's parser gives for the keys the Workspace uses, and never waits.

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

const ESC: u8 = 0x1b;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Bytes read from the terminal, not yet decoded.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// While a paste is unfinished: how much of `buf` has been searched for its end marker (M1T08R-18).
    searched: usize,
}

/// What the bytes not yet decoded wait for, which says how long the loop waits for more (M1T08R-20).
#[derive(Debug, PartialEq, Eq)]
pub enum Pending {
    Nothing,
    /// A lone ESC: the Esc key unless the rest of a sequence follows at once.
    Esc,
    /// A begun escape sequence, paste or character: its rest may come later on a slow link.
    Sequence,
}

/// What the front of the buffer is.
enum Front {
    /// An event, and how many bytes it took.
    Event(Event, usize),
    /// A sequence the Workspace has no use for (mouse, focus, an unknown key), and its length.
    Skip(usize),
    /// The start of something longer; more bytes are needed.
    Unfinished,
}

impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn pending(&self) -> Pending {
        match self.buf.as_slice() {
            [] => Pending::Nothing,
            [ESC] => Pending::Esc,
            _ => Pending::Sequence,
        }
    }

    /// The next decoded event. `idle` says no more input came within the wait `pending` asked for: then a lone ESC
    /// is the Esc key, ESC and one byte are Alt with that key, and a longer unfinished sequence is dropped. An
    /// unfinished paste or UTF-8 character keeps waiting for its rest.
    pub fn next(&mut self, idle: bool) -> Option<Event> {
        loop {
            if self.buf.starts_with(PASTE_START) {
                return self.paste();
            }
            match front(&self.buf) {
                Front::Event(event, used) => {
                    self.buf.drain(..used);
                    return Some(event);
                }
                Front::Skip(used) => {
                    self.buf.drain(..used);
                }
                Front::Unfinished if !idle || self.buf.first() != Some(&ESC) => return None,
                // Alt and a character in pieces waits for its rest as the character alone does (M1T08R-24).
                Front::Unfinished if self.buf.get(1).is_some_and(|&b| b >= 0x80) => return None,
                Front::Unfinished => {
                    let event = match self.buf.len() {
                        1 => Some(key(KeyCode::Esc, KeyModifiers::NONE)),
                        2 => alt(single(self.buf[1])),
                        _ => None,
                    };
                    self.buf.clear();
                    if event.is_some() {
                        return event;
                    }
                }
            }
        }
    }

    /// The bracketed paste at the front once its end marker is there. Only bytes not searched before are searched,
    /// from a marker's length back so that a marker split between reads is found.
    fn paste(&mut self) -> Option<Event> {
        let from = self
            .searched
            .saturating_sub(PASTE_END.len() - 1)
            .max(PASTE_START.len());
        let Some(at) = self.buf[from..]
            .windows(PASTE_END.len())
            .position(|w| w == PASTE_END)
        else {
            self.searched = self.buf.len();
            return None;
        };
        let end = from + at;
        let text = String::from_utf8_lossy(&self.buf[PASTE_START.len()..end]).into_owned();
        self.buf.drain(..end + PASTE_END.len());
        self.searched = 0;
        Some(Event::Paste(text))
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

/// A character key as crossterm reports it: an upper-case letter carries SHIFT.
fn char_key(c: char) -> Event {
    let modifiers = if c.is_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    key(KeyCode::Char(c), modifiers)
}

fn alt(event: Option<Event>) -> Option<Event> {
    match event {
        Some(Event::Key(mut k)) => {
            k.modifiers |= KeyModifiers::ALT;
            Some(Event::Key(k))
        }
        other => other,
    }
}

/// The key of a single non-ESC ASCII byte, as crossterm's parser maps it in raw mode.
fn single(b: u8) -> Option<Event> {
    Some(match b {
        b'\r' => key(KeyCode::Enter, KeyModifiers::NONE),
        b'\t' => key(KeyCode::Tab, KeyModifiers::NONE),
        0x7f => key(KeyCode::Backspace, KeyModifiers::NONE),
        // In raw mode `\n` is Ctrl+J, as in crossterm.
        0x01..=0x1a => key(
            KeyCode::Char(char::from(b - 1 + b'a')),
            KeyModifiers::CONTROL,
        ),
        0x1c..=0x1f => key(
            KeyCode::Char(char::from(b - 0x1c + b'4')),
            KeyModifiers::CONTROL,
        ),
        0x00 => key(KeyCode::Char(' '), KeyModifiers::CONTROL),
        0x20..=0x7e => char_key(char::from(b)),
        _ => return None,
    })
}

fn front(buf: &[u8]) -> Front {
    let Some(&first) = buf.first() else {
        return Front::Unfinished;
    };
    if first != ESC {
        if first < 0x80 {
            return single(first).map_or(Front::Skip(1), |e| Front::Event(e, 1));
        }
        return utf8(buf);
    }
    let Some(&second) = buf.get(1) else {
        return Front::Unfinished;
    };
    match second {
        b'[' => csi(buf),
        b'O' => match buf.get(2) {
            None => Front::Unfinished,
            Some(&b) => match ss3(b) {
                Some(code) => Front::Event(key(code, KeyModifiers::NONE), 3),
                None => Front::Skip(3),
            },
        },
        ESC => Front::Event(key(KeyCode::Esc, KeyModifiers::NONE), 2),
        _ => match front(&buf[1..]) {
            Front::Event(event, used) => match alt(Some(event)) {
                Some(event) => Front::Event(event, used + 1),
                None => Front::Skip(used + 1),
            },
            Front::Skip(used) => Front::Skip(used + 1),
            Front::Unfinished => Front::Unfinished,
        },
    }
}

fn ss3(b: u8) -> Option<KeyCode> {
    Some(match b {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P'..=b'S' => KeyCode::F(1 + b - b'P'),
        _ => return None,
    })
}

/// A UTF-8 character at the front; an invalid byte is skipped.
fn utf8(buf: &[u8]) -> Front {
    let len = match buf[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Front::Skip(1),
    };
    if buf.len() < len {
        // A byte that cannot continue the character ends it early: skip the broken start.
        if buf[1..].iter().any(|b| b & 0xc0 != 0x80) {
            return Front::Skip(1);
        }
        return Front::Unfinished;
    }
    match std::str::from_utf8(&buf[..len])
        .ok()
        .and_then(|s| s.chars().next())
    {
        Some(c) => Front::Event(char_key(c), len),
        None => Front::Skip(1),
    }
}

/// `ESC [` parameters, intermediates and a final byte (ECMA-48). Two forms carry bytes past what ends a CSI:
/// the Linux console's F1-F5, `ESC [ [` and a letter, and an X10 mouse report, `ESC [ M` and three raw bytes.
/// A bracketed paste is taken before this (`Decoder::paste`).
fn csi(buf: &[u8]) -> Front {
    let whole = match buf.get(2) {
        Some(b'[') => Some(4),
        Some(b'M') => Some(6),
        _ => None,
    };
    if let Some(len) = whole {
        return if buf.len() < len {
            Front::Unfinished
        } else {
            Front::Skip(len)
        };
    }
    let mut i = 2;
    while let Some(&b) = buf.get(i) {
        match b {
            0x20..=0x3f => i += 1,
            0x40..=0x7e => {
                return csi_key(&buf[2..i], b)
                    .map_or(Front::Skip(i + 1), |e| Front::Event(e, i + 1));
            }
            // A byte no CSI can hold: what came before it is dropped, the byte itself is decoded next.
            _ => return Front::Skip(i),
        }
    }
    Front::Unfinished
}

/// The key a complete CSI names, with the modifiers and event type of its second parameter (`1;5C` is Ctrl+Right,
/// `1;5:3C` its release). The event type is a number, as in crossterm (M1T08R-28).
fn csi_key(params: &[u8], fin: u8) -> Option<Event> {
    let text = std::str::from_utf8(params).ok()?;
    let mut fields = text.split(';');
    let first = fields.next().unwrap_or_default();
    let mut second = fields.next().unwrap_or_default().split(':');
    let modifiers = second
        .next()
        .and_then(|m| m.parse::<u8>().ok())
        .map_or(KeyModifiers::NONE, modifiers);
    let kind = match second.next().and_then(|k| k.parse::<u8>().ok()) {
        Some(2) => KeyEventKind::Repeat,
        Some(3) => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    };
    if fin == b'u' {
        return csi_u(first, modifiers, kind);
    }
    let code = match fin {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'Z' => return Some(key(KeyCode::BackTab, KeyModifiers::SHIFT)),
        b'~' => match first.parse::<u8>().ok()? {
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            _ => return None,
        },
        _ => return None,
    };
    Some(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)))
}

/// `CSI codepoint[:alternates] ; modifiers[:event type] u` (fixterms and the kitty keyboard protocol; kitty sends it
/// for some combinations in its legacy mode too, Ctrl+Shift+R as `114;6u`), as crossterm decodes it (M1T08R-23).
/// kitty's own functional keys, private use codepoints, are not keys the Workspace uses.
fn csi_u(first: &str, mut modifiers: KeyModifiers, kind: KeyEventKind) -> Option<Event> {
    let mut codepoints = first.split(':');
    let codepoint: u32 = codepoints.next()?.parse().ok()?;
    // The key's own codepoint must be a character, whatever its alternate (M1T08R-29).
    let base = char::from_u32(codepoint)?;
    if (0xe000..=0xf8ff).contains(&codepoint) {
        return None;
    }
    // With Shift, an alternate codepoint is the shifted key: that character, without Shift.
    let shifted = codepoints
        .next()
        .and_then(|c| c.parse::<u32>().ok())
        .and_then(char::from_u32)
        .filter(|_| modifiers.contains(KeyModifiers::SHIFT));
    if let Some(c) = shifted {
        modifiers.remove(KeyModifiers::SHIFT);
        return Some(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char(c),
            modifiers,
            kind,
        )));
    }
    let code = match base {
        '\x1b' => KeyCode::Esc,
        '\r' => KeyCode::Enter,
        '\t' if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        '\t' => KeyCode::Tab,
        '\x7f' => KeyCode::Backspace,
        c => KeyCode::Char(c),
    };
    Some(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)))
}

/// xterm's modifier parameter: 1 + a bit mask of Shift 1, Alt 2, Ctrl 4.
fn modifiers(param: u8) -> KeyModifiers {
    let mask = param.saturating_sub(1);
    let mut m = KeyModifiers::NONE;
    if mask & 1 != 0 {
        m |= KeyModifiers::SHIFT;
    }
    if mask & 2 != 0 {
        m |= KeyModifiers::ALT;
    }
    if mask & 4 != 0 {
        m |= KeyModifiers::CONTROL;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(bytes: &[u8], idle: bool) -> Vec<Event> {
        let mut d = Decoder::default();
        d.feed(bytes);
        std::iter::from_fn(|| d.next(idle)).collect()
    }

    fn k(code: KeyCode, m: KeyModifiers) -> Event {
        key(code, m)
    }

    const N: KeyModifiers = KeyModifiers::NONE;

    #[test]
    fn plain_and_control_keys_decode_as_crossterm_does_in_raw_mode() {
        assert_eq!(
            all(b"aZ\r\t\x7f\x03\x12\n\x00\x1c", false),
            [
                k(KeyCode::Char('a'), N),
                k(KeyCode::Char('Z'), KeyModifiers::SHIFT),
                k(KeyCode::Enter, N),
                k(KeyCode::Tab, N),
                k(KeyCode::Backspace, N),
                k(KeyCode::Char('c'), KeyModifiers::CONTROL),
                k(KeyCode::Char('r'), KeyModifiers::CONTROL),
                k(KeyCode::Char('j'), KeyModifiers::CONTROL),
                k(KeyCode::Char(' '), KeyModifiers::CONTROL),
                k(KeyCode::Char('4'), KeyModifiers::CONTROL),
            ]
        );
    }

    /// crossterm's `KeyEvent` equality ignores SHIFT on an upper-case letter, so it is checked on its own.
    #[test]
    fn an_upper_case_letter_carries_shift_as_in_crossterm() {
        let modifiers: Vec<KeyModifiers> = all(b"Za", false)
            .into_iter()
            .filter_map(|e| match e {
                Event::Key(key) => Some(key.modifiers),
                _ => None,
            })
            .collect();
        assert_eq!(modifiers, [KeyModifiers::SHIFT, KeyModifiers::NONE]);
    }

    #[test]
    fn cursor_and_editing_keys_in_their_csi_and_ss3_forms() {
        assert_eq!(
            all(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[H\x1b[F\x1bOA\x1bOH\x1b[1~\x1b[4~\x1b[7~\x1b[8~\x1b[3~", false),
            [
                k(KeyCode::Up, N),
                k(KeyCode::Down, N),
                k(KeyCode::Right, N),
                k(KeyCode::Left, N),
                k(KeyCode::Home, N),
                k(KeyCode::End, N),
                k(KeyCode::Up, N),
                k(KeyCode::Home, N),
                k(KeyCode::Home, N),
                k(KeyCode::End, N),
                k(KeyCode::Home, N),
                k(KeyCode::End, N),
                k(KeyCode::Delete, N),
            ]
        );
    }

    #[test]
    fn modifier_parameters_and_alt_prefixes() {
        assert_eq!(
            all(b"\x1b[1;5C\x1b[1;3D\x1b[3;2~\x1bb\x1b\x7f", false),
            [
                k(KeyCode::Right, KeyModifiers::CONTROL),
                k(KeyCode::Left, KeyModifiers::ALT),
                k(KeyCode::Delete, KeyModifiers::SHIFT),
                k(KeyCode::Char('b'), KeyModifiers::ALT),
                k(KeyCode::Backspace, KeyModifiers::ALT),
            ]
        );
    }

    #[test]
    fn a_lone_esc_is_the_esc_key_only_once_input_is_idle() {
        let mut d = Decoder::default();
        d.feed(b"\x1b");
        assert_eq!(d.next(false), None, "the rest of a sequence may follow");
        assert_eq!(d.pending(), Pending::Esc);
        assert_eq!(d.next(true), Some(k(KeyCode::Esc, N)));
        assert_eq!(d.pending(), Pending::Nothing);
        assert_eq!(all(b"\x1b\x1b", false), [k(KeyCode::Esc, N)]);
    }

    #[test]
    fn an_unfinished_sequence_waits_and_is_settled_when_idle() {
        let mut d = Decoder::default();
        d.feed(b"\x1b[1;");
        assert_eq!(d.next(false), None);
        d.feed(b"5A");
        assert_eq!(d.next(false), Some(k(KeyCode::Up, KeyModifiers::CONTROL)));
        // ESC and one byte left alone are Alt with that key; anything longer is dropped.
        assert_eq!(
            all(b"\x1b[", true),
            [k(KeyCode::Char('['), KeyModifiers::ALT)]
        );
        assert_eq!(all(b"\x1b[1;", true), []);
        // `x` ends a (meaningless) CSI; a control byte cannot be in one, so it is a key of its own.
        assert_eq!(all(b"\x1b[1;xy", false), [k(KeyCode::Char('y'), N)]);
        assert_eq!(
            all(b"\x1b[1;\x03", false),
            [k(KeyCode::Char('c'), KeyModifiers::CONTROL)]
        );
    }

    #[test]
    fn sequences_the_workspace_does_not_use_are_skipped() {
        // Mouse (SGR), focus in, an unknown `~` key, a cursor position report.
        assert_eq!(
            all(b"\x1b[<0;10;5M\x1b[I\x1b[99~\x1b[12;40Rx", false),
            [k(KeyCode::Char('x'), N)]
        );
    }

    #[test]
    fn utf8_characters_may_arrive_in_pieces() {
        let mut d = Decoder::default();
        let bytes = "草稿".as_bytes();
        d.feed(&bytes[..2]);
        assert_eq!(d.next(true), None, "half a character waits, idle or not");
        d.feed(&bytes[2..]);
        assert_eq!(d.next(false), Some(k(KeyCode::Char('草'), N)));
        assert_eq!(d.next(false), Some(k(KeyCode::Char('稿'), N)));
        // A broken start is skipped; what follows still counts, also when it is shorter than the character
        // would have been.
        assert_eq!(all(b"\xe8a\xff", false), [k(KeyCode::Char('a'), N)]);
        assert_eq!(all(b"\xe8a", false), [k(KeyCode::Char('a'), N)]);
    }

    #[test]
    fn a_bracketed_paste_is_one_event_however_it_arrives() {
        let mut d = Decoder::default();
        d.feed(b"\x1b[200~line one\r\n");
        assert_eq!(d.next(true), None, "a paste waits for its end, idle or not");
        d.feed("草\x1b[201".as_bytes());
        assert_eq!(d.next(false), None);
        d.feed(b"~x");
        assert_eq!(
            d.next(false),
            Some(Event::Paste("line one\r\n草".to_owned()))
        );
        assert_eq!(d.next(false), Some(k(KeyCode::Char('x'), N)));
        // The next paste is searched from its own start.
        d.feed(b"\x1b[200~z\x1b[201~");
        assert_eq!(d.next(false), Some(Event::Paste("z".to_owned())));
        let big = "y".repeat(5000);
        assert_eq!(
            all(format!("\x1b[200~{big}\x1b[201~").as_bytes(), false),
            [Event::Paste(big)]
        );
    }

    /// A lone ESC may be the Esc key and is settled soon; once a sequence has begun (`ESC [`, `ESC O`, a paste
    /// marker, a character in pieces) the rest is waited for longer, as a slow link may split it (M1T08R-20).
    #[test]
    fn pending_says_whether_a_lone_esc_or_a_begun_sequence_waits() {
        let pending = |bytes: &[u8]| {
            let mut d = Decoder::default();
            d.feed(bytes);
            assert_eq!(d.next(false), None, "{bytes:?}");
            d.pending()
        };
        assert_eq!(Decoder::default().pending(), Pending::Nothing);
        assert_eq!(pending(b"\x1b"), Pending::Esc);
        for begun in [
            &b"\x1b["[..],
            b"\x1bO",
            b"\x1b[1;",
            b"\x1b[20",
            b"\x1b[200~par",
            b"\x1b[[",
            b"\x1b[M !",
            "草".as_bytes().get(..2).unwrap_or_default(),
        ] {
            assert_eq!(pending(begun), Pending::Sequence, "{begun:?}");
        }
    }

    /// The Linux console's F1-F5 are `ESC [ [` and a letter (terminfo `linux` kf1-kf5): the letter is part of the
    /// key, not typed text (M1T08R-16).
    #[test]
    fn linux_console_function_keys_are_skipped_whole() {
        for f in b'A'..=b'E' {
            assert_eq!(
                all(&[0x1b, b'[', b'[', f, b'x'], false),
                [k(KeyCode::Char('x'), N)],
                "{}",
                char::from(f)
            );
        }
    }

    /// An X10 mouse report is `ESC [ M` and three raw bytes (button, column, row), which may be anything: none of
    /// them is typed text (M1T08R-17).
    #[test]
    fn an_x10_mouse_report_is_skipped_with_its_three_bytes() {
        assert_eq!(all(b"\x1b[M !!x", false), [k(KeyCode::Char('x'), N)]);
        assert_eq!(
            all(b"\x1b[M\xe0\xa1\xa1x", false),
            [k(KeyCode::Char('x'), N)]
        );
        assert_eq!(
            all(b"\x1b[M\x1b\x1b\x1bx", false),
            [k(KeyCode::Char('x'), N)]
        );
        // Parameters make it another sequence: SGR mouse and the like end at `M` as before.
        assert_eq!(all(b"\x1b[<0;1;1Mx", false), [k(KeyCode::Char('x'), N)]);
    }

    /// kitty sends some combinations as `CSI codepoint ; modifiers u` even in its legacy mode (Ctrl+Shift+R is
    /// `ESC [ 114 ; 6 u`); they decode as crossterm decodes them (M1T08R-23).
    #[test]
    fn csi_u_keys_decode_as_crossterm_does() {
        use ratatui::crossterm::event::KeyEventKind;
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(
            all(
                b"\x1b[114;6u\x1b[99;5u\x1b[13u\x1b[9;2u\x1b[127;3u\x1b[27u",
                false
            ),
            [
                k(KeyCode::Char('r'), ctrl_shift),
                k(KeyCode::Char('c'), KeyModifiers::CONTROL),
                k(KeyCode::Enter, N),
                k(KeyCode::BackTab, KeyModifiers::SHIFT),
                k(KeyCode::Backspace, KeyModifiers::ALT),
                k(KeyCode::Esc, N),
            ]
        );
        // `KeyEvent` equality folds Shift into the letter's case, so codes, modifiers and kinds are compared as they
        // are.
        let raw: Vec<(KeyCode, KeyModifiers, KeyEventKind)> = all(
            b"\x1b[114;6u\x1b[114;6:2u\x1b[114;6:3u\x1b[97:65;2u\x1b[97;2u",
            false,
        )
        .into_iter()
        .filter_map(|e| match e {
            Event::Key(key) => Some((key.code, key.modifiers, key.kind)),
            _ => None,
        })
        .collect();
        let r = KeyCode::Char('r');
        assert_eq!(
            raw,
            [
                (r, ctrl_shift, KeyEventKind::Press),
                (r, ctrl_shift, KeyEventKind::Repeat),
                (r, ctrl_shift, KeyEventKind::Release),
                (KeyCode::Char('A'), N, KeyEventKind::Press),
                (KeyCode::Char('a'), KeyModifiers::SHIFT, KeyEventKind::Press),
            ]
        );
        // kitty's own functional keys (private use codepoints) are not keys the Workspace uses.
        assert_eq!(all(b"\x1b[57399ux", false), [k(KeyCode::Char('x'), N)]);
    }

    /// The event type after the modifiers (`:2` repeat, `:3` release) is a number on every CSI key, as in
    /// crossterm: a released key is not pressed again (M1T08R-28).
    #[test]
    fn the_event_type_of_any_csi_key_is_read_as_a_number() {
        use ratatui::crossterm::event::KeyEventKind::{Press, Release, Repeat};
        let raw: Vec<_> = all(
            b"\x1b[99;5:03u\x1b[3;1:3~\x1b[1;1:2A\x1b[1;5:3C\x1b[3;5~\x1b[1;3D",
            false,
        )
        .into_iter()
        .filter_map(|e| match e {
            Event::Key(key) => Some((key.code, key.modifiers, key.kind)),
            _ => None,
        })
        .collect();
        assert_eq!(
            raw,
            [
                (KeyCode::Char('c'), KeyModifiers::CONTROL, Release),
                (KeyCode::Delete, N, Release),
                (KeyCode::Up, N, Repeat),
                (KeyCode::Right, KeyModifiers::CONTROL, Release),
                (KeyCode::Delete, KeyModifiers::CONTROL, Press),
                (KeyCode::Left, KeyModifiers::ALT, Press),
            ]
        );
    }

    /// A CSI u whose codepoint is no Unicode scalar value is no key, whatever its alternate (M1T08R-29).
    #[test]
    fn an_invalid_csi_u_codepoint_is_no_key_even_with_an_alternate() {
        for bad in [
            &b"\x1b[55296:65;2ux"[..],
            b"\x1b[1114112:65;2ux",
            b"\x1b[4294967295:65;2ux",
            b"\x1b[99999999999:65;2ux",
            b"\x1b[55296ux",
        ] {
            assert_eq!(all(bad, false), [k(KeyCode::Char('x'), N)], "{bad:?}");
        }
    }

    /// Alt and a character arriving in pieces waits for its rest as the character alone does, past any idle wait
    /// (M1T08R-24).
    #[test]
    fn alt_and_a_character_in_pieces_waits_for_its_rest() {
        let mut d = Decoder::default();
        let bytes = "草".as_bytes();
        d.feed(&[0x1b, bytes[0], bytes[1]]);
        assert_eq!(d.next(true), None, "idle or not");
        assert_eq!(d.pending(), Pending::Sequence);
        d.feed(&bytes[2..]);
        assert_eq!(
            d.next(false),
            Some(k(KeyCode::Char('草'), KeyModifiers::ALT))
        );
    }

    /// A paste arriving in reads of 4 KiB is searched for its end marker once, not again from its start on every read
    /// (M1T08R-18): 4 MiB takes well under a second even in a debug build, where searching it afresh each read takes
    /// over ten.
    #[test]
    fn a_long_paste_in_pieces_is_searched_once() {
        let body = "p".repeat(4 << 20);
        let mut d = Decoder::default();
        let started = std::time::Instant::now();
        d.feed(PASTE_START);
        for piece in body.as_bytes().chunks(4096) {
            d.feed(piece);
            assert_eq!(d.next(false), None);
        }
        d.feed(PASTE_END);
        let pasted = d.next(false);
        let took = started.elapsed();
        assert_eq!(
            pasted.map(|e| matches!(e, Event::Paste(t) if t == body)),
            Some(true)
        );
        assert!(took < std::time::Duration::from_secs(3), "took {took:?}");
    }

    #[test]
    fn a_burst_larger_than_any_read_is_decoded_in_full() {
        let burst = "x".repeat(5000) + "\r";
        let events = all(burst.as_bytes(), false);
        assert_eq!(events.len(), 5001);
        assert_eq!(events[5000], k(KeyCode::Enter, N));
    }
}
