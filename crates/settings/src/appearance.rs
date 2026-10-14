//! `[appearance]`: density and chrome mode, plus the density token table from `docs/design.md`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Setting;

/// UI density. Compact is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Compact,
    Comfortable,
}

/// Whether the window chrome follows macOS or is pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChromeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// The `appearance` namespace.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AppearanceSettings {
    /// `compact` (default) or `comfortable`; see the density token table in `docs/design.md`.
    pub density: Density,
    /// `system` (follow macOS, default), `light` or `dark`.
    pub chrome: ChromeMode,
}

impl Setting for AppearanceSettings {
    const NAMESPACE: &'static str = "appearance";
}

/// Sizes in logical pixels that change with [`Density`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DensityTokens {
    pub sidebar_row: f32,
    pub tab_height: f32,
    pub pane_header: f32,
    pub status_bar: f32,
    /// Base spacing unit.
    pub spacing: f32,
    /// Corner radius.
    pub radius: f32,
}

impl Density {
    /// The design-system token table (`docs/design.md` § Visual system).
    pub const fn tokens(self) -> DensityTokens {
        match self {
            Self::Compact => DensityTokens {
                sidebar_row: 24.0,
                tab_height: 28.0,
                pane_header: 28.0,
                status_bar: 22.0,
                spacing: 4.0,
                radius: 6.0,
            },
            Self::Comfortable => DensityTokens {
                sidebar_row: 30.0,
                tab_height: 32.0,
                pane_header: 34.0,
                status_bar: 26.0,
                spacing: 6.0,
                radius: 8.0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_compact_and_system() {
        let a = AppearanceSettings::default();
        assert_eq!(a.density, Density::Compact);
        assert_eq!(a.chrome, ChromeMode::System);
    }

    #[test]
    fn density_tokens_match_design_table() {
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
        let c = Density::Comfortable.tokens();
        assert_eq!(
            (
                c.sidebar_row,
                c.tab_height,
                c.pane_header,
                c.status_bar,
                c.spacing,
                c.radius
            ),
            (30.0, 32.0, 34.0, 26.0, 6.0, 8.0)
        );
    }

    #[test]
    fn deserializes_lowercase_names_with_defaults() {
        let a: AppearanceSettings =
            toml::from_str("density = \"comfortable\"\nchrome = \"dark\"\n").unwrap();
        assert_eq!(a.density, Density::Comfortable);
        assert_eq!(a.chrome, ChromeMode::Dark);
        let a: AppearanceSettings = toml::from_str("chrome = \"light\"\n").unwrap();
        assert_eq!(a.density, Density::Compact);
        assert!(toml::from_str::<AppearanceSettings>("density = \"roomy\"\n").is_err());
    }
}
