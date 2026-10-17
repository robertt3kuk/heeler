//! Review comments of the Changes view (RC-1), without any GPUI: what a comment anchors to, how
//! the anchor is found again in a newer patch (or found gone: *outdated*), and how a review is
//! written down for an agent (chat message plus R-1 attachments, or plain text for a terminal).
//!
//! A comment is held as a *draft* until it is sent; it then keeps its place as *sent* and is
//! *resolved* by the user or when the lines it was about changed after the agent's turn.

use std::ops::Range;
use std::path::Path;

use hutan_chat_view::diff::{DiffLine, FileDiff, LineKind};
use hutan_protocol::assistant::{ATTACHMENTS_MAX, Attachment, LineRange};
use hutan_protocol::chat::SEND_MAX_BYTES;
use serde::{Deserialize, Serialize};

/// Comments one diff (a checkout against one base) keeps.
pub const MAX_COMMENTS: usize = 200;
/// Lines a comment anchors to, and lines of its hunk excerpt.
pub const MAX_SNIPPET_LINES: usize = 200;
/// Lines of the hunk shown around the selection in an excerpt.
const CONTEXT_LINES: usize = 3;
/// Characters kept of a long source line.
const MAX_LINE_CHARS: usize = 400;

/// The header of a review message.
pub const HEADER: &str = "Please address these review comments:";

/// What a diff line is, for the lines a comment can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Add,
    Remove,
    Context,
}

impl Mark {
    pub fn of_kind(kind: LineKind) -> Option<Self> {
        match kind {
            LineKind::Add => Some(Self::Add),
            LineKind::Remove => Some(Self::Remove),
            LineKind::Context => Some(Self::Context),
            LineKind::Hunk | LineKind::Note => None,
        }
    }

    pub fn sign(self) -> char {
        match self {
            Self::Add => '+',
            Self::Remove => '-',
            Self::Context => ' ',
        }
    }
}

/// One line of a diff as a comment remembers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetLine {
    pub mark: Mark,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<u32>,
}

/// `text` cut to [`MAX_LINE_CHARS`] characters.
fn cut(text: &str) -> &str {
    text.char_indices()
        .nth(MAX_LINE_CHARS)
        .map_or(text, |(at, _)| &text[..at])
}

impl SnippetLine {
    fn of(line: &DiffLine) -> Option<Self> {
        Some(Self {
            mark: Mark::of_kind(line.kind)?,
            text: cut(&line.text).to_owned(),
            old: line.old,
            new: line.new,
        })
    }

    /// Whether `line` says the same as this one (its numbers may differ).
    fn same_as(&self, line: &DiffLine) -> bool {
        Mark::of_kind(line.kind) == Some(self.mark) && self.text == cut(&line.text)
    }
}

/// A comment on lines of one file of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    /// Unique within the checkout.
    pub id: u64,
    /// The base the diff was taken against.
    pub base: String,
    pub file: String,
    /// The lines the comment is about, as they were when it was written (and as they were last
    /// found: numbers follow the lines when the file moves them).
    pub lines: Vec<SnippetLine>,
    /// The selection with up to three lines of its hunk around it: what an agent is shown.
    pub excerpt: Vec<SnippetLine>,
    pub text: String,
    pub created_ms: u64,
    /// When it went to an agent; the same for every comment of one review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_ms: Option<u64>,
    /// The session it went to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_to: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    /// The user reopened it: a change of its lines does not resolve it again.
    #[serde(default)]
    pub keep_open: bool,
    /// The lines are not in the diff any more (computed, never saved).
    #[serde(skip)]
    pub outdated: bool,
}

impl ReviewComment {
    /// `a.txt:2-4`, `a.txt:7`, or `a.txt:7-9 (removed)` for lines only the old file has.
    pub fn anchor(&self) -> String {
        match self.new_range() {
            Some((from, to)) => format!("{}:{}", self.file, span(from, to)),
            None => match self.old_range() {
                Some((from, to)) => format!("{}:{} (removed)", self.file, span(from, to)),
                None => self.file.clone(),
            },
        }
    }

    /// The first and last line the comment is about in the new file.
    pub fn new_range(&self) -> Option<(u32, u32)> {
        bounds(self.lines.iter().filter_map(|l| l.new))
    }

    fn old_range(&self) -> Option<(u32, u32)> {
        bounds(self.lines.iter().filter_map(|l| l.old))
    }

    /// The line to measure how far a found anchor moved from.
    pub(super) fn hint(&self) -> Option<u32> {
        self.lines.first().and_then(|l| l.new.or(l.old))
    }

    /// Not sent, not resolved: what "Send to agent" sends.
    pub fn is_draft(&self) -> bool {
        self.sent_ms.is_none() && !self.resolved
    }

    /// The first line of the comment, for lists.
    pub fn headline(&self) -> &str {
        self.text.lines().next().unwrap_or("")
    }

    /// Takes the numbers of the diff lines `found` (a window [`locate`] returned) over; whether
    /// anything changed.
    pub fn renumber(&mut self, diff: &FileDiff, found: (usize, usize)) -> bool {
        let mut changed = false;
        let found_lines = diff.lines[found.0..=found.1]
            .iter()
            .filter(|l| Mark::of_kind(l.kind).is_some());
        for (mine, line) in self.lines.iter_mut().zip(found_lines) {
            if mine.old != line.old || mine.new != line.new {
                mine.old = line.old;
                mine.new = line.new;
                changed = true;
            }
        }
        changed
    }
}

fn bounds(numbers: impl Iterator<Item = u32>) -> Option<(u32, u32)> {
    numbers.fold(None, |acc, n| match acc {
        None => Some((n, n)),
        Some((lo, hi)) => Some((lo.min(n), hi.max(n))),
    })
}

fn span(from: u32, to: u32) -> String {
    if from == to {
        from.to_string()
    } else {
        format!("{from}-{to}")
    }
}

/// The lines of hunk `hunk` of `diff` (a range from [`super::hunk_ranges`]) a comment can be
/// about, as indices into `diff.lines`.
fn content_lines(diff: &FileDiff, hunk: &Range<usize>) -> Vec<usize> {
    (hunk.start + 1..hunk.end)
        .filter(|&at| Mark::of_kind(diff.lines[at].kind).is_some())
        .collect()
}

/// What a comment on the diff lines `from..=to` (indices into `diff.lines`, in `hunk`) holds:
/// the lines (at most [`MAX_SNIPPET_LINES`]) and their excerpt. `None` when the selection has no
/// line a comment can be about.
pub fn snippet(
    diff: &FileDiff,
    hunk: &Range<usize>,
    from: usize,
    to: usize,
) -> Option<(Vec<SnippetLine>, Vec<SnippetLine>)> {
    let content = content_lines(diff, hunk);
    let (first, last) = (
        content.iter().position(|&at| at >= from)?,
        content.iter().rposition(|&at| at <= to)?,
    );
    if first > last {
        return None;
    }
    let last = last.min(first + MAX_SNIPPET_LINES - 1);
    let take = |range: Range<usize>| -> Vec<SnippetLine> {
        content[range]
            .iter()
            .filter_map(|&at| SnippetLine::of(&diff.lines[at]))
            .collect()
    };
    let selected = take(first..last + 1);
    let spare = MAX_SNIPPET_LINES - selected.len();
    let before = CONTEXT_LINES.min(first).min(spare);
    let after = CONTEXT_LINES
        .min(content.len() - 1 - last)
        .min(spare - before);
    let excerpt = take(first - before..last + 1 + after);
    Some((selected, excerpt))
}

/// Where `lines` are in `diff` now: the first and last index into `diff.lines` of the closest
/// run of lines saying the same (within one hunk), or `None` when there is none — the
/// comment is outdated.
pub fn locate(
    diff: &FileDiff,
    hunks: &[Range<usize>],
    lines: &[SnippetLine],
    hint: Option<u32>,
) -> Option<(usize, usize)> {
    if lines.is_empty() {
        return None;
    }
    let mut best: Option<(u32, (usize, usize))> = None;
    for hunk in hunks {
        let content = content_lines(diff, hunk);
        if content.len() < lines.len() {
            continue;
        }
        for window in content.windows(lines.len()) {
            if !window
                .iter()
                .zip(lines)
                .all(|(&at, mine)| mine.same_as(&diff.lines[at]))
            {
                continue;
            }
            let (first, last) = (window[0], window[window.len() - 1]);
            let number = diff.lines[first].new.or(diff.lines[first].old);
            let distance = match (hint, number) {
                (Some(hint), Some(number)) => hint.abs_diff(number),
                _ => 0,
            };
            if best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, (first, last)));
            }
        }
    }
    best.map(|(_, found)| found)
}

/// `+added` / `-removed` / ` context` lines, one per line.
pub fn diff_text(lines: &[SnippetLine]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push(line.mark.sign());
        out.push_str(&line.text);
        out.push('\n');
    }
    out
}

/// A comment's text with every line after the first indented by `indent`.
fn indented(text: &str, indent: &str) -> String {
    text.trim_end().replace('\n', &format!("\n{indent}"))
}

/// A review as one chat message: what the user says and what is attached to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

/// Why a review cannot be sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    #[error("nothing to send")]
    Empty,
    #[error(
        "the review is {0} bytes; at most {SEND_MAX_BYTES} fit in one message: send fewer comments"
    )]
    TooLong(usize),
}

/// The review `comments` (in the order given) as a chat message: a header, each comment
/// numbered with `path:lines`, and for each a `pasted_text` of its hunk excerpt and — where the
/// lines exist in the checkout — a `file` attachment of them. The daemon takes at most
/// [`ATTACHMENTS_MAX`] attachments: excerpts come first, then files for the first comments that
/// have lines, and the excerpts of comments past the limit are written into the text.
pub fn compose_chat(
    checkout: &Path,
    comments: &[&ReviewComment],
) -> Result<Composed, ComposeError> {
    if comments.is_empty() {
        return Err(ComposeError::Empty);
    }
    let pasted = comments.len().min(ATTACHMENTS_MAX);
    let mut files = ATTACHMENTS_MAX - pasted;
    let mut text = format!("{HEADER}\n");
    let mut attachments = Vec::new();
    for (index, comment) in comments.iter().enumerate() {
        text.push_str(&format!(
            "\n{}. {}\n   {}\n",
            index + 1,
            comment.anchor(),
            indented(&comment.text, "   ")
        ));
        if index >= pasted {
            text.push_str(&fenced(comment, "   "));
            continue;
        }
        if files > 0
            && let Some((start, end)) = comment.new_range()
        {
            files -= 1;
            attachments.push(Attachment::File {
                path: checkout.join(&comment.file).display().to_string(),
                range: Some(LineRange { start, end }),
                picked: true,
            });
        }
        attachments.push(Attachment::PastedText {
            text: format!("{}\n{}", comment.anchor(), diff_text(&comment.excerpt)),
        });
    }
    if text.len() > SEND_MAX_BYTES {
        return Err(ComposeError::TooLong(text.len()));
    }
    Ok(Composed { text, attachments })
}

fn fenced(comment: &ReviewComment, indent: &str) -> String {
    let mut out = format!("{indent}```diff\n");
    for line in diff_text(&comment.excerpt).lines() {
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(indent);
    out.push_str("```\n");
    out
}

/// The same review as plain text, for an agent in a terminal or one that starts now: no
/// attachments, so every comment carries its excerpt.
pub fn compose_plain(comments: &[&ReviewComment]) -> Result<String, ComposeError> {
    if comments.is_empty() {
        return Err(ComposeError::Empty);
    }
    let mut text = format!("{HEADER}\n");
    for (index, comment) in comments.iter().enumerate() {
        text.push_str(&format!(
            "\n{}. {}\n   {}\n",
            index + 1,
            comment.anchor(),
            indented(&comment.text, "   ")
        ));
        text.push_str(&fenced(comment, "   "));
    }
    if text.len() > SEND_MAX_BYTES {
        return Err(ComposeError::TooLong(text.len()));
    }
    Ok(text)
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `14:05 UTC`.
pub fn clock(ms: u64) -> String {
    let minutes = ms / 60_000 % (24 * 60);
    format!("{:02}:{:02} UTC", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_chat_view::diff::split_patch;

    const PATCH: &str = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,5 +1,5 @@\n a\n b\n-c\n+C\n d\n e\n@@ -20,2 +20,2 @@\n-f\n+F\n g\n";

    fn diff(patch: &str) -> FileDiff {
        split_patch(patch).remove(0)
    }

    fn hunks(diff: &FileDiff) -> Vec<Range<usize>> {
        super::super::hunk_ranges(diff)
    }

    fn comment(diff: &FileDiff, from: usize, to: usize, text: &str) -> ReviewComment {
        let ranges = hunks(diff);
        let hunk = ranges
            .iter()
            .find(|r| r.contains(&from))
            .expect("a hunk holds the line");
        let (lines, excerpt) = snippet(diff, hunk, from, to).expect("lines");
        ReviewComment {
            id: 1,
            base: "main".into(),
            file: "x".into(),
            lines,
            excerpt,
            text: text.into(),
            created_ms: 0,
            sent_ms: None,
            sent_to: None,
            resolved: false,
            keep_open: false,
            outdated: false,
        }
    }

    #[test]
    fn a_selection_becomes_lines_with_their_numbers_and_an_excerpt_with_context() {
        let d = diff(PATCH);
        // Lines 3..=4 of the diff are `-c` and `+C` (line 0 is the hunk header).
        let c = comment(&d, 3, 4, "why");
        assert_eq!(
            c.lines
                .iter()
                .map(|l| (l.mark, l.text.as_str(), l.old, l.new))
                .collect::<Vec<_>>(),
            [
                (Mark::Remove, "c", Some(3), None),
                (Mark::Add, "C", None, Some(3))
            ]
        );
        assert_eq!(c.anchor(), "x:3");
        assert_eq!(diff_text(&c.excerpt), " a\n b\n-c\n+C\n d\n e\n");
        // Only removed lines: the anchor says so.
        let removed = comment(&d, 3, 3, "gone");
        assert_eq!(removed.anchor(), "x:3 (removed)");
    }

    #[test]
    fn a_selection_holds_at_most_two_hundred_lines() {
        let mut patch = String::from("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -0,0 +1,300 @@\n");
        for n in 0..300 {
            patch.push_str(&format!("+line {n}\n"));
        }
        let d = diff(&patch);
        let c = comment(&d, 1, 300, "big");
        assert_eq!(c.lines.len(), MAX_SNIPPET_LINES);
        assert_eq!(c.excerpt.len(), MAX_SNIPPET_LINES);
        assert_eq!(c.new_range(), Some((1, 200)));
    }

    #[test]
    fn anchors_are_found_again_where_the_lines_moved_and_lost_when_they_changed() {
        let d = diff(PATCH);
        let c = comment(&d, 4, 4, "x");
        assert_eq!(locate(&d, &hunks(&d), &c.lines, c.hint()), Some((4, 4)));
        // Two lines were added above: the same line, other numbers.
        let moved = diff(&PATCH.replace(
            "@@ -1,5 +1,5 @@\n a\n",
            "@@ -1,5 +1,7 @@\n+new1\n+new2\n a\n",
        ));
        let found = locate(&moved, &hunks(&moved), &c.lines, c.hint()).expect("found");
        assert_eq!(found, (6, 6));
        let mut renumbered = c.clone();
        assert!(renumbered.renumber(&moved, found));
        assert_eq!(renumbered.anchor(), "x:5");
        // The commented line was rewritten: gone.
        let changed = diff(&PATCH.replace("+C\n", "+CC\n"));
        assert_eq!(locate(&changed, &hunks(&changed), &c.lines, c.hint()), None);
        // A multi-line selection needs the whole run, in one hunk.
        let two = comment(&d, 3, 4, "x");
        assert!(locate(&d, &hunks(&d), &two.lines, two.hint()).is_some());
        let split = diff(&PATCH.replace("-c\n+C\n", "-c\n d\n+C\n"));
        assert_eq!(locate(&split, &hunks(&split), &two.lines, two.hint()), None);
    }

    #[test]
    fn the_closest_of_several_matches_wins() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,0 +1,2 @@\n+dup\n+dup\n@@ -50,0 +52,1 @@\n+dup\n";
        let d = diff(patch);
        let mut c = comment(&d, 1, 1, "x");
        c.lines[0].new = Some(52);
        assert_eq!(locate(&d, &hunks(&d), &c.lines, c.hint()), Some((4, 4)));
    }

    #[test]
    fn a_chat_review_carries_the_text_a_file_range_and_the_excerpt_of_each_comment() {
        let d = diff(PATCH);
        let one = comment(&d, 4, 4, "Rename this.\nAnd test it.");
        let mut two = comment(&d, 9, 9, "Why F?");
        two.id = 2;
        let composed = compose_chat(Path::new("/wt"), &[&one, &two]).unwrap();
        assert_eq!(
            composed.text,
            "Please address these review comments:\n\n1. x:3\n   Rename this.\n   And test it.\n\n2. x:20\n   Why F?\n"
        );
        assert_eq!(
            composed.attachments,
            [
                Attachment::File {
                    path: "/wt/x".into(),
                    range: Some(LineRange { start: 3, end: 3 }),
                    picked: true
                },
                Attachment::PastedText {
                    text: "x:3\n a\n b\n-c\n+C\n d\n e\n".into()
                },
                Attachment::File {
                    path: "/wt/x".into(),
                    range: Some(LineRange { start: 20, end: 20 }),
                    picked: true
                },
                Attachment::PastedText {
                    text: "x:20\n-f\n+F\n g\n".into()
                },
            ]
        );
        // Removed lines have no place in the checkout: the excerpt alone goes.
        let removed = comment(&d, 3, 3, "gone");
        let composed = compose_chat(Path::new("/wt"), &[&removed]).unwrap();
        assert!(matches!(
            composed.attachments.as_slice(),
            [Attachment::PastedText { .. }]
        ));
        assert_eq!(
            compose_chat(Path::new("/wt"), &[]),
            Err(ComposeError::Empty)
        );
    }

    #[test]
    fn many_comments_keep_to_the_attachment_limit() {
        let d = diff(PATCH);
        let comments: Vec<ReviewComment> = (0..20)
            .map(|n| {
                let mut c = comment(&d, 4, 4, &format!("c{n}"));
                c.id = n;
                c
            })
            .collect();
        let refs: Vec<&ReviewComment> = comments.iter().collect();
        let composed = compose_chat(Path::new("/wt"), &refs).unwrap();
        assert_eq!(composed.attachments.len(), ATTACHMENTS_MAX);
        // Sixteen excerpts and no file; the last four are in the text.
        assert!(
            composed
                .attachments
                .iter()
                .all(|a| matches!(a, Attachment::PastedText { .. }))
        );
        assert_eq!(composed.text.matches("```diff").count(), 4);
    }

    #[test]
    fn plain_text_has_every_excerpt_inline() {
        let d = diff(PATCH);
        let c = comment(&d, 4, 4, "Rename.");
        assert_eq!(
            compose_plain(&[&c]).unwrap(),
            "Please address these review comments:\n\n1. x:3\n   Rename.\n   ```diff\n    a\n    b\n   -c\n   +C\n    d\n    e\n   ```\n"
        );
    }

    #[test]
    fn a_review_too_long_for_one_message_is_refused() {
        let d = diff(PATCH);
        let c = comment(&d, 4, 4, &"x".repeat(SEND_MAX_BYTES));
        assert!(matches!(
            compose_chat(Path::new("/wt"), &[&c]),
            Err(ComposeError::TooLong(_))
        ));
    }

    #[test]
    fn comments_round_trip_through_json_without_the_computed_flag() {
        let d = diff(PATCH);
        let mut c = comment(&d, 4, 4, "x");
        c.outdated = true;
        c.sent_ms = Some(5);
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("outdated"));
        let back: ReviewComment = serde_json::from_str(&json).unwrap();
        c.outdated = false;
        assert_eq!(back, c);
    }

    #[test]
    fn the_clock_reads_utc() {
        assert_eq!(clock(14 * 3_600_000 + 5 * 60_000 + 1234), "14:05 UTC");
    }
}
