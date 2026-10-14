//! Density tokens (design.md § Visual system) and the chrome palette.

use gpui::{Hsla, WindowAppearance, hsla, rgb};

/// `compact` (default) or `comfortable`. Wave 2 binds this to `appearance.density`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Density {
    #[default]
    Compact,
    Comfortable,
}

/// Sizes in logical pixels, straight from the density token table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub sidebar_row: f32,
    pub tab_height: f32,
    pub pane_header: f32,
    pub status_bar: f32,
    pub spacing: f32,
    pub radius: f32,
    /// UI text size: 12 pt compact, 13 pt comfortable.
    pub font_size: f32,
    /// Uppercase section headers: 10 pt semibold.
    pub section_font_size: f32,
}

impl Density {
    pub fn tokens(self) -> Tokens {
        match self {
            Density::Compact => Tokens {
                sidebar_row: 24.0,
                tab_height: 28.0,
                pane_header: 28.0,
                status_bar: 22.0,
                spacing: 4.0,
                radius: 6.0,
                font_size: 12.0,
                section_font_size: 10.0,
            },
            Density::Comfortable => Tokens {
                sidebar_row: 30.0,
                tab_height: 32.0,
                pane_header: 34.0,
                status_bar: 26.0,
                spacing: 6.0,
                radius: 8.0,
                font_size: 13.0,
                section_font_size: 10.0,
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Density::Compact => "Compact",
            Density::Comfortable => "Comfortable",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Density::Compact => Density::Comfortable,
            Density::Comfortable => Density::Compact,
        }
    }
}

/// Chrome colors. Light/dark follows the window appearance; the accent is a fixed system-blue
/// approximation because GPUI exposes no system accent color.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Opaque surface of center, right dock, bottom dock.
    pub surface: Hsla,
    /// Left dock: translucent so the blurred window background shows through.
    pub sidebar: Hsla,
    pub title_bar: Hsla,
    pub border: Hsla,
    pub fg: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub warning: Hsla,
    pub hover: Hsla,
    pub active_tab: Hsla,
    pub overlay: Hsla,
    pub drop_zone: Hsla,
}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    pub fn dark() -> Self {
        let accent: Hsla = rgb(0x0a84ff).into();
        Self {
            dark: true,
            surface: rgb(0x1e1e1e).into(),
            sidebar: hsla(0.0, 0.0, 0.12, 0.55),
            title_bar: hsla(0.0, 0.0, 0.12, 0.55),
            border: hsla(0.0, 0.0, 1.0, 0.10),
            fg: rgb(0xd4d4d4).into(),
            muted: rgb(0x8a8a8e).into(),
            accent,
            warning: rgb(0xff9f0a).into(),
            hover: hsla(0.0, 0.0, 1.0, 0.07),
            active_tab: hsla(0.0, 0.0, 1.0, 0.10),
            overlay: rgb(0x2a2a2c).into(),
            drop_zone: accent.opacity(0.28),
        }
    }

    pub fn light() -> Self {
        let accent: Hsla = rgb(0x007aff).into();
        Self {
            dark: false,
            surface: rgb(0xffffff).into(),
            sidebar: hsla(0.0, 0.0, 0.96, 0.60),
            title_bar: hsla(0.0, 0.0, 0.96, 0.60),
            border: hsla(0.0, 0.0, 0.0, 0.12),
            fg: rgb(0x1d1d1f).into(),
            muted: rgb(0x6e6e73).into(),
            accent,
            warning: rgb(0xff9500).into(),
            hover: hsla(0.0, 0.0, 0.0, 0.06),
            active_tab: hsla(0.0, 0.0, 0.0, 0.08),
            overlay: rgb(0xf5f5f7).into(),
            drop_zone: accent.opacity(0.22),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_design_table() {
        let c = Density::Compact.tokens();
        assert_eq!(
            (
                c.sidebar_row,
                c.tab_height,
                c.pane_header,
                c.status_bar,
                c.spacing,
                c.radius
            ),
            (24.0, 28.0, 28.0, 22.0, 4.0, 6.0)
        );
        let m = Density::Comfortable.tokens();
        assert_eq!(
            (
                m.sidebar_row,
                m.tab_height,
                m.pane_header,
                m.status_bar,
                m.spacing,
                m.radius
            ),
            (30.0, 32.0, 34.0, 26.0, 6.0, 8.0)
        );
        assert_eq!((c.font_size, m.font_size), (12.0, 13.0));
        assert_eq!(Density::default(), Density::Compact);
    }
}
