//! Terminal color themes. Built-ins: `hutan-dark` and `hutan-light` (design.md § Visual system:
//! backgrounds match the chrome `bg` token, ANSI colors tuned for WCAG AA on them).

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Theme selector that follows the system appearance.
pub const AUTO: &str = "auto";
pub const DARK: &str = "hutan-dark";
pub const LIGHT: &str = "hutan-light";

/// 8-bit sRGB color. Serialized as `#rrggbb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `0xRRGGBB`.
    pub const fn from_u32(rgb: u32) -> Self {
        Self::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid color `{0}`; expected #rrggbb")]
pub struct ParseRgbError(String);

impl FromStr for Rgb {
    type Err = ParseRgbError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ParseRgbError(s.to_owned());
        let hex = s.strip_prefix('#').ok_or_else(bad)?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        let value = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
        Ok(Self::from_u32(value))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// What the OS currently looks like; supplied by the desktop (GPUI knows, this crate does not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemAppearance {
    Light,
    Dark,
}

/// A complete terminal palette.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalTheme {
    pub name: String,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    /// ANSI colors 0–7 (normal) then 8–15 (bright).
    pub ansi: [Rgb; 16],
}

const fn palette(colors: [u32; 16]) -> [Rgb; 16] {
    let mut out = [Rgb::new(0, 0, 0); 16];
    let mut i = 0;
    while i < 16 {
        out[i] = Rgb::from_u32(colors[i]);
        i += 1;
    }
    out
}

static BUILTIN: LazyLock<[TerminalTheme; 2]> = LazyLock::new(|| {
    [
        TerminalTheme {
            name: DARK.to_owned(),
            foreground: Rgb::from_u32(0xeae8e1),
            background: Rgb::from_u32(0x262624),
            cursor: Rgb::from_u32(0xea7a43),
            selection: Rgb::from_u32(0x4b3b30),
            ansi: palette([
                0x4a4944, 0xe87562, 0x6bbf85, 0xe2ae45, 0x72a5ec, 0xc996d8, 0x5fc2c6, 0xd6d4cc,
                0x7a7870, 0xf08c7a, 0x86d19c, 0xeec261, 0x8fb9f2, 0xdaaee6, 0x7dd3d6, 0xf5f4ef,
            ]),
        },
        TerminalTheme {
            name: LIGHT.to_owned(),
            foreground: Rgb::from_u32(0x1f1e1d),
            background: Rgb::from_u32(0xfaf9f5),
            cursor: Rgb::from_u32(0xe0692e),
            selection: Rgb::from_u32(0xf3d5c3),
            ansi: palette([
                0x1f1e1d, 0xb93a26, 0x2f7a48, 0x8a6300, 0x2b62b8, 0x8e4ea8, 0x167780, 0x6f6d66,
                0x57554f, 0xc8452f, 0x3f8f5a, 0x9a6f00, 0x3d77d1, 0xa35fbf, 0x23909a, 0x8c8a82,
            ]),
        },
    ]
});

/// All built-in themes: `hutan-dark`, `hutan-light`.
pub fn builtin() -> &'static [TerminalTheme] {
    BUILTIN.as_slice()
}

/// Built-in theme by exact name. `auto` is not a theme; use [`resolve`].
pub fn lookup(name: &str) -> Option<&'static TerminalTheme> {
    builtin().iter().find(|t| t.name == name)
}

/// The theme for a `terminal.theme` value: `auto` follows `system`; a known name selects that
/// theme; an unknown name falls back to the system-appropriate built-in (callers that want to
/// surface the typo can check [`lookup`] first).
pub fn resolve(selector: &str, system: SystemAppearance) -> &'static TerminalTheme {
    let by_system = match system {
        SystemAppearance::Dark => DARK,
        SystemAppearance::Light => LIGHT,
    };
    let name = if selector == AUTO {
        by_system
    } else {
        selector
    };
    lookup(name)
        .or_else(|| lookup(by_system))
        .unwrap_or(&BUILTIN[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_backgrounds_match_the_chrome_bg_token() {
        let dark = lookup("hutan-dark").unwrap();
        assert_eq!(dark.background, Rgb::new(0x26, 0x26, 0x24));
        assert_eq!(dark.ansi.len(), 16);
        let light = lookup("hutan-light").unwrap();
        assert_eq!(light.background, Rgb::new(0xfa, 0xf9, 0xf5));
    }

    fn luminance(c: Rgb) -> f64 {
        let lin = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn ansi_colors_are_readable_on_the_theme_background() {
        for theme in builtin() {
            let bg = theme.background;
            assert!(contrast(theme.foreground, bg) >= 7.0, "{} fg", theme.name);
            // The six hues (red..cyan) and white/black as far as they are text colors must
            // meet WCAG AA for normal text; ANSI black on the dark theme and white on the
            // light one are the "paper" colors of the other polarity and stay visible (≥ 1.5).
            for index in 1..=6 {
                let ratio = contrast(theme.ansi[index], bg);
                assert!(ratio >= 4.5, "{} ansi {index}: {ratio:.2}", theme.name);
            }
            let (text_neutral, paper_neutral) = if theme.name == DARK { (7, 0) } else { (0, 7) };
            assert!(
                contrast(theme.ansi[text_neutral], bg) >= 4.5,
                "{}",
                theme.name
            );
            assert!(
                contrast(theme.ansi[paper_neutral], bg) >= 1.5,
                "{}",
                theme.name
            );
            for index in 8..16 {
                let ratio = contrast(theme.ansi[index], bg);
                assert!(
                    ratio >= 3.0,
                    "{} bright ansi {index}: {ratio:.2}",
                    theme.name
                );
            }
            assert!(
                contrast(theme.foreground, theme.selection) >= 7.0,
                "{} selection",
                theme.name
            );
        }
    }

    #[test]
    fn lookup_by_name() {
        assert_eq!(lookup("hutan-light").unwrap().name, "hutan-light");
        assert!(lookup("hutan-dark").is_some());
        assert!(lookup("auto").is_none());
        assert!(lookup("nope").is_none());
        assert_eq!(builtin().len(), 2);
    }

    #[test]
    fn auto_follows_system_appearance() {
        assert_eq!(resolve("auto", SystemAppearance::Dark).name, "hutan-dark");
        assert_eq!(resolve("auto", SystemAppearance::Light).name, "hutan-light");
    }

    #[test]
    fn pinned_name_ignores_system_and_unknown_falls_back() {
        assert_eq!(
            resolve("hutan-dark", SystemAppearance::Light).name,
            "hutan-dark"
        );
        assert_eq!(
            resolve("hutan-light", SystemAppearance::Dark).name,
            "hutan-light"
        );
        assert_eq!(resolve("nope", SystemAppearance::Light).name, "hutan-light");
        assert_eq!(resolve("nope", SystemAppearance::Dark).name, "hutan-dark");
    }

    #[test]
    fn light_theme_is_light() {
        let t = lookup("hutan-light").unwrap();
        assert!(u32::from(t.background.r) > u32::from(t.foreground.r));
    }

    #[test]
    fn rgb_round_trips_through_hex() {
        let c: Rgb = "#1e1E1e".parse().unwrap();
        assert_eq!(c, Rgb::new(0x1e, 0x1e, 0x1e));
        assert_eq!(c.to_string(), "#1e1e1e");
        for bad in ["1e1e1e", "#1e1e1", "#1e1e1e1e", "#gggggg", "", "#"] {
            assert!(bad.parse::<Rgb>().is_err(), "{bad}");
        }
    }

    #[test]
    fn theme_serializes_as_hex_strings() {
        let json = serde_json::to_value(lookup("hutan-dark").unwrap()).unwrap();
        assert_eq!(json["background"], "#262624");
        assert_eq!(json["ansi"].as_array().unwrap().len(), 16);
        let back: TerminalTheme = serde_json::from_value(json).unwrap();
        assert_eq!(&back, lookup("hutan-dark").unwrap());
    }
}
