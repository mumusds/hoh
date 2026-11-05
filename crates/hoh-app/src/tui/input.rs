//! Terminal input decoded by HoH itself (ADR-014 decision 1). crossterm 0.29 still draws and switches the terminal's
//! modes, but its event reader is never started: once the terminal is at end of file it retries a zero-byte read
//! forever (M1QA-01), after an unfinished escape sequence it blocks in read(2) for the rest (M1T08R-13), and a read
//! it has entered cannot be called off, so a reader thread could take the shell's next command (M1T08R-12, -14). The
//! TUI loop instead waits in poll(2) itself and reads only what poll said is there; this decoder turns those bytes
//! into the same `Event`s crossterm's parser gives for the keys the Workspace uses, and never waits.

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

const ESC: u8 = 0x1b;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Bytes read from the terminal, not yet decoded.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
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

    /// Whether bytes are waiting: the caller then waits only briefly for the rest of a sequence.
    pub fn pending(&self) -> bool {
        !self.buf.is_empty()
    }

    /// The next decoded event. `idle` says no more input came after the last read: then a lone ESC is the Esc key,
    /// ESC and one byte are Alt with that key, and a longer unfinished sequence is dropped. An unfinished paste or
    /// UTF-8 character keeps waiting for its rest.
    pub fn next(&mut self, idle: bool) -> Option<Event> {
        loop {
            match front(&self.buf) {
                Front::Event(event, used) => {
                    self.buf.drain(..used);
                    return Some(event);
                }
                Front::Skip(used) => {
                    self.buf.drain(..used);
                }
                Front::Unfinished if !idle || self.buf.first() != Some(&ESC) => return None,
                Front::Unfinished if self.buf.starts_with(PASTE_START) => return None,
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

/// `ESC [` parameters, intermediates and a final byte (ECMA-48); bracketed paste runs to its end marker.
fn csi(buf: &[u8]) -> Front {
    if buf.starts_with(PASTE_START) {
        let body = &buf[PASTE_START.len()..];
        return match body.windows(PASTE_END.len()).position(|w| w == PASTE_END) {
            Some(end) => Front::Event(
                Event::Paste(String::from_utf8_lossy(&body[..end]).into_owned()),
                PASTE_START.len() + end + PASTE_END.len(),
            ),
            None => Front::Unfinished,
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

/// The key a complete CSI names, with the modifiers of its second parameter (`1;5C` is Ctrl+Right).
fn csi_key(params: &[u8], fin: u8) -> Option<Event> {
    let text = std::str::from_utf8(params).ok()?;
    let mut fields = text.split(';');
    let first = fields.next().unwrap_or_default();
    let modifiers = fields
        .next()
        .and_then(|m| m.split(':').next())
        .and_then(|m| m.parse::<u8>().ok())
        .map_or(KeyModifiers::NONE, modifiers);
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
    Some(key(code, modifiers))
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
        assert!(d.pending());
        assert_eq!(d.next(true), Some(k(KeyCode::Esc, N)));
        assert!(!d.pending());
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
        let big = "y".repeat(5000);
        assert_eq!(
            all(format!("\x1b[200~{big}\x1b[201~").as_bytes(), false),
            [Event::Paste(big)]
        );
    }

    #[test]
    fn a_burst_larger_than_any_read_is_decoded_in_full() {
        let burst = "x".repeat(1100) + "\r";
        let events = all(burst.as_bytes(), false);
        assert_eq!(events.len(), 1101);
        assert_eq!(events[1100], k(KeyCode::Enter, N));
    }
}
