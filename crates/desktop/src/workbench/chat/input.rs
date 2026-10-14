//! The composer's text: a string with a cursor, edited by the keys a multi-line text field
//! answers to. No GPUI; the view feeds it keystrokes.

/// A multi-line text and a caret between two of its characters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Byte offset, always on a character boundary.
    cursor: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the text; the caret goes to its end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Takes the text out, leaving the input empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary(self.cursor) {
            self.text.drain(self.cursor..next);
        }
    }

    pub fn left(&mut self) {
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.cursor = prev;
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.next_boundary(self.cursor) {
            self.cursor = next;
        }
    }

    /// Start of the caret's line.
    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    /// End of the caret's line.
    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start(self.cursor);
    }

    pub fn word_right(&mut self) {
        let mut at = self.cursor;
        let chars = |text: &str, at: usize| text[at..].chars().next();
        while let Some(c) = chars(&self.text, at) {
            if c.is_alphanumeric() {
                break;
            }
            at += c.len_utf8();
        }
        while let Some(c) = chars(&self.text, at) {
            if !c.is_alphanumeric() {
                break;
            }
            at += c.len_utf8();
        }
        self.cursor = at;
    }

    /// Deletes from the start of the previous word to the caret (⌥⌫).
    pub fn delete_word_back(&mut self) {
        let start = self.word_start(self.cursor);
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Deletes from the caret to the start of its line (⌘⌫).
    pub fn delete_to_line_start(&mut self) {
        let start = self.line_start(self.cursor);
        if start == self.cursor {
            self.backspace();
        } else {
            self.text.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    /// Moves the caret one line up, to the same column when the line is long enough. On the
    /// first line it goes to the start of the text. `false` when it did not move.
    pub fn up(&mut self) -> bool {
        let start = self.line_start(self.cursor);
        if start == 0 {
            let moved = self.cursor != 0;
            self.cursor = 0;
            return moved;
        }
        let column = self.text[start..self.cursor].chars().count();
        let prev_end = start - 1;
        let prev_start = self.line_start(prev_end);
        self.cursor = self.nth_char(prev_start, prev_end, column);
        true
    }

    /// Moves the caret one line down; on the last line it goes to the end of the text.
    pub fn down(&mut self) -> bool {
        let end = self.line_end(self.cursor);
        if end == self.text.len() {
            let moved = self.cursor != end;
            self.cursor = end;
            return moved;
        }
        let start = self.line_start(self.cursor);
        let column = self.text[start..self.cursor].chars().count();
        let next_start = end + 1;
        let next_end = self.line_end(next_start);
        self.cursor = self.nth_char(next_start, next_end, column);
        true
    }

    fn prev_boundary(&self, at: usize) -> Option<usize> {
        self.text[..at].char_indices().next_back().map(|(i, _)| i)
    }

    fn next_boundary(&self, at: usize) -> Option<usize> {
        self.text[at..].chars().next().map(|c| at + c.len_utf8())
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |i| at + i)
    }

    /// The offset of the `column`-th character of the line `start..end` (its end when shorter).
    fn nth_char(&self, start: usize, end: usize, column: usize) -> usize {
        self.text[start..end]
            .char_indices()
            .nth(column)
            .map_or(end, |(i, _)| start + i)
    }

    fn word_start(&self, at: usize) -> usize {
        let mut at = at;
        while let Some(prev) = self.prev_boundary(at) {
            if self.text[prev..at]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
            {
                break;
            }
            at = prev;
        }
        while let Some(prev) = self.prev_boundary(at) {
            if !self.text[prev..at]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
            {
                break;
            }
            at = prev;
        }
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> TextInput {
        let mut input = TextInput::new();
        input.set(text);
        input
    }

    #[test]
    fn typing_inserts_at_the_caret() {
        let mut i = input("ac");
        i.left();
        i.insert("b");
        assert_eq!(i.text(), "abc");
        assert_eq!(i.cursor(), 2);
        i.insert("é\n");
        assert_eq!(i.text(), "abé\nc");
    }

    #[test]
    fn backspace_and_delete_respect_characters() {
        let mut i = input("aé✓");
        i.backspace();
        assert_eq!(i.text(), "aé");
        i.left();
        i.delete();
        assert_eq!(i.text(), "a");
        i.backspace();
        i.backspace();
        assert!(i.is_empty());
        i.delete();
        assert_eq!(i.cursor(), 0);
    }

    #[test]
    fn home_and_end_stay_on_their_line() {
        let mut i = input("one\ntwo");
        i.home();
        assert_eq!(i.cursor(), 4);
        i.left();
        i.home();
        assert_eq!(i.cursor(), 0);
        i.end();
        assert_eq!(i.cursor(), 3);
    }

    #[test]
    fn up_and_down_keep_the_column() {
        let mut i = input("abcdef\nxy\nlonger line");
        // The caret sits at the end (column 11 of the last line).
        assert!(i.up());
        assert_eq!(i.cursor(), "abcdef\nxy".len(), "the short line's end");
        assert!(i.up());
        assert_eq!(i.cursor(), 2, "column 2 of the short line carried up");
        assert!(i.up());
        assert_eq!(i.cursor(), 0, "the first line's up goes to the start");
        assert!(!i.up());
        assert!(i.down());
        assert_eq!(i.cursor(), 7);
        i.end();
        assert!(i.down());
        assert!(i.down());
        assert_eq!(i.cursor(), i.text().len());
        assert!(!i.down());
    }

    #[test]
    fn word_moves_and_deletes() {
        let mut i = input("foo bar  baz");
        i.word_left();
        assert_eq!(i.cursor(), 9);
        i.word_left();
        assert_eq!(i.cursor(), 4);
        i.word_right();
        assert_eq!(i.cursor(), 7);
        i.delete_word_back();
        assert_eq!(i.text(), "foo   baz");
        i.end();
        i.delete_word_back();
        assert_eq!(i.text(), "foo   ");
    }

    #[test]
    fn command_backspace_deletes_to_the_line_start() {
        let mut i = input("keep\ndrop this");
        i.delete_to_line_start();
        assert_eq!(i.text(), "keep\n");
        i.delete_to_line_start();
        assert_eq!(i.text(), "keep");
    }

    #[test]
    fn take_empties_the_input() {
        let mut i = input("hello");
        assert_eq!(i.take(), "hello");
        assert!(i.is_empty());
        assert_eq!(i.cursor(), 0);
    }
}
