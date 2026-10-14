//! Built-in panels and status items. Each panel is registered under its id with a settings
//! namespace `panels.<id>`; the layout decides where it lives.

mod common;
mod history;
mod history_model;
mod inspector;
mod overview;
mod sessions;
mod status;

use super::model::DockPosition;
use super::registry::{PanelRegistry, PanelSpec};
use super::style::UiStyle;
use crate::theme::DensityExt as _;
use gpui::{
    AnyView, AppContext as _, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use std::rc::Rc;

pub use common::{age_label, kind_icon, tone_color};
pub use history::{Confirm as HistoryConfirm, HistoryPanel};
pub use history_model::{
    DEBOUNCE as HISTORY_DEBOUNCE, Filters as HistoryFilters, HeelerTarget, HistoryResume,
    PICKER_LIMIT as HISTORY_PICKER_LIMIT, Snippet, hit_title, parse_snippet, shell_join,
    short_path, source_icon, source_label,
};
pub use inspector::{BlockRow, InspectorPanel};
pub use overview::OverviewPanel;
pub use sessions::{DraggedWorkspace, SessionsPanel};
pub use status::{Tone as StatusTone, builtin_status_items, connection_label};

/// A panel that renders its name and a short description.
#[derive(Debug)]
pub struct PlaceholderPanel {
    title: SharedString,
    description: SharedString,
}

impl PlaceholderPanel {
    pub fn new(title: impl Into<SharedString>, description: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
        }
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let tokens = style.density.ui();
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .p(px(tokens.spacing * 3.0))
            .text_size(px(tokens.font_size))
            .text_color(style.theme.fg)
            .child(
                div()
                    .text_size(px(tokens.section_font_size))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(style.theme.muted)
                    .child(self.title.to_uppercase()),
            )
            .child(
                div()
                    .text_color(style.theme.muted)
                    .child(self.description.clone()),
            )
    }
}

/// `sessions`, `history`, `inspector`, `overview`, `notifications`.
pub fn builtin_panels() -> PanelRegistry {
    let mut registry = PanelRegistry::new();
    registry.register(PanelSpec {
        id: "sessions".into(),
        title: "Sessions".into(),
        icon: "≣".into(),
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| SessionsPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "history".into(),
        title: "History".into(),
        icon: "⌕".into(),
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| HistoryPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "inspector".into(),
        title: "Inspector".into(),
        icon: "ⓘ".into(),
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| InspectorPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "overview".into(),
        title: "Overview".into(),
        icon: "⊞".into(),
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| OverviewPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "notifications".into(),
        title: "Notifications".into(),
        icon: "◔".into(),
        default_dock: DockPosition::Bottom,
        factory: Rc::new(|_env, _window, cx| {
            AnyView::from(cx.new(|_| {
                PlaceholderPanel::new(
                    "Notifications",
                    "Toasts and unread attention events collect here (arrives with M6).",
                )
            }))
        }),
    });
    registry
}
