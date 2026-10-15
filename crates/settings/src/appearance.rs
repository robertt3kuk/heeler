//! `[appearance]`: density and the light/dark mode, plus the density token table from `docs/design.md`.

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

/// The `appearance` setting: whether the window follows macOS light/dark or is pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceMode {
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
    /// `system` (follow macOS light/dark live, default), `light` or `dark`. Files written before
    /// v2 call this key `chrome`.
    #[serde(alias = "chrome")]
    pub appearance: AppearanceMode,
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
                sidebar_row: 26.0,
                tab_height: 30.0,
                pane_header: 30.0,
                status_bar: 22.0,
                spacing: 4.0,
                radius: 6.0,
            },
            Self::Comfortable => DensityTokens {
                sidebar_row: 30.0,
                tab_height: 34.0,
                pane_header: 36.0,
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
        assert_eq!(a.appearance, AppearanceMode::System);
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
            (26.0, 30.0, 30.0, 22.0, 4.0, 6.0)
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
            (30.0, 34.0, 36.0, 26.0, 6.0, 8.0)
        );
    }

    #[test]
    fn deserializes_lowercase_names_with_defaults() {
        let a: AppearanceSettings =
            toml::from_str("density = \"comfortable\"\nappearance = \"dark\"\n").unwrap();
        assert_eq!(a.density, Density::Comfortable);
        assert_eq!(a.appearance, AppearanceMode::Dark);
        let a: AppearanceSettings = toml::from_str("appearance = \"light\"\n").unwrap();
        assert_eq!(a.density, Density::Compact);
        assert!(toml::from_str::<AppearanceSettings>("density = \"roomy\"\n").is_err());
    }
}

#[cfg(test)]
mod alias_tests {
    use super::*;

    #[test]
    fn the_pre_v2_chrome_key_still_reads() {
        let a: AppearanceSettings = toml::from_str("chrome = \"dark\"\n").unwrap();
        assert_eq!(a.appearance, AppearanceMode::Dark);
    }
}
