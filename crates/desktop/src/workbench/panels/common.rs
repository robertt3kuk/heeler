//! Pieces shared by the built-in panels: headers, state dots, time formatting.

use crate::theme::DensityExt as _;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, AppContext as _, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};
use hutan_core::{SessionId, SessionKind};
use hutan_protocol::messages::SessionInfo;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::sessions::{AgentBadge, Presentation, Tone};
use crate::theme::Theme;
use crate::workbench::actions::OpenSettingsFor;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::{BadgeSite, Region};

pub fn tone_color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Warning => theme.warning,
        Tone::Success => theme.success,
        Tone::Accent => theme.accent,
        Tone::Muted => theme.muted,
        Tone::Error => theme.error,
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
            .text_size(px(tokens.section_font_size))
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

/// Panel header: uppercase title on the left, the ⚙ that opens the panel's settings on the
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
    let hover = style.theme.hover;
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .h(px(tokens.sidebar_row))
        .px(px(tokens.spacing * 2.0))
        .child(
            div()
                .text_size(px(tokens.section_font_size))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(style.theme.muted)
                .child(SharedString::from(title.to_uppercase())),
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
                        .px(px(tokens.spacing))
                        .rounded(px(3.0))
                        .cursor_pointer()
                        .text_color(style.theme.muted)
                        .hover(move |s| s.bg(hover))
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
