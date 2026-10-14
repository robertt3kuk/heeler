//! `UiStyle`: theme + density as a GPUI global so panels render without a back-pointer to the
//! workbench. The workbench is the only writer.

use crate::theme::{Density, DensityExt as _, Theme};
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

/// The terminal view configuration currently in effect (`terminal` settings with the theme
/// resolved); the workbench writes it, overview previews read it.
#[derive(Clone, Debug)]
pub struct TerminalStyle(pub heeler_terminal_view::TerminalViewSettings);

impl Global for TerminalStyle {}

impl TerminalStyle {
    pub fn get(cx: &App) -> heeler_terminal_view::TerminalViewSettings {
        cx.try_global::<TerminalStyle>()
            .map(|s| s.0.clone())
            .unwrap_or_default()
    }
}

/// A one-line tooltip.
#[derive(Debug)]
pub struct TextTooltip(pub gpui::SharedString);

impl gpui::Render for TextTooltip {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _, div, px};
        let style = UiStyle::get(cx);
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(style.density.ui().radius - 2.0))
            .bg(style.theme.overlay)
            .border_1()
            .border_color(style.theme.border)
            .text_color(style.theme.fg)
            .text_size(px(style.density.ui().font_size - 1.0))
            .child(self.0.clone())
    }
}
