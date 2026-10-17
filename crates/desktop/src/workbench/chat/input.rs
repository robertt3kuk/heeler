//! The text of a text field: a string with a caret, an optional selection, the marked (composing)
//! range of an input method, and an undo history. No GPUI; the field's element and key handler
//! feed it (`workbench::text_field`).

use std::ops::Range;

/// Most undo steps kept.
const HISTORY_MAX: usize = 200;

/// A text and a caret between two of its characters, optionally with a selection from an anchor
/// to the caret and a marked range (text an input method is composing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Byte offset, always on a character boundary.
    cursor: usize,
    /// The other end of the selection (a character boundary); the caret is one end. `None`
    /// (or equal to the caret) when nothing is selected.
    anchor: Option<usize>,
    /// The bytes an input method is composing (character boundaries). Drawn underlined; the
    /// next committed text replaces it.
    marked: Option<Range<usize>>,
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
        self.anchor = None;
        self.marked = None;
        self.settle(EditKind::Other);
    }

    /// Replaces the text without leaving an undo step: the history starts over. For text that
    /// changed behind the input's back (a field that mirrors a string somebody else edits).
    pub fn reset_to(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
        self.anchor = None;
        self.marked = None;
        self.history = History::default();
    }

    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.note(EditKind::Other);
        }
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
        self.marked = None;
        self.settle(EditKind::Other);
    }

    /// Takes the text out, leaving the input empty and its history forgotten (the text is gone,
    /// there is nothing to undo into).
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.anchor = None;
        self.marked = None;
        self.history = History::default();
        std::mem::take(&mut self.text)
    }

    /// Types `text` at the caret; it replaces the selection when there is one.
    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(selection) = self.selection() {
            self.replace_range(selection, text);
            return;
        }
        self.note(EditKind::Typing);
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.settle(EditKind::Typing);
    }

    /// Removes the bytes `range` (character boundaries inside the text; anything else is
    /// ignored) and puts the caret where the range began.
    pub fn remove_range(&mut self, range: Range<usize>) {
        if self.is_range(&range) {
            if range.start < range.end {
                self.note(EditKind::Other);
            }
            self.text.drain(range.clone());
            self.cursor = range.start;
            self.anchor = None;
            self.marked = None;
            self.settle(EditKind::Other);
        }
    }

    /// Replaces the bytes `range` (character boundaries inside the text; anything else is
    /// ignored) with `text` and puts the caret after it.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        if self.is_range(&range) {
            if range.start < range.end || !text.is_empty() {
                self.note(EditKind::Other);
            }
            self.text.replace_range(range.clone(), text);
            self.cursor = range.start + text.len();
            self.anchor = None;
            self.marked = None;
            self.settle(EditKind::Other);
        }
    }

    fn is_range(&self, range: &Range<usize>) -> bool {
        range.start <= range.end
            && range.end <= self.text.len()
            && self.text.is_char_boundary(range.start)
            && self.text.is_char_boundary(range.end)
    }

    /// Puts the caret at byte offset `at`, or at the nearest character boundary before it. The
    /// selection goes away.
    pub fn set_cursor(&mut self, at: usize) {
        self.cursor = self.boundary_at_or_before(at);
        self.anchor = None;
        self.marked = None;
    }

    fn boundary_at_or_before(&self, at: usize) -> usize {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.note(EditKind::Backspacing);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
            self.settle(EditKind::Backspacing);
        }
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(next) = self.next_boundary(self.cursor) {
            self.note(EditKind::Other);
            self.text.drain(self.cursor..next);
            self.settle(EditKind::Other);
        }
    }

    // ---- selection -------------------------------------------------------------------------

    /// The selected bytes; `None` when nothing (an empty range included) is selected.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?.min(self.text.len());
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// The selected text.
    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|range| &self.text[range])
    }

    /// Whether the selection runs from the caret backwards (the caret is its start).
    pub fn selection_reversed(&self) -> bool {
        self.selection()
            .is_some_and(|range| range.start == self.cursor)
    }

    /// Selects everything; the caret goes to the end.
    pub fn select_all(&mut self) {
        self.marked = None;
        self.anchor = (!self.text.is_empty()).then_some(0);
        self.cursor = self.text.len();
    }

    /// Selects from `anchor` to `cursor` (each at or before the given offset's character
    /// boundary).
    pub fn select(&mut self, anchor: usize, cursor: usize) {
        let (anchor, cursor) = (
            self.boundary_at_or_before(anchor),
            self.boundary_at_or_before(cursor),
        );
        self.marked = None;
        self.anchor = (anchor != cursor).then_some(anchor);
        self.cursor = cursor;
    }

    /// Moves the caret with `motion` and keeps the selection's other end where it was (or at
    /// the caret's old place): what holding Shift does.
    pub fn extend(&mut self, motion: impl FnOnce(&mut Self)) {
        let anchor = self.anchor.unwrap_or(self.cursor);
        self.anchor = None;
        motion(self);
        self.anchor = (anchor != self.cursor).then_some(anchor);
    }

    /// Drags the selection's caret end to `at`, keeping its other end (the caret's old place
    /// when nothing was selected).
    pub fn extend_to(&mut self, at: usize) {
        self.extend(|input| input.cursor = input.boundary_at_or_before(at));
    }

    /// The bytes of the word around `at`: a run of letters and digits, of spaces, or of other
    /// characters (a line break is never part of one).
    pub fn word_range_at(&self, at: usize) -> Range<usize> {
        let at = self.boundary_at_or_before(at);
        let class = |c: char| match c {
            c if c.is_alphanumeric() || c == '_' => 0,
            c if c.is_whitespace() => 1,
            _ => 2,
        };
        let Some(pivot) = self.text[at..]
            .chars()
            .next()
            .or_else(|| self.text[..at].chars().next_back())
            .filter(|c| *c != '\n')
        else {
            return at..at;
        };
        let same = |c: char| c != '\n' && class(c) == class(pivot);
        let start = self.text[..at]
            .char_indices()
            .rev()
            .take_while(|(_, c)| same(*c))
            .last()
            .map_or(at, |(i, _)| i);
        let end = self.text[at..]
            .char_indices()
            .take_while(|(_, c)| same(*c))
            .last()
            .map_or(at, |(i, c)| at + i + c.len_utf8());
        start..end
    }

    /// Selects the word around `at` (a double click).
    pub fn select_word_at(&mut self, at: usize) {
        let range = self.word_range_at(at);
        self.select(range.start, range.end);
    }

    /// Selects the line around `at` (a triple click).
    pub fn select_line_at(&mut self, at: usize) {
        let at = self.boundary_at_or_before(at);
        self.select(self.line_start(at), self.line_end(at));
    }

    /// Removes the selection's text; `true` when there was one.
    fn delete_selection(&mut self) -> bool {
        let Some(selection) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.remove_range(selection);
        true
    }

    /// Removes the selection and returns its text (⌘X).
    pub fn cut(&mut self) -> Option<String> {
        let text = self.selected_text()?.to_owned();
        self.delete_selection();
        Some(text)
    }

    // ---- input methods ---------------------------------------------------------------------

    /// The bytes an input method is composing.
    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// Ends the composition: the marked text stays as typed.
    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// The bytes an input method's edit targets when it names no range: the composition, else
    /// the selection, else the caret.
    fn target_range(&self) -> Range<usize> {
        self.marked
            .clone()
            .or_else(|| self.selection())
            .unwrap_or(self.cursor..self.cursor)
    }

    /// Commits `text` over `range` (bytes; the composition, the selection or the caret when
    /// `None`) and ends the composition. A composition's undo step was opened when it began.
    pub fn commit(&mut self, range: Option<Range<usize>>, text: &str) {
        let composing = self.marked.is_some();
        let range = range.unwrap_or_else(|| self.target_range());
        if !self.is_range(&range) {
            return;
        }
        if composing {
            self.text.replace_range(range.clone(), text);
            self.cursor = range.start + text.len();
            self.anchor = None;
            self.marked = None;
            self.history.last = None;
        } else if range.start == range.end && range.start == self.cursor {
            self.insert(text);
        } else {
            self.replace_range(range, text);
        }
    }

    /// Replaces `range` (bytes; the composition, the selection or the caret when `None`) with
    /// `text` and marks it as being composed. `selected` is the part of `text` (bytes) the
    /// input method has selected; the caret goes to its end, an empty one by default at the
    /// end of `text`. Empty `text` ends the composition.
    pub fn mark(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        let composing = self.marked.is_some();
        let range = range.unwrap_or_else(|| self.target_range());
        if !self.is_range(&range) {
            return;
        }
        if !composing {
            // One undo step for the whole composition, taken before it changes anything.
            self.note(EditKind::Other);
        }
        self.text.replace_range(range.clone(), text);
        self.history.last = None;
        let start = range.start;
        if text.is_empty() {
            self.marked = None;
            self.cursor = start;
            self.anchor = None;
            return;
        }
        self.marked = Some(start..start + text.len());
        let end = text.len();
        let mut selected = selected.map_or(end..end, |s| s.start.min(end)..s.end.min(end));
        if !text.is_char_boundary(selected.start) || !text.is_char_boundary(selected.end) {
            selected = end..end;
        }
        self.cursor = start + selected.end;
        self.anchor = (selected.start != selected.end).then_some(start + selected.start);
    }

    // ---- UTF-16 (what the platform's input methods count in) --------------------------------

    /// The UTF-16 offset of byte offset `at`.
    pub fn utf16_offset(&self, at: usize) -> usize {
        utf16_len(&self.text[..self.boundary_at_or_before(at)])
    }

    /// The byte offset of UTF-16 offset `offset`: the start of the character that contains it.
    pub fn byte_offset(&self, offset: usize) -> usize {
        utf16_to_byte(&self.text, offset)
    }

    /// `range` (bytes) in UTF-16.
    pub fn utf16_range(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16_offset(range.start)..self.utf16_offset(range.end)
    }

    /// `range` (UTF-16) in bytes; a surrogate pair cut in the middle is taken whole.
    pub fn byte_range(&self, range: &Range<usize>) -> Range<usize> {
        let start = self.byte_offset(range.start);
        let mut end = self.byte_offset(range.end);
        if end < self.text.len() && self.utf16_offset(end) < range.end {
            end = self.next_boundary(end).unwrap_or(end);
        }
        start..end.max(start)
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
        self.marked = None;
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
        self.anchor = None;
        self.marked = None;
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
        self.anchor = None;
        self.marked = None;
        true
    }

    /// ← : one character back; with a selection, its start.
    pub fn left(&mut self) {
        if let Some(selection) = self.selection() {
            self.move_to(selection.start);
        } else if let Some(prev) = self.prev_boundary(self.cursor) {
            self.move_to(prev);
        }
    }

    /// → : one character on; with a selection, its end.
    pub fn right(&mut self) {
        if let Some(selection) = self.selection() {
            self.move_to(selection.end);
        } else if let Some(next) = self.next_boundary(self.cursor) {
            self.move_to(next);
        }
    }

    /// Puts the caret at `at` (a character boundary) and drops the selection.
    fn move_to(&mut self, at: usize) {
        self.cursor = at;
        self.anchor = None;
        self.marked = None;
    }

    /// Start of the caret's line.
    pub fn home(&mut self) {
        self.move_to(self.line_start(self.cursor));
    }

    /// End of the caret's line.
    pub fn end(&mut self) {
        self.move_to(self.line_end(self.cursor));
    }

    /// Start of the text (⌘↑).
    pub fn text_start(&mut self) {
        self.move_to(0);
    }

    /// End of the text (⌘↓).
    pub fn text_end(&mut self) {
        self.move_to(self.text.len());
    }

    pub fn word_left(&mut self) {
        self.move_to(self.word_start(self.cursor));
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
        self.move_to(at);
    }

    /// Deletes from the start of the previous word to the caret (⌥⌫); the selection if there
    /// is one.
    pub fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = self.word_start(self.cursor);
        if start < self.cursor {
            self.note(EditKind::Other);
            self.text.drain(start..self.cursor);
            self.cursor = start;
            self.settle(EditKind::Other);
        }
    }

    /// Deletes from the caret to the start of its line (⌘⌫); the selection if there is one.
    pub fn delete_to_line_start(&mut self) {
        if self.delete_selection() {
            return;
        }
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
        self.anchor = None;
        self.marked = None;
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
        self.anchor = None;
        self.marked = None;
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

/// The length of `text` in UTF-16 code units (what the platform's input methods count in).
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The byte offset in `text` of UTF-16 offset `units`: the start of the character that contains
/// it, or the end of the text.
pub fn utf16_to_byte(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (at, c) in text.char_indices() {
        if seen + c.len_utf16() > units {
            return at;
        }
        seen += c.len_utf16();
    }
    text.len()
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

    #[test]
    fn utf16_ranges_round_trip_through_emoji_cyrillic_and_cjk() {
        let i = input("aй日😀b");
        // a=1, й=1, 日=1, 😀=2, b=1 UTF-16 units.
        assert_eq!(utf16_len(i.text()), 6);
        let emoji = "aй日".len();
        assert_eq!(i.utf16_offset(emoji), 3);
        assert_eq!(i.byte_offset(3), emoji);
        let emoji_end = emoji + "😀".len();
        assert_eq!(i.utf16_range(&(emoji..emoji_end)), 3..5);
        assert_eq!(i.byte_range(&(3..5)), emoji..emoji_end);
        // A range ending inside the surrogate pair takes the whole character.
        assert_eq!(i.byte_range(&(3..4)), emoji..emoji_end);
        assert_eq!(
            i.byte_offset(4),
            emoji,
            "inside a pair is the start of the character"
        );
        assert_eq!(i.byte_offset(99), i.text().len());
        assert_eq!(
            i.utf16_offset(emoji + 1),
            3,
            "inside a character rounds down"
        );
    }

    #[test]
    fn typing_replaces_the_selection_and_shift_motions_select() {
        let mut i = input("hello world");
        i.select_all();
        assert_eq!(i.selected_text(), Some("hello world"));
        i.insert("ы");
        assert_eq!(i.text(), "ы");
        assert!(i.selection().is_none());
        i.set("one two");
        i.extend(TextInput::word_left);
        assert_eq!(i.selected_text(), Some("two"));
        assert!(
            i.selection_reversed(),
            "the caret is at the start of the word"
        );
        i.extend(TextInput::left);
        assert_eq!(i.selected_text(), Some(" two"));
        i.left();
        assert_eq!(
            i.cursor(),
            "one".len(),
            "← with a selection goes to its start"
        );
        assert!(i.selection().is_none());
        i.select_all();
        i.backspace();
        assert!(i.is_empty());
        assert!(i.undo());
        assert_eq!(i.text(), "one two");
    }

    #[test]
    fn cut_and_word_selection() {
        let mut i = input("alpha beta  gamma");
        i.select_word_at(8);
        assert_eq!(i.selected_text(), Some("beta"));
        i.select_word_at(10);
        assert_eq!(i.selected_text(), Some("  "));
        i.select_line_at(3);
        assert_eq!(i.cut().as_deref(), Some("alpha beta  gamma"));
        assert!(i.is_empty());
        assert_eq!(i.cut(), None);
    }

    #[test]
    fn a_composition_is_marked_then_committed_in_one_undo_step() {
        let mut i = TextInput::new();
        i.reset_to("ab");
        i.left();
        // The input method shows 'に' while composing, then 'にほん', then commits '日本'.
        i.mark(None, "に", None);
        assert_eq!(i.text(), "aにb");
        assert_eq!(i.marked(), Some(1..4));
        assert_eq!(i.cursor(), 4);
        i.mark(None, "にほん", Some(0..3));
        assert_eq!(i.text(), "aにほんb");
        assert_eq!(i.marked(), Some(1..10));
        assert_eq!(i.selection(), Some(1..4), "the segment being converted");
        i.commit(None, "日本");
        assert_eq!(i.text(), "a日本b");
        assert_eq!(i.marked(), None);
        assert_eq!(i.cursor(), "a日本".len());
        assert!(i.undo());
        assert_eq!(i.text(), "ab", "the composition undoes as one step");
        assert!(!i.undo());
    }

    #[test]
    fn committing_without_a_composition_is_typing() {
        let mut i = input("");
        i.commit(None, "ф");
        i.commit(None, "ы");
        assert_eq!(i.text(), "фы");
        assert!(i.undo(), "typed runs undo together");
        assert!(i.is_empty());
        // An explicit range replaces: press-and-hold accent popup replaces the typed 'a'.
        i.commit(None, "a");
        i.commit(Some(0..1), "à");
        assert_eq!(i.text(), "à");
        // An empty marked text ends the composition without leaving text behind.
        i.mark(None, "x", None);
        i.mark(None, "", None);
        assert_eq!(i.text(), "à");
        assert_eq!(i.marked(), None);
    }
}
