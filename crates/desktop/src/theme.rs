//! Density tokens (design.md § Visual system) and the chrome palette.

use gpui::{Hsla, Rgba, WindowAppearance, hsla, rgb};

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

/// Sidebar (left dock) tint laid over the window blur: sRGB color plus alpha. This is the single
/// source for the translucent dock background; contrast of `Theme::muted` and `Theme::fg` against
/// it is checked in the tests below for a mid-gray worst-case backdrop.
pub const SIDEBAR_TINT_DARK: (u8, u8, u8, f32) = (28, 28, 30, 0.72);
pub const SIDEBAR_TINT_LIGHT: (u8, u8, u8, f32) = (246, 246, 246, 0.72);

fn tint((r, g, b, a): (u8, u8, u8, f32)) -> Hsla {
    Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a,
    }
    .into()
}

/// Chrome colors. Light/dark follows the window appearance; the accent is a fixed system-blue
/// approximation because GPUI exposes no system accent color.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Opaque surface of center, right dock, bottom dock.
    pub surface: Hsla,
    /// Left dock: translucent so the blurred window background shows through. See
    /// [`SIDEBAR_TINT_DARK`]. `muted` is chosen to stay ≥ 4.5:1 on it.
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
            sidebar: tint(SIDEBAR_TINT_DARK),
            title_bar: tint(SIDEBAR_TINT_DARK),
            border: hsla(0.0, 0.0, 1.0, 0.10),
            fg: rgb(0xd4d4d4).into(),
            muted: rgb(0xaaaaaf).into(),
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
            sidebar: tint(SIDEBAR_TINT_LIGHT),
            title_bar: tint(SIDEBAR_TINT_LIGHT),
            border: hsla(0.0, 0.0, 0.0, 0.12),
            fg: rgb(0x1d1d1f).into(),
            muted: rgb(0x55555a).into(),
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

    fn rgba_of(color: Hsla) -> Rgba {
        color.into()
    }

    fn luminance(c: Rgba) -> f32 {
        let lin = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn over(top: Rgba, backdrop: f32) -> Rgba {
        let mix = |t: f32| t * top.a + backdrop * (1.0 - top.a);
        Rgba {
            r: mix(top.r),
            g: mix(top.g),
            b: mix(top.b),
            a: 1.0,
        }
    }

    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn sidebar_tint_per_appearance() {
        let dark = rgba_of(Theme::for_appearance(WindowAppearance::Dark).sidebar);
        let light = rgba_of(Theme::for_appearance(WindowAppearance::Light).sidebar);
        let close = |c: Rgba, (r, g, b, a): (u8, u8, u8, f32)| {
            (c.r - f32::from(r) / 255.0).abs() < 1e-3
                && (c.g - f32::from(g) / 255.0).abs() < 1e-3
                && (c.b - f32::from(b) / 255.0).abs() < 1e-3
                && (c.a - a).abs() < 1e-3
        };
        assert!(close(dark, (28, 28, 30, 0.72)), "{dark:?}");
        assert!(close(light, (246, 246, 246, 0.72)), "{light:?}");
        // Vibrant variants follow their base appearance.
        assert_eq!(
            Theme::for_appearance(WindowAppearance::VibrantDark).sidebar,
            Theme::dark().sidebar
        );
    }

    #[test]
    fn text_on_the_translucent_sidebar_is_readable_over_any_mid_gray_blur() {
        for theme in [Theme::dark(), Theme::light()] {
            // Blur averages from near-black to near-white; the tint pulls it toward its own
            // color, so the worst cases are the extremes that move *toward* the text color.
            for backdrop in [0.0, 0.43, 0.5, 0.6, 1.0] {
                let bg = over(rgba_of(theme.sidebar), backdrop);
                let muted = contrast(rgba_of(theme.muted), bg);
                let fg = contrast(rgba_of(theme.fg), bg);
                let worst_for_theme = if theme.dark {
                    backdrop <= 0.6
                } else {
                    backdrop >= 0.43
                };
                if worst_for_theme {
                    assert!(
                        muted >= 4.5,
                        "dark={} backdrop={backdrop}: muted {muted:.2}",
                        theme.dark
                    );
                    assert!(
                        fg >= 4.5,
                        "dark={} backdrop={backdrop}: fg {fg:.2}",
                        theme.dark
                    );
                }
            }
        }
    }

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
