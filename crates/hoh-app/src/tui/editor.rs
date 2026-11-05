//! The single-line input editor: the cursor moves by grapheme cluster and its column is the display width of the text
//! before it (docs/03: wide characters by display width, not bytes). Multi-line editing is REQ-015 (M4).

use ratatui::buffer::CellWidth;
use unicode_segmentation::UnicodeSegmentation;

/// The cells a grapheme takes when ratatui draws it: ratatui measures and draws a line grapheme by grapheme, and
/// leaves out a control character (M1T08R-34).
fn cells(g: &str) -> usize {
    if g.chars().any(char::is_control) {
        0
    } else {
        usize::from(g.cell_width())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Editor {
    text: String,
    /// Byte offset of the cursor, always on a grapheme boundary.
    cursor: usize,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Insert at the cursor; line breaks become spaces (one line).
    pub fn insert(&mut self, text: &str) {
        let flat = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
        self.text.insert_str(self.cursor, &flat);
        self.cursor += flat.len();
        self.snap();
    }

    /// Move the cursor to the end of the grapheme it is in, if an edit joined graphemes around it (a zero-width
    /// joiner, a combining mark; M1T08R-03): it is always on a boundary of the whole text.
    fn snap(&mut self) {
        if let Some(boundary) = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(self.text.len()))
            .find(|b| *b >= self.cursor)
        {
            self.cursor = boundary;
        }
    }

    /// Replace the whole text, cursor at the end.
    pub fn set(&mut self, text: &str) {
        self.text.clear();
        self.cursor = 0;
        self.insert(text);
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// The text, leaving the editor empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    fn previous(&self) -> Option<usize> {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i)
    }

    fn next(&self) -> Option<usize> {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map(|g| self.cursor + g.len())
    }

    pub fn backspace(&mut self) {
        if let Some(start) = self.previous() {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
            self.snap();
        }
    }

    pub fn delete(&mut self) {
        if let Some(end) = self.next() {
            self.text.replace_range(self.cursor..end, "");
            self.snap();
        }
    }

    pub fn left(&mut self) {
        if let Some(start) = self.previous() {
            self.cursor = start;
        }
    }

    pub fn right(&mut self) {
        if let Some(end) = self.next() {
            self.cursor = end;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Display width of the text before the cursor, as drawn.
    #[cfg(test)]
    pub fn cursor_column(&self) -> usize {
        self.text[..self.cursor].graphemes(true).map(cells).sum()
    }

    #[cfg(test)]
    pub fn cursor_byte(&self) -> usize {
        self.cursor
    }

    /// The part of the text to show in `width` columns so that the cursor stays visible, and the cursor's column in
    /// it: as many whole graphemes before the cursor as fit, found walking back from it, so that a draw is linear in
    /// the line's length (M1T08R-31). Both are measured in the cells ratatui draws (M1T08R-34).
    pub fn view(&self, width: usize) -> (&str, usize) {
        let room = width.saturating_sub(1);
        let mut start = self.cursor;
        let mut column = 0;
        for (i, g) in self.text[..self.cursor].grapheme_indices(true).rev() {
            let w = cells(g);
            if column + w > room {
                break;
            }
            column += w;
            start = i;
        }
        (&self.text[start..], column)
    }
}
