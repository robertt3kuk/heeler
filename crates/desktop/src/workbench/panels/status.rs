//! Status bar items: `connection`, `sessions`, `attention`, `density`, `theme`, `palette_hint`.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, AnyView, AppContext as _, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, px,
};
use heeler_client::ConnectionState;

use crate::theme::DensityExt as _;
use crate::workbench::actions::{JumpToNextAttention, ToggleDensity, TogglePalette, daemon};
use crate::workbench::registry::{StatusContext, StatusItemRegistry, StatusItemSpec, StatusSide};
use crate::workbench::style::TextTooltip;

/// Text and tone of the connection item.
pub fn connection_label(state: ConnectionState) -> (String, Tone) {
    match state {
        ConnectionState::Connecting => ("connecting to heelerd…".to_owned(), Tone::Muted),
        ConnectionState::Connected => ("heelerd connected".to_owned(), Tone::Ok),
        ConnectionState::Reconnecting { attempt, retry_in } => (
            format!("reconnecting ({attempt}) · {}", retry_text(retry_in)),
            Tone::Warn,
        ),
        ConnectionState::Stopped => ("heelerd stopped".to_owned(), Tone::Error),
    }
}

fn retry_text(retry_in: Duration) -> String {
    let secs = retry_in.as_secs_f32();
    if secs < 1.0 {
        format!("retry in {} ms", retry_in.as_millis())
    } else {
        format!("retry in {secs:.0} s")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Error,
    Muted,
}

fn clickable(id: &'static str, ctx: &StatusContext<'_>) -> gpui::Stateful<gpui::Div> {
    let hover = ctx.theme.hover;
    div()
        .id(id)
        .px(px(4.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
}

fn tooltip(text: String) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> AnyView + 'static {
    move |_, cx| {
        cx.new(|_| TextTooltip(SharedString::from(text.clone())))
            .into()
    }
}

/// The six built-in status items.
pub fn builtin_status_items() -> StatusItemRegistry {
    let mut registry = StatusItemRegistry::new();
    registry.register(StatusItemSpec {
        id: "connection",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let (text, tone) = ctx.data.connection_line.clone();
            let color = match tone {
                Tone::Ok => ctx.theme.success,
                Tone::Warn => ctx.theme.warning,
                Tone::Error => ctx.theme.error,
                Tone::Muted => ctx.theme.muted,
            };
            let epoch = ctx.data.epoch.clone().map_or_else(
                || "no daemon epoch yet".to_owned(),
                |e| format!("daemon epoch {e}"),
            );
            clickable("status-connection", ctx)
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_color(ctx.theme.muted)
                .tooltip(tooltip(format!("{epoch} · click for the daemon controls")))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(daemon::Status), cx))
                .child(div().size(px(7.0)).rounded_full().bg(color))
                .child(SharedString::from(text))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "sessions",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let n = ctx.data.sessions;
            div()
                .id("status-sessions")
                .text_color(ctx.theme.muted)
                .child(format!("{n} session{}", if n == 1 { "" } else { "s" }))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "attention",
        default_side: StatusSide::Left,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            let n = ctx.data.needs_you;
            if n == 0 {
                return div()
                    .id("status-attention")
                    .text_color(ctx.theme.muted)
                    .child("nothing needs you")
                    .into_any_element();
            }
            clickable("status-attention", ctx)
                .text_color(ctx.theme.warning)
                .tooltip(tooltip(format!(
                    "Jump to the next session that needs you {}",
                    ctx.attention_hint
                )))
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(JumpToNextAttention), cx);
                })
                .child(format!("● {n} need{} you", if n == 1 { "s" } else { "" }))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "density",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            clickable("status-density", ctx)
                .text_color(ctx.theme.muted)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleDensity), cx))
                .child(ctx.density.label())
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "theme",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| {
            clickable("status-theme", ctx)
                .text_color(ctx.theme.muted)
                .tooltip(tooltip("Terminal theme (change in settings)".to_owned()))
                .on_click(|_, window, cx| {
                    window.dispatch_action(
                        Box::new(crate::workbench::actions::OpenSettingsFor(
                            "terminal".to_owned(),
                        )),
                        cx,
                    );
                })
                .child(SharedString::from(ctx.data.terminal_theme.clone()))
                .into_any_element()
        }),
    });
    registry.register(StatusItemSpec {
        id: "palette_hint",
        default_side: StatusSide::Right,
        render: Rc::new(|ctx: &StatusContext<'_>| -> AnyElement {
            clickable("status-palette-hint", ctx)
                .text_color(ctx.theme.muted)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(TogglePalette), cx))
                .child(ctx.palette_hint.clone())
                .into_any_element()
        }),
    });
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_states_have_distinct_labels() {
        assert_eq!(connection_label(ConnectionState::Connected).1, Tone::Ok);
        let (text, tone) = connection_label(ConnectionState::Reconnecting {
            attempt: 3,
            retry_in: Duration::from_millis(400),
        });
        assert_eq!(tone, Tone::Warn);
        assert!(text.contains("reconnecting (3)"), "{text}");
        assert_eq!(connection_label(ConnectionState::Stopped).1, Tone::Error);
    }
}
