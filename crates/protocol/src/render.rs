//! Terminal render DTOs (ADR 0004). The daemon is the only VT parser; clients draw these.
//!
//! Flow: `terminal.attach` → one [`TerminalSnapshot`], then [`TerminalFrame`]s, each sent only
//! after the client acks the previous frame (`terminal.ack`), so slow clients skip states
//! instead of queueing them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TerminalRender {
    Snapshot(TerminalSnapshot),
    Frame(TerminalFrame),
}

/// Complete visible state; replaces anything the client had for this terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub frame: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    pub modes: Modes,
    pub palette: Palette,
    pub title: Option<String>,
    /// Exactly `rows` entries, top to bottom.
    pub lines: Vec<Row>,
    pub links: Vec<Link>,
    /// Lines of scrollback available through `terminal.history`.
    pub history_len: u32,
}

/// Changes since the frame the client last acked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalFrame {
    pub frame: u64,
    /// Lines the viewport content moved up (positive) before `dirty` applies.
    pub scrolled: i32,
    /// `(row index, new content)`; rows not listed are unchanged after scrolling.
    pub dirty: Vec<(u16, Row)>,
    pub cursor: Cursor,
    /// Present only when modes changed.
    pub modes: Option<Modes>,
    /// Present only when the title changed; `Some(None)` clears it.
    pub title: Option<Option<String>>,
    /// Hyperlinks first referenced in this frame.
    pub links: Vec<Link>,
    pub history_len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Row {
    pub runs: Vec<Run>,
    /// The line continues on the next row (soft wrap).
    pub wrapped: bool,
}

/// Consecutive cells sharing one style. Wide characters occupy two columns; their spacer
/// cell is omitted from `text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub underline_color: Option<Color>,
    /// Index into the terminal's hyperlink table.
    pub link: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    /// Default foreground or background, depending on position.
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
    pub struct CellFlags: u16 {
        const BOLD = 1 << 0;
        const DIM = 1 << 1;
        const ITALIC = 1 << 2;
        const UNDERLINE = 1 << 3;
        const DOUBLE_UNDERLINE = 1 << 4;
        const UNDERCURL = 1 << 5;
        const DOTTED_UNDERLINE = 1 << 6;
        const DASHED_UNDERLINE = 1 << 7;
        const INVERSE = 1 << 8;
        const HIDDEN = 1 << 9;
        const STRIKETHROUGH = 1 << 10;
        /// Run consists of double-width characters.
        const WIDE = 1 << 11;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    pub visible: bool,
    pub blinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
    HollowBlock,
}

/// Terminal modes clients need for input encoding and rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modes {
    pub alt_screen: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    pub focus_events: bool,
    pub alternate_scroll: bool,
    /// Kitty keyboard protocol progressive-enhancement flags (0 = legacy encoding).
    pub kitty_keyboard: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseMode {
    #[default]
    None,
    /// Button press/release only.
    Click,
    /// Press/release plus motion while a button is held.
    Drag,
    /// All motion.
    Motion,
}

/// Colors changed by the application (OSC 4/10/11/12); everything else is the client theme.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Palette {
    pub indexed: Vec<(u8, (u8, u8, u8))>,
    pub foreground: Option<(u8, u8, u8)>,
    pub background: Option<(u8, u8, u8)>,
    pub cursor: Option<(u8, u8, u8)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub id: u32,
    pub uri: String,
}
