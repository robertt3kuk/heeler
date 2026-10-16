//! Pieces shared by the built-in panels: headers, state dots, time formatting.

use crate::theme::DensityExt as _;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, AppContext as _, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, prelude::*,
    px,
};
use hutan_core::{SessionId, SessionKind};
use hutan_protocol::messages::SessionInfo;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::sessions::{AgentBadge, PlacementBadge, Presentation, Tone};
use crate::theme::{Theme, Tokens};
use crate::workbench::actions::{OpenSettingsFor, SessionMenuAt, ShowPlacementMenu};
use crate::workbench::chat::TextInput;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::{BadgeSite, Region};

pub fn tone_color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Warning => theme.warning,
        Tone::Success => theme.success,
        Tone::Accent => theme.accent,
        Tone::Muted => theme.muted,
        Tone::Error => theme.danger,
    }
}

/// Filled dot for things that need a look, hollow ring otherwise (state is never color alone).
pub fn state_dot(theme: &Theme, presentation: &Presentation) -> AnyElement {
    let color = tone_color(theme, presentation.tone);
    let dot = div().flex_none().size(px(8.0)).rounded_full();
    if presentation.filled {
        dot.bg(color).into_any_element()
    } else {
        dot.border_1().border_color(color).into_any_element()
    }
}

/// The compact agent badge of a session: `↳2` for the subagents running, `▲` when any node is
/// blocked; its tooltip lists them. `None` while the session has nothing to show.
pub fn agent_badge(
    theme: &Theme,
    badge: &AgentBadge,
    site: BadgeSite,
    session: SessionId,
    regions: &Rc<RefCell<HashMap<Region, gpui::Bounds<gpui::Pixels>>>>,
    cx: &App,
) -> Option<AnyElement> {
    if badge.is_empty() {
        return None;
    }
    let tokens = UiStyle::get(cx).density.ui();
    let tip = SharedString::from(badge.tooltip());
    let site_name = match site {
        BadgeSite::Row => "row",
        BadgeSite::Tab => "tab",
        BadgeSite::Card => "card",
    };
    Some(
        div()
            .id(SharedString::from(format!(
                "agent-badge-{site_name}-{session}"
            )))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .text_size(px(tokens.secondary_font_size))
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .children(badge.running_text().map(|text| {
                div()
                    .text_color(theme.accent)
                    .child(SharedString::from(text))
            }))
            .children(
                badge
                    .blocked_text()
                    .map(|text| div().text_color(theme.warning).child(text)),
            )
            .child(probe(regions, Region::AgentBadge(site, session)))
            .into_any_element(),
    )
}

/// The placement badge of a session row: `main` or `wt <branch>`, framed like the other row
/// badges, with a warning border and `⚠` while another live agent works in the same checkout.
/// Its tooltip names the checkout.
pub fn placement_badge(
    theme: &Theme,
    badge: &PlacementBadge,
    site: BadgeSite,
    session: SessionId,
    regions: &Rc<RefCell<HashMap<Region, gpui::Bounds<gpui::Pixels>>>>,
    cx: &App,
) -> AnyElement {
    let tokens = UiStyle::get(cx).density.ui();
    let tip = SharedString::from(badge.tooltip.clone());
    let (color, text) = if badge.shared {
        (theme.warning, format!("⚠ {}", badge.text))
    } else {
        (theme.muted, badge.text.clone())
    };
    div()
        .id(SharedString::from(format!("placement-badge-{session}")))
        .relative()
        .flex_none()
        .max_w(px(130.0))
        .truncate()
        .px(px(tokens.spacing * 2.0))
        .rounded(px(crate::theme::PILL_RADIUS))
        .bg(color.opacity(if badge.shared { 0.14 } else { 0.0 }))
        .text_size(px(tokens.secondary_font_size))
        .text_color(color)
        .cursor_pointer()
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            window.dispatch_action(
                Box::new(ShowPlacementMenu(SessionMenuAt {
                    session: session.to_string(),
                    position: event.position(),
                })),
                cx,
            );
        })
        .child(SharedString::from(text))
        .child(probe(regions, Region::PlacementBadge(site, session)))
        .into_any_element()
}

pub fn kind_icon(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Terminal => "❯",
        SessionKind::Harness => "◆",
        SessionKind::Direct => "✦",
    }
}

/// Last path component of the session's working directory (`~` for the home directory).
pub fn placement(info: &SessionInfo) -> String {
    match &info.cwd {
        Some(cwd) => {
            if std::env::home_dir().is_some_and(|home| *cwd == home) {
                return "~".to_owned();
            }
            cwd.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cwd.display().to_string())
        }
        None => String::new(),
    }
}

/// Applies a key press to a text field: characters, ⌘V, Backspace, Delete, ←, →, Home, End and
/// (in a multi-line field) Enter. `true` when the text or the caret changed; keys the field does
/// not answer to (Tab, Escape, ⌘ shortcuts, Enter in a single line) are left alone.
pub fn edit_text(input: &mut TextInput, event: &KeyDownEvent, multiline: bool, cx: &App) -> bool {
    let ks = &event.keystroke;
    if ks.modifiers.platform {
        if ks.key == "v"
            && let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
        {
            let text = if multiline {
                text
            } else {
                text.replace(['\r', '\n'], " ")
            };
            input.insert(&text);
            return true;
        }
        return false;
    }
    if ks.modifiers.control || ks.modifiers.function {
        return false;
    }
    match ks.key.as_str() {
        "backspace" => input.backspace(),
        "delete" => input.delete(),
        "left" => input.left(),
        "right" => input.right(),
        "home" => input.home(),
        "end" => input.end(),
        "enter" if multiline => input.insert("\n"),
        _ => match &ks.key_char {
            Some(text) if !text.chars().any(char::is_control) => input.insert(text),
            _ => return false,
        },
    }
    true
}

/// A text field's box: the text with a caret while `focused`, or the dim `placeholder` while it
/// is empty. The caller adds the id's click, focus tracking and key handler.
pub fn field_box(
    id: impl Into<SharedString>,
    input: &TextInput,
    placeholder: &str,
    focused: bool,
    multiline: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    let text = input.text();
    let cursor = input.cursor().min(text.len());
    let (before, after) = text.split_at(cursor);
    let mut content = div().flex().flex_wrap().items_center().min_w_0();
    if text.is_empty() && !focused {
        content = content.child(
            div()
                .text_color(theme.faint)
                .child(SharedString::from(placeholder.to_owned())),
        );
    } else {
        content = content
            .child(SharedString::from(before.to_owned()))
            .when(focused, |s| {
                s.child(
                    div()
                        .w(px(1.0))
                        .h(px(tokens.font_size + 2.0))
                        .bg(theme.accent),
                )
            })
            .child(SharedString::from(after.to_owned()));
    }
    div()
        .id(id.into())
        .flex()
        .items_center()
        .min_h(px(tokens.sidebar_row))
        .when(multiline, |s| {
            s.min_h(px(tokens.sidebar_row * 2.5)).items_start()
        })
        .px(px(tokens.spacing * 2.0))
        .py(px(tokens.spacing / 2.0))
        .rounded(px(tokens.radius))
        .bg(theme.bg)
        .border_1()
        .border_color(if focused { theme.accent } else { theme.border })
        .text_color(theme.fg)
        .cursor_text()
        .child(content)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `now`, `3m`, `2h`, `4d` since `created_at_ms`.
pub fn age_label(created_at_ms: u64) -> String {
    let secs = now_ms().saturating_sub(created_at_ms) / 1000;
    match secs {
        0..=59 => "now".to_owned(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// `2026-10-14 12:31:07 UTC`.
pub fn format_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Panel header: sentence-case section label on the left, the ⚙ that opens the panel's settings on the
/// right.
pub fn panel_header(title: &str, namespace: &'static str, cx: &App) -> AnyElement {
    panel_header_with(title, namespace, Vec::new(), cx)
}

/// [`panel_header`] with `extra` controls between the title and the ⚙.
pub fn panel_header_with(
    title: &str,
    namespace: &'static str,
    extra: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let style = UiStyle::get(cx);
    let tokens = style.density.ui();
    let hover = style.theme.surface_hover;
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .h(px(tokens.sidebar_row + 4.0))
        .px(px(tokens.spacing * 2.5))
        .child(
            div()
                .text_size(px(tokens.section_font_size))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(style.theme.muted)
                .child(SharedString::from(title.to_owned())),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .children(extra)
                .child(
                    div()
                        .id(SharedString::from(format!("gear-{namespace}")))
                        .px(px(tokens.spacing * 1.5))
                        .rounded(px(tokens.radius))
                        .cursor_pointer()
                        .text_color(style.theme.muted)
                        .hover(move |s| s.bg(hover).text_color(style.theme.fg))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(
                                Box::new(OpenSettingsFor(namespace.to_owned())),
                                cx,
                            );
                        })
                        .child("⚙"),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formatting_matches_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        // 2026-10-14T12:31:07Z
        assert_eq!(format_utc(1_791_981_067_000), "2026-10-14 12:31:07 UTC");
        // Leap day.
        assert_eq!(format_utc(1_709_210_096_000), "2024-02-29 12:34:56 UTC");
    }
}
