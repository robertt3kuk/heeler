//! Plain view configuration. Wave 2 maps the `terminal` settings namespace onto it.

use crate::theme::{Rgb, TerminalPalette};

/// Which Option key acts as Meta (ESC prefix) instead of composing characters.
///
/// GPUI's keystroke carries a single `alt` flag, so the two physical keys cannot be told apart:
/// every variant except [`OptionAsMeta::None`] makes Option act as Meta on both keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptionAsMeta {
    None,
    #[default]
    Left,
    Right,
    Both,
}

impl OptionAsMeta {
    pub(crate) fn enabled(self) -> bool {
        self != Self::None
    }
}

/// Cursor shape the user prefers for applications that do not ask for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorSettings {
    pub style: CursorStyle,
    pub blink: bool,
}

impl Default for CursorSettings {
    fn default() -> Self {
        Self {
            style: CursorStyle::Block,
            blink: true,
        }
    }
}

/// The block overlay (gutter, separators, chips, folds, actions bar).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockSettings {
    /// Draw the overlay; false makes the terminal look as if blocks did not exist.
    pub enabled: bool,
    /// Width of the gutter bar in logical pixels (a density token); it is drawn inside the
    /// padding at the left edge, the grid never moves.
    pub gutter_width: f32,
    /// Gutter bar and chip of a command that exited 0.
    pub success: Rgb,
    /// Gutter bar and chip of a command that failed.
    pub error: Rgb,
    /// Running commands, commands without an exit code, hover and highlight tints.
    pub neutral: Rgb,
    /// Accent for the highlighted block.
    pub accent: Rgb,
    /// Animate the activity indicator of a running block (false under Reduce Motion).
    pub animate: bool,
}

impl Default for BlockSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            gutter_width: 3.0,
            success: Rgb(0x30, 0xd1, 0x58),
            error: Rgb(0xff, 0x45, 0x3a),
            neutral: Rgb(0xaa, 0xaa, 0xaf),
            accent: Rgb(0x0a, 0x84, 0xff),
            animate: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalViewSettings {
    pub font_family: String,
    pub font_size: f32,
    /// Multiple of `font_size`.
    pub line_height: f32,
    pub palette: TerminalPalette,
    pub cursor: CursorSettings,
    pub option_as_meta: OptionAsMeta,
    pub copy_on_select: bool,
    pub scroll_to_bottom_on_input: bool,
    /// Space between the view's edge and the grid, on all four sides, in logical pixels. The
    /// grid (columns and rows, mouse hit-testing) is laid out in the padded content box.
    pub padding: f32,
    pub blocks: BlockSettings,
}

impl Default for TerminalViewSettings {
    fn default() -> Self {
        Self {
            font_family: "Menlo".to_owned(),
            font_size: 13.0,
            line_height: 1.2,
            palette: TerminalPalette::default(),
            cursor: CursorSettings::default(),
            option_as_meta: OptionAsMeta::Left,
            copy_on_select: false,
            scroll_to_bottom_on_input: true,
            padding: 0.0,
            blocks: BlockSettings::default(),
        }
    }
}
