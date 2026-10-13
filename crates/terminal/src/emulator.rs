//! Headless VT engine: OSC sniffer → `alacritty_terminal` → render DTOs.
//!
//! This is the whole terminal minus the PTY. [`crate::Terminal`] drives one from its reader
//! thread; the conformance corpus, the recorder and the flood bench drive it directly.

use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape as EngineCursorShape, Processor};
use heeler_protocol::messages::TerminalSize;
use heeler_protocol::render::{
    Cursor, CursorShape, Link, Modes, MouseMode, Palette, Row, TerminalFrame, TerminalSnapshot,
};
use parking_lot::Mutex;

use crate::error::TerminalError;
use crate::render::{LinkTable, RenderTracker, convert_row, row_text};
use crate::sniffer::{OscSniffer, TerminalMeta};

/// Replies produced by one `feed` call beyond this are dropped (a hostile stream of queries
/// must not grow memory without bound).
const MAX_PENDING_REPLY_BYTES: usize = 64 * 1024;
/// Length of the DEC 2026 begin marker `ESC [ ? 2 0 2 6 h`.
const BSU_LEN: usize = 8;
/// Same bound for queued metadata events.
const MAX_PENDING_META: usize = 4096;

#[derive(Debug, Default)]
struct SinkInner {
    replies: Vec<u8>,
    bells: u32,
}

/// Engine event listener: collects the replies the engine wants written back to the PTY and
/// bell notifications. Everything else the engine reports is intentionally ignored (clipboard,
/// colour and text-area queries, mouse-cursor hints).
#[derive(Debug, Clone, Default)]
struct Sink(Arc<Mutex<SinkInner>>);

impl EventListener for Sink {
    fn send_event(&self, event: Event) {
        let mut inner = self.0.lock();
        match event {
            Event::PtyWrite(text) => {
                if inner.replies.len() + text.len() <= MAX_PENDING_REPLY_BYTES {
                    inner.replies.extend_from_slice(text.as_bytes());
                }
            }
            Event::Bell => inner.bells = inner.bells.saturating_add(1),
            _ => {}
        }
    }
}

struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

fn validate(size: TerminalSize) -> Result<Dims, TerminalError> {
    if size.cols == 0 || size.rows == 0 {
        return Err(TerminalError::InvalidSize {
            cols: size.cols,
            rows: size.rows,
        });
    }
    Ok(Dims {
        cols: usize::from(size.cols),
        rows: usize::from(size.rows),
    })
}

/// A VT engine instance with render-state extraction. Not thread-safe by itself.
pub struct Emulator {
    term: Term<Sink>,
    parser: Processor,
    sniffer: OscSniffer,
    sink: Sink,
    links: LinkTable,
    meta: Vec<TerminalMeta>,
    title: Option<String>,
    size: TerminalSize,
    /// Bumped whenever the visible state may have changed.
    generation: u64,
}

impl std::fmt::Debug for Emulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emulator")
            .field("size", &self.size)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Emulator {
    /// `scrollback_lines` bounds the retained history exactly.
    pub fn new(size: TerminalSize, scrollback_lines: usize) -> Result<Self, TerminalError> {
        let dims = validate(size)?;
        let sink = Sink::default();
        let config = Config {
            kitty_keyboard: true,
            scrolling_history: scrollback_lines,
            ..Config::default()
        };
        Ok(Self {
            term: Term::new(config, &dims, sink.clone()),
            parser: Processor::new(),
            sniffer: OscSniffer::new(),
            sink,
            links: LinkTable::default(),
            meta: Vec::new(),
            title: None,
            size,
            generation: 1,
        })
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    /// Counter that changes whenever the visible state may have changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Runs `bytes` through the sniffer and the engine. Returns `true` when visible state may
    /// have changed; `false` while every byte went into an open DEC 2026 synchronized batch
    /// (the batch is committed by its end marker, [`Self::stop_sync`] or a buffer overflow).
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let meta_start = self.meta.len();
        let was_pending = self.sync_deadline().is_some();
        self.sniffer.feed(bytes, &mut self.meta);
        self.parser.advance(&mut self.term, bytes);
        self.collect_bell();
        self.apply_titles(meta_start);
        let withheld = self.parser.sync_bytes_count();
        let visible = if was_pending {
            // Everything went into the batch unless the batch ended inside this chunk.
            withheld < bytes.len()
        } else if self.sync_deadline().is_some() {
            // A batch opened inside this chunk: only bytes before its begin marker count.
            bytes.len() > withheld + BSU_LEN
        } else {
            true
        };
        if visible {
            self.generation += 1;
        }
        visible
    }

    /// When the open synchronized-output batch must be force-closed (150 ms after it began).
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// True while a synchronized-output batch is open and its bytes are withheld.
    pub fn sync_pending(&self) -> bool {
        self.parser.sync_bytes_count() > 0 || self.sync_deadline().is_some()
    }

    /// Commits an open synchronized-output batch.
    pub fn stop_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
        self.collect_bell();
        self.generation += 1;
    }

    /// Takes the bytes the engine wants written back to the PTY (DA, DSR, DECRQM, kitty
    /// keyboard query replies).
    pub fn drain_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.sink.0.lock().replies)
    }

    /// Takes pending metadata events in stream order (bell, at most once per `feed`, last).
    pub fn drain_meta(&mut self) -> Vec<TerminalMeta> {
        std::mem::take(&mut self.meta)
    }

    pub fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalError> {
        let dims = validate(size)?;
        self.term.resize(dims);
        self.size = size;
        self.generation += 1;
        Ok(())
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Lines of scrollback currently retained (0 on the alternate screen).
    pub fn history_len(&self) -> u32 {
        u32::try_from(self.term.history_size()).unwrap_or(u32::MAX)
    }

    /// True when `tracker` cannot be advanced with a [`TerminalFrame`] and needs a fresh
    /// [`TerminalSnapshot`]: it never had one, the size changed, or the palette changed
    /// (frames carry neither dimensions nor palette).
    pub fn needs_snapshot(&self, tracker: &RenderTracker) -> bool {
        !tracker.synced
            || tracker.cols != self.size.cols
            || tracker.rows != self.size.rows
            || tracker.palette != self.palette()
    }

    /// Full visible state; resets `tracker` to it.
    pub fn snapshot(&mut self, tracker: &mut RenderTracker) -> TerminalSnapshot {
        let lines = self.viewport_rows();
        let cursor = self.cursor();
        let modes = self.modes();
        let palette = self.palette();
        let title = self.title.clone();
        let history_len = self.history_len();
        let links = self.links.all();
        tracker.frame += 1;
        tracker.synced = true;
        tracker.cols = self.size.cols;
        tracker.rows = self.size.rows;
        tracker.lines = lines.clone();
        tracker.cursor = Some(cursor);
        tracker.modes = modes;
        tracker.palette = palette.clone();
        tracker.title = title.clone();
        tracker.links_sent = self.links.len();
        tracker.history_len = history_len;
        tracker.generation = self.generation;
        TerminalSnapshot {
            frame: tracker.frame,
            cols: self.size.cols,
            rows: self.size.rows,
            cursor,
            modes,
            palette,
            title,
            lines,
            links,
            history_len,
        }
    }

    /// Changes since `tracker`'s state, or `None` when nothing changed — and also when the
    /// tracker [needs a snapshot](Self::needs_snapshot) instead.
    pub fn frame(&mut self, tracker: &mut RenderTracker) -> Option<TerminalFrame> {
        if self.needs_snapshot(tracker) || tracker.generation == self.generation {
            return None;
        }
        let lines = self.viewport_rows();
        let cursor = self.cursor();
        let modes = self.modes();
        let history_len = self.history_len();
        let dirty: Vec<(u16, Row)> = lines
            .iter()
            .zip(&tracker.lines)
            .enumerate()
            .filter(|(_, (now, before))| now != before)
            .map(|(index, (now, _))| (index as u16, now.clone()))
            .collect();
        let title_changed = self.title != tracker.title;
        let modes_changed = modes != tracker.modes;
        tracker.generation = self.generation;
        if dirty.is_empty()
            && !title_changed
            && !modes_changed
            && tracker.cursor == Some(cursor)
            && tracker.history_len == history_len
        {
            return None;
        }
        let links = self.links.since(tracker.links_sent);
        tracker.links_sent = self.links.len();
        tracker.frame += 1;
        tracker.lines = lines;
        tracker.cursor = Some(cursor);
        tracker.modes = modes;
        tracker.history_len = history_len;
        if title_changed {
            tracker.title.clone_from(&self.title);
        }
        Some(TerminalFrame {
            frame: tracker.frame,
            scrolled: 0,
            dirty,
            cursor,
            modes: modes_changed.then_some(modes),
            title: title_changed.then(|| self.title.clone()),
            links,
            history_len,
        })
    }

    /// Scrollback rows, newest first: `from_line` 0 is the line directly above the viewport,
    /// larger is older. Shorter than `count` at the top of history.
    pub fn history(&mut self, from_line: u32, count: u32) -> Vec<Row> {
        let available = self.history_len();
        let cols = self.term.columns();
        let end = from_line.saturating_add(count).min(available);
        let mut rows = Vec::with_capacity(end.saturating_sub(from_line) as usize);
        for index in from_line..end {
            let line = Line(-(index as i32) - 1);
            rows.push(convert_row(&self.term.grid()[line], cols, &mut self.links));
        }
        rows
    }

    /// Visible rows as plain text, one line per row, trailing spaces trimmed (for tests and
    /// corpus `.grid` files).
    pub fn grid_text(&mut self) -> String {
        let rows = self.viewport_rows();
        rows.iter().map(row_text).collect::<Vec<_>>().join("\n")
    }

    /// Hyperlink table so far (`id == index`).
    pub fn links(&self) -> Vec<Link> {
        self.links.all()
    }

    pub fn cursor(&self) -> Cursor {
        let content = self.term.renderable_content();
        let style = self.term.cursor_style();
        let shape = match style.shape {
            EngineCursorShape::Block | EngineCursorShape::Hidden => CursorShape::Block,
            EngineCursorShape::Underline => CursorShape::Underline,
            EngineCursorShape::Beam => CursorShape::Bar,
            EngineCursorShape::HollowBlock => CursorShape::HollowBlock,
        };
        let point = content.cursor.point;
        Cursor {
            col: u16::try_from(point.column.0).unwrap_or(u16::MAX),
            row: u16::try_from(point.line.0.max(0)).unwrap_or(u16::MAX),
            shape,
            visible: content.cursor.shape != EngineCursorShape::Hidden,
            blinking: style.blinking,
        }
    }

    pub fn modes(&self) -> Modes {
        let mode = *self.term.mode();
        let mouse = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseMode::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseMode::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseMode::Click
        } else {
            MouseMode::None
        };
        const KITTY: [(TermMode, u8); 5] = [
            (TermMode::DISAMBIGUATE_ESC_CODES, 1),
            (TermMode::REPORT_EVENT_TYPES, 2),
            (TermMode::REPORT_ALTERNATE_KEYS, 4),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
            (TermMode::REPORT_ASSOCIATED_TEXT, 16),
        ];
        let kitty_keyboard = KITTY
            .iter()
            .filter(|(flag, _)| mode.contains(*flag))
            .fold(0, |bits, (_, bit)| bits | bit);
        Modes {
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse,
            mouse_sgr: mode.contains(TermMode::SGR_MOUSE),
            focus_events: mode.contains(TermMode::FOCUS_IN_OUT),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            kitty_keyboard,
        }
    }

    /// Colours the application changed with OSC 4/10/11/12.
    pub fn palette(&self) -> Palette {
        let colors = self.term.colors();
        let rgb = |index: usize| colors[index].map(|c| (c.r, c.g, c.b));
        Palette {
            indexed: (0..256usize)
                .filter_map(|index| rgb(index).map(|color| (index as u8, color)))
                .collect(),
            foreground: rgb(256),
            background: rgb(257),
            cursor: rgb(258),
        }
    }

    fn viewport_rows(&mut self) -> Vec<Row> {
        let cols = self.term.columns();
        let lines = self.term.screen_lines();
        (0..lines)
            .map(|line| convert_row(&self.term.grid()[Line(line as i32)], cols, &mut self.links))
            .collect()
    }

    fn collect_bell(&mut self) {
        let bells = std::mem::take(&mut self.sink.0.lock().bells);
        if bells > 0 {
            self.meta.push(TerminalMeta::Bell);
        }
        if self.meta.len() > MAX_PENDING_META {
            self.meta.truncate(MAX_PENDING_META);
        }
    }

    fn apply_titles(&mut self, from: usize) {
        for meta in self.meta.iter().skip(from) {
            if let TerminalMeta::Title(title) = meta {
                self.title = (!title.is_empty()).then(|| title.clone());
            }
        }
    }
}
