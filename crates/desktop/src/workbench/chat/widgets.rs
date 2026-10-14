//! Small pieces the chat view shares: buttons, chips and number formatting.

use gpui::{
    Hsla, InteractiveElement, MouseButton, ParentElement, SharedString, Styled, div, prelude::*, px,
};

use super::markdown_view::Look;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// The main action: filled with the accent.
    Primary,
    /// Allows something: outlined in the success color.
    Allow,
    /// Refuses something: outlined in the error color.
    Deny,
    Plain,
}

/// A text button. A disabled one is drawn dim and ignores the mouse (the caller adds the click
/// handler only when enabled).
pub fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    look: &Look,
) -> gpui::Stateful<gpui::Div> {
    let (theme, tokens) = (&look.theme, &look.tokens);
    let (border, fill, text): (Hsla, Option<Hsla>, Hsla) = match kind {
        ButtonKind::Primary => (theme.accent, Some(theme.accent.opacity(0.28)), theme.fg),
        ButtonKind::Allow => (theme.success, Some(theme.success.opacity(0.14)), theme.fg),
        ButtonKind::Deny => (theme.error, Some(theme.error.opacity(0.10)), theme.fg),
        ButtonKind::Plain => (theme.border, None, theme.fg),
    };
    let hover = theme.hover;
    let mut button = div()
        .id(id.into())
        .flex_none()
        .px(px(tokens.spacing * 2.5))
        .h(px(tokens.sidebar_row))
        .flex()
        .items_center()
        .gap(px(tokens.spacing))
        .rounded(px(tokens.radius - 1.0))
        .border_1()
        .border_color(border)
        .text_size(px(tokens.font_size))
        .when_some(fill, |s, fill| s.bg(fill))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label.into());
    if enabled {
        button = button
            .text_color(text)
            .cursor_pointer()
            .hover(move |s| s.bg(hover));
    } else {
        button = button.text_color(theme.muted).opacity(0.55);
    }
    button
}

/// A small label in a tone's color ("exit 0", "running").
pub fn pill(text: impl Into<SharedString>, color: Hsla, look: &Look) -> gpui::Div {
    let tokens = &look.tokens;
    div()
        .flex_none()
        .px(px(tokens.spacing * 1.5))
        .rounded(px(tokens.radius))
        .border_1()
        .border_color(color.opacity(0.5))
        .text_color(color)
        .text_size(px(tokens.section_font_size + 1.0))
        .child(text.into())
}

/// `12 ms`, `1.2 s`, `2 m 3 s`.
pub fn duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms} ms"),
        1000..=59_999 => format!("{:.1} s", ms as f64 / 1000.0),
        _ => format!("{} m {} s", ms / 60_000, (ms % 60_000) / 1000),
    }
}

/// `842`, `22.2k`, `1.4M`.
pub fn tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1000..=999_999 => format!("{:.1}k", count as f64 / 1000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// The last `max` lines of `text` and how many came before them.
pub fn tail_lines(text: &str, max: usize) -> (&str, usize) {
    let total = text.lines().count();
    if total <= max {
        return (text, 0);
    }
    let skip = total - max;
    let mut at = 0;
    for _ in 0..skip {
        at += text[at..].find('\n').map_or(text.len() - at, |i| i + 1);
    }
    (&text[at..], skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_counts_read_naturally() {
        assert_eq!(duration(12), "12 ms");
        assert_eq!(duration(1234), "1.2 s");
        assert_eq!(duration(125_000), "2 m 5 s");
        assert_eq!(tokens(842), "842");
        assert_eq!(tokens(22_178), "22.2k");
        assert_eq!(tokens(1_400_000), "1.4M");
    }

    #[test]
    fn tail_lines_keeps_the_end() {
        let text = "1\n2\n3\n4\n5\n";
        assert_eq!(tail_lines(text, 2), ("4\n5\n", 3));
        assert_eq!(tail_lines(text, 9), (text, 0));
        assert_eq!(tail_lines("", 3), ("", 0));
        assert_eq!(tail_lines("a\nb", 1), ("b", 1));
    }
}
