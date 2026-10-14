//! Terminal color themes. Built-ins: `heeler-dark` (matches the daemon's OSC 10/11/12 defaults)
//! and `heeler-light`.

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Theme selector that follows the system appearance.
pub const AUTO: &str = "auto";
pub const DARK: &str = "heeler-dark";
pub const LIGHT: &str = "heeler-light";

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
            foreground: Rgb::from_u32(0xd4d4d4),
            background: Rgb::from_u32(0x1e1e1e),
            cursor: Rgb::from_u32(0xd4d4d4),
            selection: Rgb::from_u32(0x264f78),
            ansi: palette([
                0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5,
                0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
            ]),
        },
        TerminalTheme {
            name: LIGHT.to_owned(),
            foreground: Rgb::from_u32(0x1f1f1f),
            background: Rgb::from_u32(0xffffff),
            cursor: Rgb::from_u32(0x1f1f1f),
            selection: Rgb::from_u32(0xadd6ff),
            ansi: palette([
                0x000000, 0xcd3131, 0x00bc00, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555,
                0x666666, 0xcd3131, 0x14ce14, 0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
            ]),
        },
    ]
});

/// All built-in themes: `heeler-dark`, `heeler-light`.
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
    fn dark_theme_matches_design_and_daemon_defaults() {
        let t = lookup("heeler-dark").unwrap();
        assert_eq!(t.background, Rgb::new(0x1e, 0x1e, 0x1e));
        assert_eq!(t.foreground, Rgb::new(0xd4, 0xd4, 0xd4));
        assert_eq!(t.cursor, Rgb::new(0xd4, 0xd4, 0xd4));
        assert_eq!(t.ansi.len(), 16);
    }

    #[test]
    fn lookup_by_name() {
        assert_eq!(lookup("heeler-light").unwrap().name, "heeler-light");
        assert!(lookup("heeler-dark").is_some());
        assert!(lookup("auto").is_none());
        assert!(lookup("nope").is_none());
        assert_eq!(builtin().len(), 2);
    }

    #[test]
    fn auto_follows_system_appearance() {
        assert_eq!(resolve("auto", SystemAppearance::Dark).name, "heeler-dark");
        assert_eq!(
            resolve("auto", SystemAppearance::Light).name,
            "heeler-light"
        );
    }

    #[test]
    fn pinned_name_ignores_system_and_unknown_falls_back() {
        assert_eq!(
            resolve("heeler-dark", SystemAppearance::Light).name,
            "heeler-dark"
        );
        assert_eq!(
            resolve("heeler-light", SystemAppearance::Dark).name,
            "heeler-light"
        );
        assert_eq!(
            resolve("nope", SystemAppearance::Light).name,
            "heeler-light"
        );
        assert_eq!(resolve("nope", SystemAppearance::Dark).name, "heeler-dark");
    }

    #[test]
    fn light_theme_is_light() {
        let t = lookup("heeler-light").unwrap();
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
        let json = serde_json::to_value(lookup("heeler-dark").unwrap()).unwrap();
        assert_eq!(json["background"], "#1e1e1e");
        assert_eq!(json["ansi"].as_array().unwrap().len(), 16);
        let back: TerminalTheme = serde_json::from_value(json).unwrap();
        assert_eq!(&back, lookup("heeler-dark").unwrap());
    }
}
