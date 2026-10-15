//! Cold scrollback store (ADR 0013): rows that scrolled out of the engine's hot window, packed.
//!
//! Rows are kept in chunks of [`CHUNK_ROWS`]. A chunk owns one UTF-8 text buffer, a row index
//! (end offsets, trailing blank count, flags) and style spans keyed by byte offset that exist
//! only where the style changes. Nothing is allocated per cell or per row. Eviction (oldest
//! first) advances a head counter and drops a chunk once all its rows are gone, so evicted rows
//! release their memory at chunk granularity.

use std::collections::{HashMap, VecDeque};

use hutan_protocol::render::{Color, Row};

use crate::render::{RowScratch, Style, run_of};

/// Rows per chunk. Bounds the dead rows retained at the oldest end and the cost of locating a row.
pub(crate) const CHUNK_ROWS: usize = 512;
/// A chunk is also sealed once its text reaches this size (very wide rows).
const CHUNK_TEXT_BYTES: usize = 1 << 20;

const WRAPPED: u8 = 1;
const CONTINUATION: u8 = 2;

/// [`Style`] packed to plain integers so it can be hashed and stored in 20 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PackedStyle {
    fg: u32,
    bg: u32,
    underline: u32,
    link: u32,
    flags: u16,
}

const NO_COLOR: u32 = 3 << 24;
const NO_LINK: u32 = u32::MAX;

fn pack_color(color: Color) -> u32 {
    match color {
        Color::Default => 0,
        Color::Indexed(index) => (1 << 24) | u32::from(index),
        Color::Rgb(r, g, b) => {
            (2 << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        }
    }
}

fn unpack_color(packed: u32) -> Color {
    let [tag, r, g, b] = packed.to_be_bytes();
    match tag {
        1 => Color::Indexed(b),
        2 => Color::Rgb(r, g, b),
        _ => Color::Default,
    }
}

impl PackedStyle {
    fn pack(style: &Style) -> Self {
        Self {
            fg: pack_color(style.fg),
            bg: pack_color(style.bg),
            underline: style.underline_color.map_or(NO_COLOR, pack_color),
            link: style.link.unwrap_or(NO_LINK),
            flags: style.flags.bits(),
        }
    }

    fn unpack(self) -> Style {
        Style {
            fg: unpack_color(self.fg),
            bg: unpack_color(self.bg),
            flags: hutan_protocol::render::CellFlags::from_bits_retain(self.flags),
            underline_color: (self.underline != NO_COLOR).then(|| unpack_color(self.underline)),
            link: (self.link != NO_LINK).then_some(self.link),
        }
    }
}

/// From this byte offset (relative to the row's text) on, the row uses style `style` (an index
/// into the chunk's style table). Before the first span of a row the style is the default one.
#[derive(Clone, Copy)]
struct Span {
    at: u32,
    style: u32,
}

/// Row index entry. A row's text and spans start where the previous row's end (0 for the first
/// row of the chunk).
#[derive(Clone, Copy)]
struct RowMeta {
    text_end: u32,
    span_end: u32,
    blank_tail: u16,
    flags: u8,
}

struct Chunk {
    text: String,
    spans: Vec<Span>,
    rows: Vec<RowMeta>,
    styles: Vec<PackedStyle>,
    /// Style → index in `styles`; only the open chunk keeps it.
    style_ids: HashMap<PackedStyle, u32>,
    /// Rows `0..head` are evicted (their bytes stay until the whole chunk goes).
    head: usize,
}

impl Chunk {
    fn open() -> Self {
        Self {
            text: String::new(),
            spans: Vec::new(),
            rows: Vec::with_capacity(CHUNK_ROWS),
            styles: Vec::new(),
            style_ids: HashMap::new(),
            head: 0,
        }
    }

    fn is_full(&self) -> bool {
        self.rows.len() >= CHUNK_ROWS || self.text.len() >= CHUNK_TEXT_BYTES
    }

    fn seal(&mut self) {
        self.text.shrink_to_fit();
        self.spans.shrink_to_fit();
        self.rows.shrink_to_fit();
        self.styles.shrink_to_fit();
        self.style_ids = HashMap::new();
    }

    fn live(&self) -> usize {
        self.rows.len() - self.head
    }

    fn style_id(&mut self, style: PackedStyle) -> u32 {
        if let Some(&id) = self.style_ids.get(&style) {
            return id;
        }
        // A chunk holds at most `CHUNK_TEXT_BYTES` of text, so its style count fits in u32.
        let id = u32::try_from(self.styles.len()).unwrap_or(u32::MAX);
        self.styles.push(style);
        self.style_ids.insert(style, id);
        id
    }

    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        self.text.capacity()
            + self.spans.capacity() * size_of::<Span>()
            + self.rows.capacity() * size_of::<RowMeta>()
            + self.styles.capacity() * size_of::<PackedStyle>()
            + self.style_ids.capacity() * (size_of::<PackedStyle>() + size_of::<u32>() + 1)
    }
}

/// Packed history rows, oldest to newest.
#[derive(Default)]
pub(crate) struct ScrollbackStore {
    chunks: VecDeque<Chunk>,
    len: usize,
    /// The newest row soft-wraps onto the next one, which therefore continues its line.
    last_wrapped: bool,
}

impl ScrollbackStore {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends `row` as the newest row.
    pub(crate) fn push(&mut self, row: &RowScratch) {
        if self.chunks.back().is_none_or(Chunk::is_full) {
            if let Some(full) = self.chunks.back_mut() {
                full.seal();
            }
            self.chunks.push_back(Chunk::open());
        }
        let Some(chunk) = self.chunks.back_mut() else {
            return;
        };
        let (_, span_start) = chunk
            .rows
            .last()
            .map_or((0, 0), |meta| (meta.text_end, meta.span_end));
        debug_assert_eq!(span_start as usize, chunk.spans.len());
        let base = chunk.text.len();
        chunk.text.push_str(&row.text);
        let mut current = PackedStyle::pack(&Style::DEFAULT);
        for (style, start) in &row.runs {
            let packed = PackedStyle::pack(style);
            if packed != current {
                let id = chunk.style_id(packed);
                chunk.spans.push(Span {
                    at: u32::try_from(*start).unwrap_or(u32::MAX),
                    style: id,
                });
                current = packed;
            }
        }
        let mut flags = 0;
        if row.wrapped {
            flags |= WRAPPED;
        }
        if self.last_wrapped {
            flags |= CONTINUATION;
        }
        // Chunk text and spans stay far below 4 GiB (`CHUNK_TEXT_BYTES` plus one row).
        chunk.rows.push(RowMeta {
            text_end: u32::try_from(base + row.text.len()).unwrap_or(u32::MAX),
            span_end: u32::try_from(chunk.spans.len()).unwrap_or(u32::MAX),
            blank_tail: row.blank_tail,
            flags,
        });
        self.last_wrapped = row.wrapped;
        self.len += 1;
    }

    /// Drops the `count` oldest rows.
    pub(crate) fn evict_oldest(&mut self, mut count: usize) {
        count = count.min(self.len);
        self.len -= count;
        while count > 0 {
            let Some(front) = self.chunks.front_mut() else {
                break;
            };
            let live = front.live();
            if count >= live {
                count -= live;
                self.chunks.pop_front();
            } else {
                front.head += count;
                count = 0;
            }
        }
        if self.chunks.is_empty() {
            self.chunks = VecDeque::new();
        }
    }

    /// Keeps at most the `max_len` newest rows.
    pub(crate) fn truncate_to(&mut self, max_len: usize) {
        if self.len > max_len {
            self.evict_oldest(self.len - max_len);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.chunks = VecDeque::new();
        self.len = 0;
        self.last_wrapped = false;
    }

    /// Row `index` counted from the oldest (0) to the newest (`len() - 1`).
    pub(crate) fn get(&self, index: usize) -> Option<ColdRow<'_>> {
        if index >= self.len {
            return None;
        }
        let mut remaining = index;
        for chunk in &self.chunks {
            let live = chunk.live();
            if remaining < live {
                return Some(ColdRow {
                    chunk,
                    row: chunk.head + remaining,
                });
            }
            remaining -= live;
        }
        None
    }

    /// Allocated bytes (capacity, not length), to check that eviction releases memory.
    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        self.chunks.iter().map(Chunk::heap_bytes).sum()
    }
}

/// A borrowed cold row.
pub(crate) struct ColdRow<'a> {
    chunk: &'a Chunk,
    row: usize,
}

impl ColdRow<'_> {
    fn bounds(&self) -> (usize, usize, usize, usize) {
        let meta = self.chunk.rows[self.row];
        let (text_start, span_start) = match self.row.checked_sub(1) {
            Some(previous) => {
                let before = self.chunk.rows[previous];
                (before.text_end, before.span_end)
            }
            None => (0, 0),
        };
        (
            text_start as usize,
            meta.text_end as usize,
            span_start as usize,
            meta.span_end as usize,
        )
    }

    fn meta(&self) -> RowMeta {
        self.chunk.rows[self.row]
    }

    pub(crate) fn text(&self) -> &str {
        let (start, end, _, _) = self.bounds();
        &self.chunk.text[start..end]
    }

    pub(crate) fn wrapped(&self) -> bool {
        self.meta().flags & WRAPPED != 0
    }

    /// The row continues a line that started on an earlier row (which may be evicted).
    #[cfg(test)]
    pub(crate) fn continuation(&self) -> bool {
        self.meta().flags & CONTINUATION != 0
    }

    /// Blank default-style columns that were trimmed from the end of the row.
    pub(crate) fn blank_tail(&self) -> u16 {
        self.meta().blank_tail
    }

    /// The protocol row.
    pub(crate) fn to_row(&self) -> Row {
        let (start, end, span_start, span_end) = self.bounds();
        let text = &self.chunk.text[start..end];
        let spans = &self.chunk.spans[span_start..span_end];
        let mut runs = Vec::with_capacity(spans.len() + 1);
        let mut style = Style::DEFAULT;
        let mut run_start = 0;
        for span in spans {
            let at = span.at as usize;
            if at > run_start {
                runs.push(run_of(&style, text[run_start..at].to_owned()));
                run_start = at;
            }
            style = self.chunk.styles[span.style as usize].unpack();
        }
        if run_start < text.len() {
            runs.push(run_of(&style, text[run_start..].to_owned()));
        }
        Row {
            runs,
            wrapped: self.wrapped(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::render::{CellFlags, Run};

    fn scratch(text: &str, runs: &[(Style, usize)], wrapped: bool, blank_tail: u16) -> RowScratch {
        RowScratch {
            text: text.to_owned(),
            runs: runs.to_vec(),
            wrapped,
            blank_tail,
        }
    }

    fn red() -> Style {
        Style {
            fg: Color::Indexed(1),
            ..Style::DEFAULT
        }
    }

    fn rich() -> Style {
        Style {
            fg: Color::Rgb(1, 2, 3),
            bg: Color::Indexed(200),
            flags: CellFlags::BOLD | CellFlags::WIDE,
            underline_color: Some(Color::Rgb(9, 8, 7)),
            link: Some(41),
        }
    }

    fn plain(i: usize) -> RowScratch {
        scratch(&format!("row {i}"), &[(Style::DEFAULT, 0)], false, 0)
    }

    #[test]
    fn packed_styles_round_trip() {
        for style in [
            Style::DEFAULT,
            red(),
            rich(),
            Style {
                underline_color: Some(Color::Default),
                ..Style::DEFAULT
            },
        ] {
            assert!(PackedStyle::pack(&style).unpack() == style);
        }
    }

    #[test]
    fn rows_round_trip_through_spans() {
        let mut store = ScrollbackStore::default();
        // default "ab", red "cd", default "ef", rich "g"
        store.push(&scratch(
            "abcdefg",
            &[
                (Style::DEFAULT, 0),
                (red(), 2),
                (Style::DEFAULT, 4),
                (rich(), 6),
            ],
            true,
            3,
        ));
        store.push(&scratch("xy", &[(red(), 0)], false, 0));
        store.push(&scratch("", &[], false, 0));
        let run = |text: &str, style: &Style| -> Run { run_of(style, text.to_owned()) };
        let first = store.get(0).map(|row| row.to_row());
        assert_eq!(
            first,
            Some(Row {
                runs: vec![
                    run("ab", &Style::DEFAULT),
                    run("cd", &red()),
                    run("ef", &Style::DEFAULT),
                    run("g", &rich())
                ],
                wrapped: true
            })
        );
        let second = store.get(1);
        assert_eq!(
            second.as_ref().map(ColdRow::to_row),
            Some(Row {
                runs: vec![run("xy", &red())],
                wrapped: false
            })
        );
        assert_eq!(second.map(|row| row.continuation()), Some(true));
        let third = store.get(2);
        assert_eq!(third.as_ref().map(ColdRow::to_row), Some(Row::default()));
        assert_eq!(third.map(|row| row.continuation()), Some(false));
        assert_eq!(store.get(0).map(|row| row.blank_tail()), Some(3));
        assert!(store.get(3).is_none());
    }

    #[test]
    fn default_rows_store_no_spans() {
        let mut store = ScrollbackStore::default();
        for i in 0..10 {
            store.push(&plain(i));
        }
        assert!(store.chunks.iter().all(|chunk| chunk.spans.is_empty()));
    }

    #[test]
    fn eviction_is_oldest_first_and_exact() {
        let mut store = ScrollbackStore::default();
        let total = CHUNK_ROWS * 3 + 17;
        for i in 0..total {
            store.push(&plain(i));
        }
        assert_eq!(store.len(), total);
        store.evict_oldest(CHUNK_ROWS + 5);
        assert_eq!(store.len(), total - CHUNK_ROWS - 5);
        assert_eq!(
            store.get(0).map(|row| row.text().to_owned()),
            Some(format!("row {}", CHUNK_ROWS + 5))
        );
        store.truncate_to(10);
        assert_eq!(store.len(), 10);
        assert_eq!(
            store.get(9).map(|row| row.text().to_owned()),
            Some(format!("row {}", total - 1))
        );
        store.truncate_to(100);
        assert_eq!(store.len(), 10);
        store.evict_oldest(1_000_000);
        assert!(store.is_empty() && store.chunks.is_empty());
        // Pushing after a full eviction keeps working.
        store.push(&plain(7));
        assert_eq!(
            store.get(0).map(|row| row.text().to_owned()),
            Some("row 7".into())
        );
    }

    #[test]
    fn eviction_releases_whole_chunks() {
        let mut store = ScrollbackStore::default();
        for i in 0..CHUNK_ROWS * 8 {
            store.push(&plain(i));
        }
        let full = store.heap_bytes();
        // Less than a chunk: nothing can be freed yet.
        store.evict_oldest(CHUNK_ROWS - 1);
        assert_eq!(store.heap_bytes(), full);
        store.evict_oldest(1);
        assert!(store.heap_bytes() < full);
        store.evict_oldest(CHUNK_ROWS * 6);
        assert!(store.heap_bytes() < full / 2);
        store.clear();
        assert_eq!(store.heap_bytes(), 0);
    }

    #[test]
    fn continuation_flag_survives_eviction_of_the_line_start() {
        let mut store = ScrollbackStore::default();
        store.push(&scratch("aaaa", &[(Style::DEFAULT, 0)], true, 0));
        store.push(&scratch("bbbb", &[(Style::DEFAULT, 0)], true, 0));
        store.push(&scratch("cc", &[(Style::DEFAULT, 0)], false, 0));
        store.push(&scratch("dd", &[(Style::DEFAULT, 0)], false, 0));
        store.evict_oldest(1);
        let flags: Vec<bool> = (0..store.len())
            .filter_map(|i| store.get(i).map(|row| row.continuation()))
            .collect();
        assert_eq!(flags, [true, true, false]);
    }

    #[test]
    fn sealed_chunks_do_not_keep_spare_capacity() {
        let mut store = ScrollbackStore::default();
        for i in 0..CHUNK_ROWS * 2 {
            store.push(&scratch(
                &format!("line {i}"),
                &[(red(), 0), (Style::DEFAULT, 3)],
                false,
                0,
            ));
        }
        let sealed = &store.chunks[0];
        assert_eq!(sealed.text.capacity(), sealed.text.len());
        assert_eq!(sealed.spans.capacity(), sealed.spans.len());
        assert!(sealed.style_ids.is_empty());
        assert_eq!(sealed.styles.len(), 2);
    }
}
