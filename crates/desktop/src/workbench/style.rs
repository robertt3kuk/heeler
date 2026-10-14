//! `UiStyle`: theme + density as a GPUI global so panels render without a back-pointer to the
//! workbench. The workbench is the only writer.

use crate::theme::{Density, Theme};
use gpui::{App, Global};

#[derive(Clone, Copy, Debug)]
pub struct UiStyle {
    pub theme: Theme,
    pub density: Density,
}

impl Global for UiStyle {}

impl UiStyle {
    /// Current style; falls back to dark/compact before the workbench installs one.
    pub fn get(cx: &App) -> UiStyle {
        cx.try_global::<UiStyle>().copied().unwrap_or(UiStyle {
            theme: Theme::dark(),
            density: Density::default(),
        })
    }
}
