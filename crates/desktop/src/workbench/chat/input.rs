//! The composer's text: a string with a cursor, edited by the keys a multi-line text field
//! answers to, with an undo history. No GPUI; the view feeds it keystrokes.

/// Most undo steps kept.
const HISTORY_MAX: usize = 200;

/// A multi-line text and a caret between two of its characters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Byte offset, always on a character boundary.
    cursor: usize,
    history: History,
}

/// What one edit does, to decide whether it joins the undo step before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    /// Typed characters: a run of them undoes in one step.
    Typing,
    /// Backspaces: a run of them undoes in one step.
    Backspacing,
    /// Anything else: a step of its own.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    cursor: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The kind of the last edit and where its caret ended up: the next edit of the same kind
    /// starting there joins its step.
    last: Option<(EditKind, usize)>,
    /// Edits are being grouped by the caller (one step for all of them).
    grouped: bool,
}

impl History {
    fn push(&mut self, snapshot: Snapshot) {
        self.redo.clear();
        self.undo.push(snapshot);
        if self.undo.len() > HISTORY_MAX {
            self.undo.remove(0);
        }
    }
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
        if self.text != text {
            self.note(EditKind::Other);
        }
        self.text = text.to_owned();
        self.cursor = self.text.len();
        self.settle(EditKind::Other);
    }

    /// Replaces the text without leaving an undo step: the history starts over. For text that
    /// changed behind the input's back (a field that mirrors a string somebody else edits).
    pub fn reset_to(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
        self.history = History::default();
    }

    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.note(EditKind::Other);
        }
        self.text.clear();
        self.cursor = 0;
        self.settle(EditKind::Other);
    }

    /// Takes the text out, leaving the input empty and its history forgotten (the text is gone,
    /// there is nothing to undo into).
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.history = History::default();
        std::mem::take(&mut self.text)
    }

    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.note(EditKind::Typing);
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.settle(EditKind::Typing);
    }

    /// Removes the bytes `range` (character boundaries inside the text; anything else is
    /// ignored) and puts the caret where the range began.
    pub fn remove_range(&mut self, range: std::ops::Range<usize>) {
        if range.start <= range.end
            && range.end <= self.text.len()
            && self.text.is_char_boundary(range.start)
            && self.text.is_char_boundary(range.end)
        {
            if range.start < range.end {
                self.note(EditKind::Other);
            }
            self.text.drain(range.clone());
            self.cursor = range.start;
            self.settle(EditKind::Other);
        }
    }

    /// Replaces the bytes `range` (character boundaries inside the text; anything else is
    /// ignored) with `text` and puts the caret after it.
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        if range.start <= range.end
            && range.end <= self.text.len()
            && self.text.is_char_boundary(range.start)
            && self.text.is_char_boundary(range.end)
        {
            if range.start < range.end || !text.is_empty() {
                self.note(EditKind::Other);
            }
            self.text.replace_range(range.clone(), text);
            self.cursor = range.start + text.len();
            self.settle(EditKind::Other);
        }
    }

    /// Puts the caret at byte offset `at`, or at the nearest character boundary before it.
    pub fn set_cursor(&mut self, at: usize) {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.cursor = at;
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.note(EditKind::Backspacing);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
            self.settle(EditKind::Backspacing);
        }
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary(self.cursor) {
            self.note(EditKind::Other);
            self.text.drain(self.cursor..next);
            self.settle(EditKind::Other);
        }
    }

    // ---- undo ------------------------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        }
    }

    /// Called before an edit: opens an undo step unless the edit continues the last one.
    fn note(&mut self, kind: EditKind) {
        if self.history.grouped {
            return;
        }
        let continues = kind != EditKind::Other && self.history.last == Some((kind, self.cursor));
        if !continues {
            let snapshot = self.snapshot();
            self.history.push(snapshot);
        }
    }

    /// Called after an edit.
    fn settle(&mut self, kind: EditKind) {
        if !self.history.grouped {
            self.history.last = Some((kind, self.cursor));
        }
    }

    /// Starts a group: every edit until [`end_group`](Self::end_group) undoes in one step.
    pub fn begin_group(&mut self) {
        if !self.history.grouped {
            let snapshot = self.snapshot();
            self.history.push(snapshot);
            self.history.grouped = true;
            self.history.last = None;
        }
    }

    /// Ends the group; a group that changed nothing leaves no step behind.
    pub fn end_group(&mut self) {
        if self.history.grouped {
            self.history.grouped = false;
            self.history.last = None;
            if self
                .history
                .undo
                .last()
                .is_some_and(|step| step.text == self.text)
            {
                self.history.undo.pop();
            }
        }
    }

    /// Undoes the last step; `false` when there is none.
    pub fn undo(&mut self) -> bool {
        self.history.grouped = false;
        self.history.last = None;
        let Some(step) = self.history.undo.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.history.redo.push(current);
        self.text = step.text;
        self.cursor = step.cursor;
        true
    }

    /// Redoes the last undone step; `false` when there is none.
    pub fn redo(&mut self) -> bool {
        self.history.grouped = false;
        self.history.last = None;
        let Some(step) = self.history.redo.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.history.undo.push(current);
        self.text = step.text;
        self.cursor = step.cursor;
        true
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
        if start < self.cursor {
            self.note(EditKind::Other);
            self.text.drain(start..self.cursor);
            self.cursor = start;
            self.settle(EditKind::Other);
        }
    }

    /// Deletes from the caret to the start of its line (⌘⌫).
    pub fn delete_to_line_start(&mut self) {
        let start = self.line_start(self.cursor);
        if start == self.cursor {
            self.backspace();
        } else {
            self.note(EditKind::Other);
            self.text.drain(start..self.cursor);
            self.cursor = start;
            self.settle(EditKind::Other);
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

    #[test]
    fn typing_and_backspacing_undo_in_runs() {
        let mut i = TextInput::new();
        i.insert("a");
        i.insert("b");
        i.insert("c");
        i.backspace();
        i.backspace();
        assert_eq!(i.text(), "a");
        assert!(i.undo());
        assert_eq!(i.text(), "abc", "the backspaces were one step");
        assert!(i.undo());
        assert_eq!(i.text(), "", "the typing was one step");
        assert!(!i.undo());
        assert!(i.redo());
        assert_eq!(i.text(), "abc");
        assert!(i.redo());
        assert_eq!(i.text(), "a");
        assert!(!i.redo());
    }

    #[test]
    fn a_new_edit_drops_the_redo_steps_and_movement_splits_typing() {
        let mut i = input("ab");
        i.insert("c");
        i.left();
        i.left();
        i.insert("X");
        assert_eq!(i.text(), "aXbc");
        assert!(i.undo());
        assert_eq!(i.text(), "abc");
        i.insert("!");
        assert!(!i.redo(), "the undone step is gone");
    }

    #[test]
    fn a_group_undoes_in_one_step_and_an_empty_group_leaves_none() {
        let mut i = TextInput::new();
        i.reset_to("one two");
        i.begin_group();
        i.remove_range(0..3);
        i.insert("1");
        i.end_group();
        assert_eq!(i.text(), "1 two");
        i.begin_group();
        i.end_group();
        assert!(i.undo());
        assert_eq!(i.text(), "one two");
        assert_eq!(i.cursor(), 7, "the caret is where it was before the group");
        assert!(!i.undo(), "the empty group left nothing behind");
    }

    #[test]
    fn the_history_is_bounded_and_take_forgets_it() {
        let mut i = TextInput::new();
        for n in 0..(HISTORY_MAX + 50) {
            i.set(&n.to_string());
        }
        let mut steps = 0;
        while i.undo() {
            steps += 1;
        }
        assert_eq!(steps, HISTORY_MAX);
        let mut i = input("x");
        i.insert("y");
        i.take();
        assert!(!i.undo());
    }
}
