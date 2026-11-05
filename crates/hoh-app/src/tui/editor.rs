//! The single-line input editor: the cursor moves by grapheme cluster and its column is the display width of the text
//! before it (docs/03: wide characters by display width, not bytes). Multi-line editing is REQ-015 (M4).

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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
        }
    }

    pub fn delete(&mut self) {
        if let Some(end) = self.next() {
            self.text.replace_range(self.cursor..end, "");
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

    /// Display width of the text before the cursor.
    #[cfg(test)]
    pub fn cursor_column(&self) -> usize {
        self.text[..self.cursor].width()
    }

    /// The part of the text to show in `width` columns so that the cursor stays visible, and the cursor's column in
    /// it: whole graphemes are dropped from the front until the text before the cursor fits.
    pub fn view(&self, width: usize) -> (&str, usize) {
        let mut start = 0;
        let room = width.saturating_sub(1);
        while self.text[start..self.cursor].width() > room {
            match self.text[start..].graphemes(true).next() {
                Some(g) => start += g.len(),
                None => break,
            }
        }
        (&self.text[start..], self.text[start..self.cursor].width())
    }
}
