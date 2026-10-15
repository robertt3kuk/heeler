//! Text selection over absolute lines (GPUI-free). See the model docs for line numbering.

use staffy_protocol::render::Row;

use crate::model::{Cell, TerminalModel, row_cells, row_width};

/// A cell position: absolute `line` (history and viewport, see [`TerminalModel`]) and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: i64,
    pub col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// Cell to cell.
    Simple,
    /// Whole words; dragging extends by words (double click).
    Word,
    /// Whole logical lines, following soft wraps (triple click).
    Line,
}

/// Inclusive selected range, ordered, with columns snapped to whole cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Point,
    pub end: Point,
}

impl Span {
    /// Selected columns `start..end` (end exclusive) on `line`, if it is inside the span.
    pub fn columns_on(&self, line: i64, cols: u16) -> Option<(u16, u16)> {
        if line < self.start.line || line > self.end.line {
            return None;
        }
        let from = if line == self.start.line {
            self.start.col
        } else {
            0
        };
        let to = if line == self.end.line {
            self.end.col.saturating_add(1)
        } else {
            cols
        };
        (from < to).then_some((from, to))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    kind: SelectionKind,
    /// The initially selected unit (a cell, word or line).
    anchor: Span,
    head: Point,
}

/// Characters, besides alphanumerics, that belong to a word (iTerm's default set).
const WORD_EXTRA: &str = "/-+\\~_.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Blank,
    Other(char),
}

fn class_of(cell: Option<&Cell<'_>>) -> Class {
    let Some(first) = cell.and_then(|c| c.text.chars().next()) else {
        return Class::Blank;
    };
    if first.is_alphanumeric() || WORD_EXTRA.contains(first) {
        Class::Word
    } else if first == ' ' || first.is_control() {
        Class::Blank
    } else {
        Class::Other(first)
    }
}

fn row_of(model: &TerminalModel, line: i64) -> Option<&Row> {
    model.row_at(line)
}

/// Columns `(start, end)` (end exclusive) of the cell under `col`; one column for blanks past
/// the end of the row's content.
fn cell_bounds(cells: &[Cell<'_>], col: u16) -> (u16, u16) {
    cells
        .iter()
        .find(|c| c.col <= col && col < c.end())
        .map_or((col, col.saturating_add(1)), |c| (c.col, c.end()))
}

impl Selection {
    pub fn new(kind: SelectionKind, at: Point, model: &TerminalModel) -> Self {
        let anchor = unit_span(kind, at, model);
        Self {
            kind,
            anchor,
            head: at,
        }
    }

    pub fn kind(&self) -> SelectionKind {
        self.kind
    }

    pub fn extend(&mut self, to: Point) {
        self.head = to;
    }

    /// The selected range, or `None` for a click that selected nothing.
    pub fn span(&self, model: &TerminalModel) -> Option<Span> {
        let head = unit_span(self.kind, self.head, model);
        let start = self.anchor.start.min(head.start);
        let end = self.anchor.end.max(head.end);
        // A click (same cell) selects nothing.
        if self.kind == SelectionKind::Simple && self.anchor.start == head.start {
            return None;
        }
        Some(Span { start, end })
    }
}

/// The unit around `at`: its cell, word or logical line.
fn unit_span(kind: SelectionKind, at: Point, model: &TerminalModel) -> Span {
    let empty = Row::default();
    let row = row_of(model, at.line).unwrap_or(&empty);
    let cells = row_cells(row);
    match kind {
        SelectionKind::Simple => {
            let (start, end) = cell_bounds(&cells, at.col);
            Span {
                start: Point {
                    line: at.line,
                    col: start,
                },
                end: Point {
                    line: at.line,
                    col: end - 1,
                },
            }
        }
        SelectionKind::Word => {
            let last_col = model.cols().saturating_sub(1);
            let content = row_width(row);
            let idx = cells
                .iter()
                .position(|c| c.col <= at.col && at.col < c.end());
            let Some(idx) = idx else {
                // Blank tail of the row.
                return Span {
                    start: Point {
                        line: at.line,
                        col: content.min(at.col),
                    },
                    end: Point {
                        line: at.line,
                        col: last_col.max(at.col),
                    },
                };
            };
            let class = class_of(Some(&cells[idx]));
            let (mut lo, mut hi) = (idx, idx);
            if !matches!(class, Class::Other(_)) {
                while lo > 0 && class_of(Some(&cells[lo - 1])) == class {
                    lo -= 1;
                }
                while hi + 1 < cells.len() && class_of(Some(&cells[hi + 1])) == class {
                    hi += 1;
                }
            }
            let mut end = cells[hi].end() - 1;
            if class == Class::Blank && hi + 1 == cells.len() {
                end = last_col.max(end);
            }
            Span {
                start: Point {
                    line: at.line,
                    col: cells[lo].col,
                },
                end: Point {
                    line: at.line,
                    col: end,
                },
            }
        }
        SelectionKind::Line => {
            let mut first = at.line;
            while row_of(model, first - 1).is_some_and(|r| r.wrapped) {
                first -= 1;
            }
            let mut last = at.line;
            while row_of(model, last).is_some_and(|r| r.wrapped)
                && row_of(model, last + 1).is_some()
            {
                last += 1;
            }
            Span {
                start: Point {
                    line: first,
                    col: 0,
                },
                end: Point {
                    line: last,
                    col: model.cols().saturating_sub(1),
                },
            }
        }
    }
}

/// The text covered by `span`. Rows end with a newline unless they soft-wrap into the next one;
/// trailing blanks at the end of a line are dropped.
pub fn selected_text(model: &TerminalModel, span: Span) -> String {
    let empty = Row::default();
    let mut out = String::new();
    for line in span.start.line..=span.end.line {
        let row = row_of(model, line).unwrap_or(&empty);
        let from = if line == span.start.line {
            span.start.col
        } else {
            0
        };
        let to = if line == span.end.line {
            span.end.col
        } else {
            u16::MAX
        };
        let start_len = out.len();
        for cell in row_cells(row) {
            if cell.col <= to && cell.end() > from {
                out.push_str(cell.text);
            }
        }
        let last = line == span.end.line;
        if last {
            if to >= row_width(row).saturating_sub(1) {
                trim_blanks(&mut out, start_len);
            }
        } else if !row.wrapped {
            trim_blanks(&mut out, start_len);
            out.push('\n');
        }
    }
    out
}

fn trim_blanks(out: &mut String, floor: usize) {
    while out.len() > floor && out.ends_with(' ') {
        out.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::*;
    use staffy_protocol::render::{Row, TerminalSnapshot};

    fn model_with(rows: Vec<Row>, cols: u16, history_len: u32) -> TerminalModel {
        let mut m = TerminalModel::new();
        let mut s: TerminalSnapshot = snapshot(1, cols, &[], history_len);
        s.rows = rows.len() as u16;
        s.lines = rows;
        m.apply_snapshot(s);
        m
    }

    fn text_model(lines: &[&str], cols: u16) -> TerminalModel {
        model_with(lines.iter().map(|l| row(l)).collect(), cols, 0)
    }

    fn p(line: i64, col: u16) -> Point {
        Point { line, col }
    }

    fn sel(model: &TerminalModel, kind: SelectionKind, a: Point, b: Point) -> Option<String> {
        let mut s = Selection::new(kind, a, model);
        s.extend(b);
        s.span(model).map(|span| selected_text(model, span))
    }

    #[test]
    fn simple_selection_within_a_row() {
        let m = text_model(&["hello world", "second"], 20);
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 0), p(0, 4)).as_deref(),
            Some("hello")
        );
        // Dragging backwards selects the same text.
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 4), p(0, 0)).as_deref(),
            Some("hello")
        );
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 6), p(0, 10)).as_deref(),
            Some("world")
        );
    }

    #[test]
    fn a_click_selects_nothing() {
        let m = text_model(&["hello"], 20);
        assert_eq!(sel(&m, SelectionKind::Simple, p(0, 2), p(0, 2)), None);
    }

    #[test]
    fn simple_selection_across_rows_joins_with_newlines() {
        let m = text_model(&["one  ", "two", "three"], 20);
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 1), p(2, 2)).as_deref(),
            Some("ne\ntwo\nthr")
        );
    }

    #[test]
    fn wrapped_rows_join_without_newline() {
        let mut first = row("abcdef");
        first.wrapped = true;
        let m = model_with(vec![first, row("ghi")], 6, 0);
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 3), p(1, 1)).as_deref(),
            Some("defgh")
        );
    }

    #[test]
    fn selection_past_the_end_of_text_drops_trailing_blanks() {
        let m = text_model(&["ab", "cd"], 20);
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 0), p(1, 15)).as_deref(),
            Some("ab\ncd")
        );
    }

    #[test]
    fn wide_characters_are_selected_whole() {
        let wide = Row {
            runs: vec![run("a"), wide_run("日本"), run("b")],
            wrapped: false,
        };
        let m = model_with(vec![wide], 10, 0);
        // Columns: a=0, 日=1..2, 本=3..4, b=5. Starting on the right half of 日 includes it.
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 2), p(0, 3)).as_deref(),
            Some("日本")
        );
        // Ending on the left half of 本 still includes the whole character.
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 0), p(0, 3)).as_deref(),
            Some("a日本")
        );
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 0), p(0, 5)).as_deref(),
            Some("a日本b")
        );
        // A click on either half of a wide character is still a click.
        assert_eq!(sel(&m, SelectionKind::Simple, p(0, 1), p(0, 2)), None);
        let span = {
            let mut s = Selection::new(SelectionKind::Simple, p(0, 2), &m);
            s.extend(p(0, 3));
            s.span(&m).unwrap()
        };
        assert_eq!((span.start.col, span.end.col), (1, 4));
    }

    #[test]
    fn combining_characters_stay_with_their_base() {
        let r = Row {
            runs: vec![run("e\u{301}x")],
            wrapped: false,
        };
        let m = model_with(vec![r], 10, 0);
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 0), p(0, 1)).as_deref(),
            Some("e\u{301}x")
        );
    }

    #[test]
    fn double_click_selects_a_word() {
        let m = text_model(&["cd ~/src/staffy-wt now"], 40);
        let word = |col| sel(&m, SelectionKind::Word, p(0, col), p(0, col));
        assert_eq!(word(0).as_deref(), Some("cd"));
        assert_eq!(word(1).as_deref(), Some("cd"));
        assert_eq!(word(5).as_deref(), Some("~/src/staffy-wt"));
        assert_eq!(word(2).as_deref(), Some(" "));
        assert_eq!(word(17).as_deref(), Some("~/src/staffy-wt"));
        assert_eq!(word(18).as_deref(), Some(" "));
        assert_eq!(word(19).as_deref(), Some("now"));
    }

    #[test]
    fn double_click_on_punctuation_selects_one_character() {
        let m = text_model(&["a(b)c"], 10);
        assert_eq!(
            sel(&m, SelectionKind::Word, p(0, 1), p(0, 1)).as_deref(),
            Some("(")
        );
    }

    #[test]
    fn dragging_after_a_double_click_extends_by_words() {
        let m = text_model(&["one two three four"], 40);
        assert_eq!(
            sel(&m, SelectionKind::Word, p(0, 5), p(0, 15)).as_deref(),
            Some("two three four")
        );
        assert_eq!(
            sel(&m, SelectionKind::Word, p(0, 5), p(0, 1)).as_deref(),
            Some("one two")
        );
    }

    #[test]
    fn triple_click_selects_the_logical_line() {
        let mut first = row("abcdef");
        first.wrapped = true;
        let m = model_with(vec![row("above"), first, row("ghi"), row("below")], 6, 0);
        assert_eq!(
            sel(&m, SelectionKind::Line, p(2, 0), p(2, 0)).as_deref(),
            Some("abcdefghi")
        );
        assert_eq!(
            sel(&m, SelectionKind::Line, p(1, 3), p(1, 3)).as_deref(),
            Some("abcdefghi")
        );
        assert_eq!(
            sel(&m, SelectionKind::Line, p(0, 3), p(0, 3)).as_deref(),
            Some("above")
        );
        assert_eq!(
            sel(&m, SelectionKind::Line, p(0, 3), p(3, 0)).as_deref(),
            Some("above\nabcdefghi\nbelow")
        );
    }

    #[test]
    fn selection_extends_across_scrollback() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 20, &["live one", "live two"], 3));
        // Absolute lines: history 0..=2, viewport 3..=4.
        m.apply_history(0, vec![row("hist c"), row("hist b"), row("hist a")]);
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(1, 5), p(3, 3)).as_deref(),
            Some("b\nhist c\nlive")
        );
        // Output scrolls two lines into history: the same absolute lines still hold the text.
        m.apply_frame(frame(2, &[(0, "new one"), (1, "new two")], 5))
            .unwrap();
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(1, 5), p(2, 5)).as_deref(),
            Some("b\nhist c")
        );
        assert_eq!(m.viewport_line(0), 5);
    }

    #[test]
    fn columns_on_reports_the_highlight_per_line() {
        let span = Span {
            start: p(1, 3),
            end: p(3, 4),
        };
        assert_eq!(span.columns_on(0, 10), None);
        assert_eq!(span.columns_on(1, 10), Some((3, 10)));
        assert_eq!(span.columns_on(2, 10), Some((0, 10)));
        assert_eq!(span.columns_on(3, 10), Some((0, 5)));
        assert_eq!(span.columns_on(4, 10), None);
    }

    #[test]
    fn missing_history_rows_read_as_blank() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 20, &["live"], 2));
        assert_eq!(
            sel(&m, SelectionKind::Simple, p(0, 0), p(2, 3)).as_deref(),
            Some("\n\nlive")
        );
    }
}
