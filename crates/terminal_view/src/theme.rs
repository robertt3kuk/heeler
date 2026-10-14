//! Colors: the client theme plus the application's OSC 4/10/11/12 overrides (ADR 0004).

use heeler_protocol::render::{CellFlags, Color, Palette};

/// 8-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn from_hex(hex: u32) -> Self {
        Self((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    pub(crate) const fn to_hex(self) -> u32 {
        ((self.0 as u32) << 16) | ((self.1 as u32) << 8) | self.2 as u32
    }

    /// `self` moved `t` (0..=1) of the way towards `other`.
    pub(crate) fn mix(self, other: Self, t: f32) -> Self {
        let channel = |a: u8, b: u8| {
            let a = f32::from(a);
            let b = f32::from(b);
            (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
        };
        Self(
            channel(self.0, other.0),
            channel(self.1, other.1),
            channel(self.2, other.2),
        )
    }
}

/// Fully resolved terminal colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalPalette {
    /// ANSI colors 0–7 then the bright ones 8–15.
    pub ansi: [Rgb; 16],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Background of selected cells.
    pub selection: Rgb,
}

impl TerminalPalette {
    /// Built-in `heeler-dark` (design.md § Visual system): bg `#1e1e1e`, fg `#d4d4d4`.
    pub const HEELER_DARK: Self = Self {
        ansi: [
            Rgb::from_hex(0x000000),
            Rgb::from_hex(0xcd3131),
            Rgb::from_hex(0x0dbc79),
            Rgb::from_hex(0xe5e510),
            Rgb::from_hex(0x2472c8),
            Rgb::from_hex(0xbc3fbc),
            Rgb::from_hex(0x11a8cd),
            Rgb::from_hex(0xe5e5e5),
            Rgb::from_hex(0x666666),
            Rgb::from_hex(0xf14c4c),
            Rgb::from_hex(0x23d18b),
            Rgb::from_hex(0xf5f543),
            Rgb::from_hex(0x3b8eea),
            Rgb::from_hex(0xd670d6),
            Rgb::from_hex(0x29b8db),
            Rgb::from_hex(0xffffff),
        ],
        foreground: Rgb::from_hex(0xd4d4d4),
        background: Rgb::from_hex(0x1e1e1e),
        cursor: Rgb::from_hex(0xd4d4d4),
        selection: Rgb::from_hex(0x264f78),
    };

    /// Built-in `heeler-light`.
    pub const HEELER_LIGHT: Self = Self {
        ansi: [
            Rgb::from_hex(0x000000),
            Rgb::from_hex(0xcd3131),
            Rgb::from_hex(0x00bc00),
            Rgb::from_hex(0x949800),
            Rgb::from_hex(0x0451a5),
            Rgb::from_hex(0xbc05bc),
            Rgb::from_hex(0x0598bc),
            Rgb::from_hex(0x555555),
            Rgb::from_hex(0x666666),
            Rgb::from_hex(0xcd3131),
            Rgb::from_hex(0x14ce14),
            Rgb::from_hex(0xb5ba00),
            Rgb::from_hex(0x0451a5),
            Rgb::from_hex(0xbc05bc),
            Rgb::from_hex(0x0598b3),
            Rgb::from_hex(0xa5a5a5),
        ],
        foreground: Rgb::from_hex(0x333333),
        background: Rgb::from_hex(0xffffff),
        cursor: Rgb::from_hex(0x333333),
        selection: Rgb::from_hex(0xadd6ff),
    };
}

impl Default for TerminalPalette {
    fn default() -> Self {
        Self::HEELER_DARK
    }
}

/// The colors a client reports to the daemon so OSC 10/11/12 replies match what is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
}

impl From<&TerminalPalette> for DefaultColors {
    fn from(palette: &TerminalPalette) -> Self {
        Self {
            foreground: palette.foreground,
            background: palette.background,
            cursor: palette.cursor,
        }
    }
}

/// Theme palette with the application's overrides applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    base: TerminalPalette,
    indexed: Vec<(u8, Rgb)>,
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    cursor: Option<Rgb>,
}

/// Foreground and background of a run after flags (inverse, dim, hidden) are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedColors {
    pub fg: Rgb,
    pub bg: Rgb,
    /// The cell background differs from the terminal background and must be painted.
    pub paint_bg: bool,
}

impl Theme {
    pub fn new(base: TerminalPalette) -> Self {
        Self {
            base,
            indexed: Vec::new(),
            foreground: None,
            background: None,
            cursor: None,
        }
    }

    pub fn set_base(&mut self, base: TerminalPalette) {
        self.base = base;
    }

    pub fn set_overrides(&mut self, palette: &Palette) {
        self.indexed = palette
            .indexed
            .iter()
            .map(|(i, (r, g, b))| (*i, Rgb(*r, *g, *b)))
            .collect();
        let rgb = |c: Option<(u8, u8, u8)>| c.map(|(r, g, b)| Rgb(r, g, b));
        self.foreground = rgb(palette.foreground);
        self.background = rgb(palette.background);
        self.cursor = rgb(palette.cursor);
    }

    /// Hash of everything that affects resolved colors.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut h = DefaultHasher::new();
        self.base.hash(&mut h);
        self.indexed.hash(&mut h);
        (self.foreground, self.background, self.cursor).hash(&mut h);
        h.finish()
    }

    pub fn base(&self) -> &TerminalPalette {
        &self.base
    }

    pub fn foreground(&self) -> Rgb {
        self.foreground.unwrap_or(self.base.foreground)
    }

    pub fn background(&self) -> Rgb {
        self.background.unwrap_or(self.base.background)
    }

    pub fn cursor(&self) -> Rgb {
        self.cursor.unwrap_or(self.base.cursor)
    }

    pub fn selection(&self) -> Rgb {
        self.base.selection
    }

    pub fn indexed(&self, index: u8) -> Rgb {
        if let Some((_, rgb)) = self.indexed.iter().rev().find(|(i, _)| *i == index) {
            return *rgb;
        }
        match index {
            0..=15 => self.base.ansi[usize::from(index)],
            16..=231 => {
                let n = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
            }
            232..=255 => {
                let v = 8 + (index - 232) * 10;
                Rgb(v, v, v)
            }
        }
    }

    pub fn resolve(&self, color: Color, default: Rgb) -> Rgb {
        match color {
            Color::Default => default,
            Color::Indexed(i) => self.indexed(i),
            Color::Rgb(r, g, b) => Rgb(r, g, b),
        }
    }

    /// Colors a run is drawn with.
    pub fn resolve_run(&self, fg: Color, bg: Color, flags: CellFlags) -> ResolvedColors {
        let default_fg = self.foreground();
        let default_bg = self.background();
        let mut fg_rgb = self.resolve(fg, default_fg);
        let mut bg_rgb = self.resolve(bg, default_bg);
        let mut paint_bg = bg != Color::Default;
        if flags.contains(CellFlags::INVERSE) {
            std::mem::swap(&mut fg_rgb, &mut bg_rgb);
            paint_bg = true;
        }
        if flags.contains(CellFlags::HIDDEN) {
            fg_rgb = bg_rgb;
        } else if flags.contains(CellFlags::DIM) {
            fg_rgb = fg_rgb.mix(bg_rgb, 0.4);
        }
        ResolvedColors {
            fg: fg_rgb,
            bg: bg_rgb,
            paint_bg: paint_bg && bg_rgb != default_bg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_is_heeler_dark() {
        let p = TerminalPalette::default();
        assert_eq!(p.background, Rgb(0x1e, 0x1e, 0x1e));
        assert_eq!(p.foreground, Rgb(0xd4, 0xd4, 0xd4));
        assert_eq!(p.cursor, Rgb(0xd4, 0xd4, 0xd4));
    }

    #[test]
    fn indexed_colors_use_the_xterm_cube_and_ramp() {
        let theme = Theme::new(TerminalPalette::default());
        assert_eq!(theme.indexed(16), Rgb(0, 0, 0));
        assert_eq!(theme.indexed(196), Rgb(255, 0, 0));
        assert_eq!(theme.indexed(21), Rgb(0, 0, 255));
        assert_eq!(theme.indexed(231), Rgb(255, 255, 255));
        assert_eq!(theme.indexed(232), Rgb(8, 8, 8));
        assert_eq!(theme.indexed(255), Rgb(238, 238, 238));
        assert_eq!(theme.indexed(1), TerminalPalette::HEELER_DARK.ansi[1]);
    }

    #[test]
    fn snapshot_overrides_win_over_the_theme() {
        let mut theme = Theme::new(TerminalPalette::default());
        theme.set_overrides(&Palette {
            indexed: vec![(1, (1, 2, 3)), (200, (9, 9, 9))],
            foreground: Some((10, 20, 30)),
            background: Some((40, 50, 60)),
            cursor: None,
        });
        assert_eq!(theme.indexed(1), Rgb(1, 2, 3));
        assert_eq!(theme.indexed(200), Rgb(9, 9, 9));
        assert_eq!(theme.foreground(), Rgb(10, 20, 30));
        assert_eq!(theme.background(), Rgb(40, 50, 60));
        assert_eq!(theme.cursor(), TerminalPalette::default().cursor);
        theme.set_overrides(&Palette::default());
        assert_eq!(theme.foreground(), TerminalPalette::default().foreground);
    }

    #[test]
    fn inverse_swaps_and_forces_a_background() {
        let theme = Theme::new(TerminalPalette::default());
        let c = theme.resolve_run(Color::Default, Color::Default, CellFlags::INVERSE);
        assert_eq!(c.fg, theme.background());
        assert_eq!(c.bg, theme.foreground());
        assert!(c.paint_bg);
        let plain = theme.resolve_run(Color::Default, Color::Default, CellFlags::empty());
        assert!(!plain.paint_bg);
    }

    #[test]
    fn hidden_text_takes_the_background_color() {
        let theme = Theme::new(TerminalPalette::default());
        let c = theme.resolve_run(Color::Indexed(1), Color::Indexed(4), CellFlags::HIDDEN);
        assert_eq!(c.fg, c.bg);
    }
}
