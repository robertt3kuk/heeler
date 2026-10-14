//! Built-in placeholder panels and status items for this slice. Wave 2 replaces the panels with
//! real implementations registered under the same ids.

use super::actions::{ToggleDensity, TogglePalette};
use super::model::DockPosition;
use super::registry::{
    PanelRegistry, PanelSpec, StatusContext, StatusItemRegistry, StatusItemSpec, StatusSide,
};
use super::style::UiStyle;
use gpui::{
    AnyView, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use std::rc::Rc;

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
        let tokens = style.density.tokens();
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

fn placeholder_spec(
    id: &str,
    title: &'static str,
    icon: &'static str,
    default_dock: DockPosition,
    description: &'static str,
) -> PanelSpec {
    PanelSpec {
        id: id.into(),
        title: title.into(),
        icon: icon.into(),
        default_dock,
        factory: Rc::new(move |_window, cx| {
            AnyView::from(cx.new(|_| PlaceholderPanel::new(title, description)))
        }),
    }
}

/// `sessions`, `inspector`, `overview`, `notifications`.
pub fn builtin_panels() -> PanelRegistry {
    let mut registry = PanelRegistry::new();
    registry.register(placeholder_spec(
        "sessions",
        "Sessions",
        "≣",
        DockPosition::Left,
        "Placeholder: workspaces and agent sessions, grouped by what needs you. Arrives with M2.5.",
    ));
    registry.register(placeholder_spec(
        "inspector",
        "Inspector",
        "ⓘ",
        DockPosition::Right,
        "Placeholder: approvals, changes, and session info for the focused session.",
    ));
    registry.register(placeholder_spec(
        "overview",
        "Overview",
        "⊞",
        DockPosition::Left,
        "Placeholder: board of live session cards sorted by attention.",
    ));
    registry.register(placeholder_spec(
        "notifications",
        "Notifications",
        "◔",
        DockPosition::Bottom,
        "Placeholder: toasts and unread attention events collect here.",
    ));
    registry
}

/// `connection` ("—"), `density`, `palette_hint`.
pub fn builtin_status_items() -> StatusItemRegistry {
    let mut registry = StatusItemRegistry::new();
    registry.register(StatusItemSpec {
        id: "connection",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            div()
                .id("status-connection")
                .text_color(ctx.theme.muted)
                .child("—")
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "density",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let hover = ctx.theme.hover;
            div()
                .id("status-density")
                .text_color(ctx.theme.muted)
                .cursor_pointer()
                .px(px(4.0))
                .rounded(px(3.0))
                .hover(move |s| s.bg(hover))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleDensity), cx))
                .child(ctx.density.label())
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "palette_hint",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let hover = ctx.theme.hover;
            div()
                .id("status-palette-hint")
                .text_color(ctx.theme.muted)
                .cursor_pointer()
                .px(px(4.0))
                .rounded(px(3.0))
                .hover(move |s| s.bg(hover))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(TogglePalette), cx))
                .child(ctx.palette_hint.clone())
                .into_any_element()
        }),
    });
    registry
}
