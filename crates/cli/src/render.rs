//! Draws the daemon's render stream (ADR 0004) into the host terminal, tmux-style.
//!
//! The daemon is the only VT parser; [`HostRenderer`] turns [`TerminalSnapshot`]s and
//! [`TerminalFrame`]s into escape sequences. It keeps a model of the remote screen plus a
//! mirror of what the host currently shows, so each frame rewrites only the rows that really
//! differ (and uses the host's own scroll for `scrolled` frames). Output goes to a caller
//! buffer; nothing here touches stdout.
//!
//! The renderer assumes it owns the host screen: call [`HostRenderer::enter`] first, apply
//! snapshots and frames, call [`HostRenderer::leave`] last. To redraw after the host screen was
//! disturbed (for example after resuming from a suspend), call [`HostRenderer::enter`] and then
//! [`HostRenderer::resize_host`] with the current size.
//!
//! Remote content is untrusted: control characters in cell text and hyperlink targets are
//! never forwarded to the host.

use std::collections::HashMap;
use std::io::Write as _;

use heeler_protocol::render::{
    CellFlags, Color, Cursor, CursorShape, Link, Modes, MouseMode, Row, Run, TerminalFrame,
    TerminalSnapshot,
};
use unicode_width::UnicodeWidthChar;

/// Host-terminal state mirrored from the remote terminal, with the host screen it is drawn on.
#[derive(Debug)]
pub struct HostRenderer {
    host_cols: u16,
    host_rows: u16,
    /// Remote viewport, one entry per remote row; empty until the first snapshot.
    rows: Vec<Row>,
    links: HashMap<u32, String>,
    cursor: Option<Cursor>,
    /// Modes the remote terminal wants.
    modes: Modes,
    /// Modes currently enabled on the host (empty after [`leave`](Self::leave) in the host's
    /// eyes, but remembered so [`enter`](Self::enter) can restore them).
    applied_modes: Modes,
    /// Host screen content by row; `None` means unknown and forces a repaint.
    shown: Vec<Option<Row>>,
    host_cursor: HostCursor,
}

/// What the host's cursor is known to be doing; `None` fields are unknown.
#[derive(Debug, Default, Clone, Copy)]
struct HostCursor {
    pos: Option<(u16, u16)>,
    shape: Option<u8>,
    visible: Option<bool>,
}

impl HostCursor {
    fn hide(&mut self, out: &mut Vec<u8>) {
        if self.visible != Some(false) {
            out.extend_from_slice(b"\x1b[?25l");
            self.visible = Some(false);
        }
    }
}

impl HostRenderer {
    pub fn new(host_cols: u16, host_rows: u16) -> Self {
        Self {
            host_cols,
            host_rows,
            rows: Vec::new(),
            links: HashMap::new(),
            cursor: None,
            modes: Modes::default(),
            applied_modes: Modes::default(),
            shown: vec![None; usize::from(host_rows)],
            host_cursor: HostCursor::default(),
        }
    }

    /// Takes over the host screen: alternate screen (which also saves the cursor), cleared,
    /// with the remote modes applied so far re-enabled.
    pub fn enter(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[?1049h\x1b[0m\x1b[2J\x1b[H\x1b[?25l");
        write_mode_delta(&Modes::default(), &self.applied_modes, out);
    }

    /// Gives the host screen back: every mirrored mode off, cursor shown with its default
    /// shape, alternate screen left (restoring the cursor saved by [`enter`](Self::enter)).
    pub fn leave(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0m");
        write_mode_delta(&self.applied_modes, &Modes::default(), out);
        out.extend_from_slice(b"\x1b[0 q\x1b[?25h\x1b[?1049l");
    }

    /// Replaces everything the renderer knows about the remote terminal and repaints the host.
    pub fn apply_snapshot(&mut self, s: &TerminalSnapshot, out: &mut Vec<u8>) {
        self.rows = s.lines.clone();
        self.rows.resize_with(usize::from(s.rows), Row::default);
        self.links = s.links.iter().map(|l| (l.id, l.uri.clone())).collect();
        self.cursor = Some(s.cursor);
        self.modes = s.modes;
        self.shown.fill(None);
        self.host_cursor = HostCursor::default();
        self.draw(0, out);
    }

    /// Applies the change since the last frame; only rows whose host content changes are
    /// rewritten. Writes nothing when the host already shows the result. Frames before the
    /// first snapshot are ignored.
    pub fn apply_frame(&mut self, f: &TerminalFrame, out: &mut Vec<u8>) {
        if self.cursor.is_none() {
            return;
        }
        self.links
            .extend(f.links.iter().map(|Link { id, uri }| (*id, uri.clone())));
        self.scroll_model(f.scrolled);
        for (index, row) in &f.dirty {
            if let Some(slot) = self.rows.get_mut(usize::from(*index)) {
                slot.clone_from(row);
            }
        }
        self.cursor = Some(f.cursor);
        if let Some(modes) = f.modes {
            self.modes = modes;
        }
        self.draw(f.scrolled, out);
    }

    /// The host terminal changed size (or needs a full redraw): clear and repaint, clipping or
    /// padding the remote screen to the new size.
    pub fn resize_host(&mut self, cols: u16, rows: u16, out: &mut Vec<u8>) {
        self.host_cols = cols;
        self.host_rows = rows;
        self.shown = vec![None; usize::from(rows)];
        self.host_cursor = HostCursor::default();
        out.extend_from_slice(BEGIN_UPDATE);
        out.extend_from_slice(b"\x1b[0m\x1b[2J");
        self.paint_rows(out);
        self.write_cursor(true, out);
        out.extend_from_slice(END_UPDATE);
    }

    fn scroll_model(&mut self, scrolled: i32) {
        let len = self.rows.len();
        let n = usize::try_from(scrolled.unsigned_abs()).map_or(len, |n| n.min(len));
        if n == 0 {
            return;
        }
        if scrolled > 0 {
            self.rows.rotate_left(n);
            self.rows[len - n..].fill(Row::default());
        } else {
            self.rows.rotate_right(n);
            self.rows[..n].fill(Row::default());
        }
    }

    /// One synchronized update: mode changes, host scroll, changed rows, cursor.
    fn draw(&mut self, scrolled: i32, out: &mut Vec<u8>) {
        let start = out.len();
        out.extend_from_slice(BEGIN_UPDATE);
        let body = out.len();
        self.write_modes(out);
        self.scroll_host(scrolled, out);
        let painted = self.paint_rows(out);
        self.write_cursor(painted, out);
        if out.len() == body {
            out.truncate(start);
        } else {
            out.extend_from_slice(END_UPDATE);
        }
    }

    fn write_modes(&mut self, out: &mut Vec<u8>) {
        write_mode_delta(&self.applied_modes, &self.modes, out);
        self.applied_modes = self.modes;
    }

    /// Scrolls the host screen the way the remote viewport moved, so rows the frame did not
    /// list stay correct without being rewritten.
    fn scroll_host(&mut self, scrolled: i32, out: &mut Vec<u8>) {
        if scrolled == 0 || self.shown.is_empty() {
            return;
        }
        let len = self.shown.len();
        let n = usize::try_from(scrolled.unsigned_abs()).map_or(len, |n| n.min(len));
        // Scrolling fills the new lines with the current background; start from a clean one.
        let _ = write!(
            out,
            "\x1b[0m\x1b[{n}{}",
            if scrolled > 0 { 'S' } else { 'T' }
        );
        if scrolled > 0 {
            self.shown.rotate_left(n);
            self.shown[len - n..].fill(Some(Row::default()));
        } else {
            self.shown.rotate_right(n);
            self.shown[..n].fill(Some(Row::default()));
        }
    }

    /// Rewrites every host row whose content differs from the model; returns whether any was.
    fn paint_rows(&mut self, out: &mut Vec<u8>) -> bool {
        let empty = Row::default();
        let mut painted = false;
        for index in 0..self.shown.len() {
            let want = self.rows.get(index).unwrap_or(&empty);
            if self.shown[index].as_ref() == Some(want) {
                continue;
            }
            if !painted {
                painted = true;
                self.host_cursor.hide(out);
            }
            paint_row(index, want, &self.links, self.host_cols, out);
            self.shown[index] = Some(want.clone());
        }
        if painted {
            self.host_cursor.pos = None;
        }
        painted
    }

    fn write_cursor(&mut self, painted: bool, out: &mut Vec<u8>) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let on_screen = cursor.col < self.host_cols && cursor.row < self.host_rows;
        if !(cursor.visible && on_screen) {
            self.host_cursor.hide(out);
            return;
        }
        let shape = decscusr(cursor);
        if self.host_cursor.shape != Some(shape) {
            let _ = write!(out, "\x1b[{shape} q");
            self.host_cursor.shape = Some(shape);
        }
        let pos = (cursor.col, cursor.row);
        if painted || self.host_cursor.pos != Some(pos) {
            let _ = write!(out, "\x1b[{};{}H", pos.1 + 1, pos.0 + 1);
            self.host_cursor.pos = Some(pos);
        }
        if self.host_cursor.visible != Some(true) {
            out.extend_from_slice(b"\x1b[?25h");
            self.host_cursor.visible = Some(true);
        }
    }
}

/// Synchronized update: the host shows the whole frame at once (unsupported hosts ignore it).
const BEGIN_UPDATE: &[u8] = b"\x1b[?2026h";
const END_UPDATE: &[u8] = b"\x1b[?2026l";

/// DECSCUSR parameter. Hosts have no hollow block; it falls back to a steady block.
fn decscusr(cursor: Cursor) -> u8 {
    let base = match cursor.shape {
        CursorShape::Block | CursorShape::HollowBlock => 1,
        CursorShape::Underline => 3,
        CursorShape::Bar => 5,
    };
    if cursor.shape == CursorShape::HollowBlock || !cursor.blinking {
        base + 1
    } else {
        base
    }
}

/// Emits the sequences that take the host from `from` to `to`. Alternate screen is not
/// mirrored: the daemon's viewport already is what the user sees, and the host is on its own
/// alternate screen.
fn write_mode_delta(from: &Modes, to: &Modes, out: &mut Vec<u8>) {
    let mut toggle = |was: bool, now: bool, on: &[u8], off: &[u8]| {
        if was != now {
            out.extend_from_slice(if now { on } else { off });
        }
    };
    toggle(from.app_cursor, to.app_cursor, b"\x1b[?1h", b"\x1b[?1l");
    toggle(from.app_keypad, to.app_keypad, b"\x1b=", b"\x1b>");
    toggle(
        from.bracketed_paste,
        to.bracketed_paste,
        b"\x1b[?2004h",
        b"\x1b[?2004l",
    );
    toggle(
        from.focus_events,
        to.focus_events,
        b"\x1b[?1004h",
        b"\x1b[?1004l",
    );
    toggle(
        from.alternate_scroll,
        to.alternate_scroll,
        b"\x1b[?1007h",
        b"\x1b[?1007l",
    );
    toggle(from.mouse_sgr, to.mouse_sgr, b"\x1b[?1006h", b"\x1b[?1006l");
    if from.mouse != to.mouse {
        if let Some(code) = mouse_code(from.mouse) {
            let _ = write!(out, "\x1b[?{code}l");
        }
        if let Some(code) = mouse_code(to.mouse) {
            let _ = write!(out, "\x1b[?{code}h");
        }
    }
    // Kitty flags are a stack on the host; keep exactly zero or one entry of ours on it.
    if from.kitty_keyboard != to.kitty_keyboard {
        if from.kitty_keyboard != 0 {
            out.extend_from_slice(b"\x1b[<u");
        }
        if to.kitty_keyboard != 0 {
            let _ = write!(out, "\x1b[>{}u", to.kitty_keyboard);
        }
    }
}

fn mouse_code(mode: MouseMode) -> Option<u16> {
    match mode {
        MouseMode::None => None,
        MouseMode::Click => Some(1000),
        MouseMode::Drag => Some(1002),
        MouseMode::Motion => Some(1003),
    }
}

/// Style of a run as the host sees it; the WIDE flag only affects layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    flags: CellFlags,
    underline_color: Option<Color>,
}

impl Style {
    const DEFAULT: Self = Self {
        fg: Color::Default,
        bg: Color::Default,
        flags: CellFlags::empty(),
        underline_color: None,
    };

    fn of(run: &Run) -> Self {
        Self {
            fg: run.fg,
            bg: run.bg,
            flags: run.flags.difference(CellFlags::WIDE),
            underline_color: run.underline_color,
        }
    }

    /// One SGR sequence that sets exactly this style from any prior state.
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        for (flag, code) in [
            (CellFlags::BOLD, "1"),
            (CellFlags::DIM, "2"),
            (CellFlags::ITALIC, "3"),
            (CellFlags::INVERSE, "7"),
            (CellFlags::HIDDEN, "8"),
            (CellFlags::STRIKETHROUGH, "9"),
        ] {
            if self.flags.contains(flag) {
                out.push(b';');
                out.extend_from_slice(code.as_bytes());
            }
        }
        let underline = [
            (CellFlags::UNDERCURL, "4:3"),
            (CellFlags::DOUBLE_UNDERLINE, "4:2"),
            (CellFlags::DOTTED_UNDERLINE, "4:4"),
            (CellFlags::DASHED_UNDERLINE, "4:5"),
            (CellFlags::UNDERLINE, "4"),
        ]
        .into_iter()
        .find(|(flag, _)| self.flags.contains(*flag));
        if let Some((_, code)) = underline {
            out.push(b';');
            out.extend_from_slice(code.as_bytes());
        }
        write_color(out, self.fg, 30, 90, "38");
        write_color(out, self.bg, 40, 100, "48");
        match self.underline_color {
            Some(Color::Indexed(n)) => {
                let _ = write!(out, ";58:5:{n}");
            }
            Some(Color::Rgb(r, g, b)) => {
                let _ = write!(out, ";58:2::{r}:{g}:{b}");
            }
            Some(Color::Default) | None => {}
        }
        out.push(b'm');
    }
}

fn write_color(out: &mut Vec<u8>, color: Color, base: u8, bright_base: u8, extended: &str) {
    match color {
        Color::Default => {}
        Color::Indexed(n @ 0..=7) => {
            let _ = write!(out, ";{}", base + n);
        }
        Color::Indexed(n @ 8..=15) => {
            let _ = write!(out, ";{}", bright_base + (n - 8));
        }
        Color::Indexed(n) => {
            let _ = write!(out, ";{extended};5;{n}");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{extended};2;{r};{g};{b}");
        }
    }
}

/// Columns one character of a run occupies; zero for combining marks.
fn char_width(c: char, wide: bool) -> usize {
    match c.width() {
        Some(0) => 0,
        _ if wide => 2,
        _ => 1,
    }
}

/// Paints `row` at host row `index`, clipped to `host_cols` and padded by erasing to the end
/// of the line. Leaves SGR reset and no hyperlink open.
fn paint_row(
    index: usize,
    row: &Row,
    links: &HashMap<u32, String>,
    host_cols: u16,
    out: &mut Vec<u8>,
) {
    let host_cols = usize::from(host_cols);
    let _ = write!(out, "\x1b[{};1H", index + 1);
    let mut style = Style::DEFAULT;
    let mut open_link: Option<u32> = None;
    let mut col = 0;
    'runs: for run in &row.runs {
        let wide = run.flags.contains(CellFlags::WIDE);
        let run_style = Style::of(run);
        let link = run
            .link
            .filter(|id| links.get(id).is_some_and(|uri| is_safe_uri(uri)));
        let mut started = false;
        for c in run.text.chars() {
            let replaced = c.is_control();
            let (c, width) = if replaced {
                (' ', if wide { 2 } else { 1 })
            } else {
                (c, char_width(c, wide))
            };
            if col + width > host_cols {
                break 'runs;
            }
            if !started {
                started = true;
                if run_style != style {
                    run_style.write(out);
                    style = run_style;
                }
                if link != open_link {
                    write_link(out, link, links);
                    open_link = link;
                }
            }
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf).as_bytes();
            // A replaced control character keeps the columns the cell had.
            let repeat = if replaced { width } else { 1 };
            for _ in 0..repeat {
                out.extend_from_slice(encoded);
            }
            col += width;
        }
    }
    if open_link.is_some() {
        write_link(out, None, links);
    }
    if style != Style::DEFAULT {
        out.extend_from_slice(b"\x1b[0m");
    }
    if col < host_cols {
        out.extend_from_slice(b"\x1b[K");
    }
}

fn is_safe_uri(uri: &str) -> bool {
    !uri.chars().any(char::is_control)
}

/// OSC 8 open (`Some`) or close (`None`). The `id` lets the host join separate runs of one link.
fn write_link(out: &mut Vec<u8>, link: Option<u32>, links: &HashMap<u32, String>) {
    match link.and_then(|id| links.get(&id).map(|uri| (id, uri))) {
        Some((id, uri)) => {
            let _ = write!(out, "\x1b]8;id=heeler-{id};{uri}\x1b\\");
        }
        None => out.extend_from_slice(b"\x1b]8;;\x1b\\"),
    }
}
