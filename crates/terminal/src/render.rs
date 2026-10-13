//! Engine grid → protocol render DTOs, the hyperlink table and the per-client diff tracker.
//!
//! Row conventions (clients must follow them when painting):
//! * A [`Row`] lists runs left to right; a wide character counts as two columns, its spacer
//!   cell is not represented. Cells to the right of the last run are blank with the default
//!   style (trailing default-style blanks are trimmed so an empty 200-column row is empty).
//! * Combining characters are appended to the text of their base character.

use std::collections::HashMap;

use alacritty_terminal::grid::Row as GridRow;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::vte::ansi::Color as EngineColor;
use heeler_protocol::render::{CellFlags, Color, Cursor, Link, Modes, Palette, Row, Run};

/// Upper bound on distinct hyperlinks remembered per terminal (ids are stable for the
/// terminal's lifetime, so the table cannot shrink). Links beyond the cap render unlinked.
pub(crate) const MAX_LINKS: usize = 16 * 1024;

/// Interns hyperlinks to dense ids (`id == index`), stable for the terminal's lifetime.
#[derive(Debug, Default)]
pub(crate) struct LinkTable {
    ids: HashMap<Hyperlink, u32>,
    list: Vec<Link>,
}

impl LinkTable {
    fn intern(&mut self, link: &Hyperlink) -> Option<u32> {
        if let Some(&id) = self.ids.get(link) {
            return Some(id);
        }
        if self.list.len() >= MAX_LINKS {
            return None;
        }
        let id = u32::try_from(self.list.len()).ok()?;
        self.list.push(Link {
            id,
            uri: link.uri().to_owned(),
        });
        self.ids.insert(link.clone(), id);
        Some(id)
    }

    pub(crate) fn len(&self) -> usize {
        self.list.len()
    }

    pub(crate) fn all(&self) -> Vec<Link> {
        self.list.clone()
    }

    /// Links with an index `>= from`.
    pub(crate) fn since(&self, from: usize) -> Vec<Link> {
        self.list
            .get(from..)
            .map(<[Link]>::to_vec)
            .unwrap_or_default()
    }
}

/// Per-client record of what was last sent, so [`crate::Emulator::frame`] can emit only changes.
///
/// A fresh tracker always gets a full snapshot first. `scrolled` in frames stays 0 in this
/// implementation: a scrolling viewport is reported as the rows that differ.
#[derive(Debug, Default)]
pub struct RenderTracker {
    pub(crate) synced: bool,
    pub(crate) frame: u64,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) lines: Vec<Row>,
    pub(crate) cursor: Option<Cursor>,
    pub(crate) modes: Modes,
    pub(crate) palette: Palette,
    pub(crate) title: Option<String>,
    pub(crate) links_sent: usize,
    pub(crate) history_len: u32,
    pub(crate) generation: u64,
}

impl RenderTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of the last snapshot or frame produced for this tracker (0 = none yet).
    pub fn frame_number(&self) -> u64 {
        self.frame
    }

    /// True once a snapshot has been produced for this tracker.
    pub fn is_synced(&self) -> bool {
        self.synced
    }
}

/// Style identity of a run, everything except the text.
#[derive(Clone, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    flags: CellFlags,
    underline_color: Option<Color>,
    link: Option<u32>,
}

impl Style {
    fn is_default(&self) -> bool {
        self.fg == Color::Default
            && self.bg == Color::Default
            && self.flags.is_empty()
            && self.underline_color.is_none()
            && self.link.is_none()
    }
}

pub(crate) fn convert_color(color: EngineColor) -> Color {
    match color {
        EngineColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        EngineColor::Indexed(index) => Color::Indexed(index),
        EngineColor::Named(named) => match named as usize {
            index @ 0..=15 => Color::Indexed(index as u8),
            // Dim variants of the eight base colours (the DIM flag carries the dimming).
            index @ 259..=266 => Color::Indexed((index - 259) as u8),
            // Foreground, background, cursor and the bright/dim defaults: client theme.
            _ => Color::Default,
        },
    }
}

fn convert_flags(flags: Flags) -> CellFlags {
    const MAP: [(Flags, CellFlags); 11] = [
        (Flags::BOLD, CellFlags::BOLD),
        (Flags::DIM, CellFlags::DIM),
        (Flags::ITALIC, CellFlags::ITALIC),
        (Flags::UNDERLINE, CellFlags::UNDERLINE),
        (Flags::DOUBLE_UNDERLINE, CellFlags::DOUBLE_UNDERLINE),
        (Flags::UNDERCURL, CellFlags::UNDERCURL),
        (Flags::DOTTED_UNDERLINE, CellFlags::DOTTED_UNDERLINE),
        (Flags::DASHED_UNDERLINE, CellFlags::DASHED_UNDERLINE),
        (Flags::INVERSE, CellFlags::INVERSE),
        (Flags::HIDDEN, CellFlags::HIDDEN),
        (Flags::STRIKEOUT, CellFlags::STRIKETHROUGH),
    ];
    let mut out = CellFlags::empty();
    for (engine, dto) in MAP {
        if flags.intersects(engine) {
            out |= dto;
        }
    }
    if flags.contains(Flags::WIDE_CHAR) {
        out |= CellFlags::WIDE;
    }
    out
}

fn cell_style(cell: &Cell, links: &mut LinkTable) -> Style {
    let link = cell.hyperlink().and_then(|link| links.intern(&link));
    Style {
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        flags: convert_flags(cell.flags),
        underline_color: cell.underline_color().map(convert_color),
        link,
    }
}

/// Converts one engine row of `cols` cells.
pub(crate) fn convert_row(row: &GridRow<Cell>, cols: usize, links: &mut LinkTable) -> Row {
    let mut runs: Vec<Run> = Vec::new();
    let mut last_style: Option<Style> = None;
    for col in 0..cols.min(row.len()) {
        let cell = &row[Column(col)];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let style = cell_style(cell, links);
        if last_style.as_ref() != Some(&style) {
            runs.push(Run {
                text: String::new(),
                fg: style.fg,
                bg: style.bg,
                flags: style.flags,
                underline_color: style.underline_color,
                link: style.link,
            });
            last_style = Some(style);
        }
        if let Some(run) = runs.last_mut() {
            run.text.push(if cell.c == '\t' { ' ' } else { cell.c });
            if let Some(extra) = cell.zerowidth() {
                run.text.extend(extra);
            }
        }
    }
    trim_trailing_blanks(&mut runs);
    let wrapped =
        cols > 0 && row.len() >= cols && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
    Row { runs, wrapped }
}

fn trim_trailing_blanks(runs: &mut Vec<Run>) {
    while let Some(run) = runs.last_mut() {
        let default_style = Style {
            fg: run.fg,
            bg: run.bg,
            flags: run.flags,
            underline_color: run.underline_color,
            link: run.link,
        }
        .is_default();
        if !default_style {
            break;
        }
        let kept = run.text.trim_end_matches(' ').len();
        run.text.truncate(kept);
        if run.text.is_empty() {
            runs.pop();
        } else {
            break;
        }
    }
}

/// Plain text of a row (wide characters count once), trailing spaces removed.
pub(crate) fn row_text(row: &Row) -> String {
    let mut text: String = row.runs.iter().map(|run| run.text.as_str()).collect();
    text.truncate(text.trim_end_matches(' ').len());
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

    #[test]
    fn named_colors_map_to_indexed_or_default() {
        assert_eq!(
            convert_color(EngineColor::Named(NamedColor::Red)),
            Color::Indexed(1)
        );
        assert_eq!(
            convert_color(EngineColor::Named(NamedColor::BrightWhite)),
            Color::Indexed(15)
        );
        assert_eq!(
            convert_color(EngineColor::Named(NamedColor::DimGreen)),
            Color::Indexed(2)
        );
        assert_eq!(
            convert_color(EngineColor::Named(NamedColor::Foreground)),
            Color::Default
        );
        assert_eq!(
            convert_color(EngineColor::Named(NamedColor::Background)),
            Color::Default
        );
        assert_eq!(
            convert_color(EngineColor::Named(NamedColor::Cursor)),
            Color::Default
        );
        assert_eq!(
            convert_color(EngineColor::Indexed(196)),
            Color::Indexed(196)
        );
        assert_eq!(
            convert_color(EngineColor::Spec(Rgb { r: 1, g: 2, b: 3 })),
            Color::Rgb(1, 2, 3)
        );
    }
}
