//! Markdown blocks as GPUI elements: paragraphs and headings as one styled text each, fenced
//! code with a copy button, lists, quotes and rules. Links open in the browser.

use gpui::{
    AnyElement, App, ClipboardItem, ElementId, Font, FontStyle, FontWeight, InteractiveElement,
    InteractiveText, IntoElement, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, StyledText, TextRun, UnderlineStyle, div, font, px,
};
use staffy_chat_view::markdown::{Block, Inline, Span, flatten};

use super::view::Regions;
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::render::probe;
use crate::workbench::style::{TerminalStyle, UiStyle};
use crate::workbench::view::Region;

/// What the chat view draws with: chrome colors, density tokens and the monospace font.
#[derive(Debug, Clone)]
pub struct Look {
    pub theme: Theme,
    pub tokens: Tokens,
    pub mono: SharedString,
}

impl Look {
    pub fn of(cx: &App) -> Self {
        let style = UiStyle::get(cx);
        Self {
            theme: style.theme,
            tokens: style.density.ui(),
            mono: TerminalStyle::get(cx).font_family.into(),
        }
    }
}

/// Hands out element ids that are unique within one entry, and records where its Copy buttons
/// are drawn.
pub struct Ids {
    scope: String,
    next: usize,
    copies: usize,
    probe: Option<(Regions, usize)>,
}

impl std::fmt::Debug for Ids {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ids").field("scope", &self.scope).finish()
    }
}

impl Ids {
    pub fn new(scope: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            next: 0,
            copies: 0,
            probe: None,
        }
    }

    /// Records the Copy buttons of entry `entry` in `regions`.
    #[must_use]
    pub fn probing(mut self, regions: Regions, entry: usize) -> Self {
        self.probe = Some((regions, entry));
        self
    }

    /// An invisible child that records the bounds of the next Copy button.
    fn copy_probe(&mut self) -> Option<AnyElement> {
        let (regions, entry) = self.probe.as_ref()?;
        let region = Region::ChatCopy(*entry, self.copies);
        self.copies += 1;
        Some(probe(regions, region).into_any_element())
    }

    pub fn next(&mut self, what: &str) -> ElementId {
        self.next += 1;
        ElementId::from(SharedString::from(format!(
            "{}-{what}-{}",
            self.scope, self.next
        )))
    }
}

fn run(font: Font, len: usize, color: gpui::Hsla) -> TextRun {
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Text runs for `styled`'s spans; `base` is the paragraph's own weight.
fn runs(spans: &[Span], look: &Look, base_bold: bool) -> Vec<TextRun> {
    let theme = &look.theme;
    spans
        .iter()
        .map(|span| {
            let style = &span.style;
            let mut face = if style.code {
                font(look.mono.clone())
            } else {
                Font::default()
            };
            if style.bold || base_bold {
                face.weight = FontWeight::BOLD;
            }
            if style.italic {
                face.style = FontStyle::Italic;
            }
            let mut text_run = run(
                face,
                span.range.len(),
                if style.link.is_some() {
                    theme.accent
                } else {
                    theme.fg
                },
            );
            if style.code {
                text_run.background_color = Some(theme.hover);
            }
            if style.link.is_some() {
                text_run.underline = Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(theme.accent),
                    wavy: false,
                });
            }
            text_run
        })
        .collect()
}

/// One paragraph or heading: the inlines as a single styled text; link ranges are clickable.
pub fn inline_text(content: &[Inline], look: &Look, bold: bool, id: ElementId) -> AnyElement {
    let styled = flatten(content);
    if styled.text.is_empty() {
        return div().into_any_element();
    }
    let text_runs = runs(&styled.spans, look, bold);
    let links: Vec<(std::ops::Range<usize>, String)> = styled
        .spans
        .iter()
        .filter_map(|span| span.style.link.clone().map(|url| (span.range.clone(), url)))
        .collect();
    let text = StyledText::new(styled.text).with_runs(text_runs);
    if links.is_empty() {
        return div().child(text).into_any_element();
    }
    let ranges = links.iter().map(|(range, _)| range.clone()).collect();
    let urls: Vec<String> = links.into_iter().map(|(_, url)| url).collect();
    InteractiveText::new(id, text)
        .on_click(ranges, move |index, _, cx| {
            if let Some(url) = urls.get(index) {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

/// A block of code (or a table, or a command's output): monospace, scrolls sideways, with a
/// Copy button.
pub fn code_block(text: &str, lang: Option<&str>, look: &Look, ids: &mut Ids) -> AnyElement {
    let theme = &look.theme;
    let tokens = &look.tokens;
    let copy_id = ids.next("copy");
    let body_id = ids.next("code");
    let copy_probe = ids.copy_probe();
    let copied = text.to_owned();
    let hover = theme.hover;
    div()
        .relative()
        .rounded(px(tokens.radius))
        .bg(theme.overlay)
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.font_size + 8.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(tokens.section_font_size + 1.0))
                .text_color(theme.muted)
                .child(SharedString::from(lang.unwrap_or("text").to_owned()))
                .child(
                    div()
                        .id(copy_id)
                        .relative()
                        .px(px(tokens.spacing))
                        .rounded(px(3.0))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover).text_color(theme.fg))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                        })
                        .child("Copy")
                        .children(copy_probe),
                ),
        )
        .child(
            div()
                .id(body_id)
                .overflow_x_scroll()
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing * 1.5))
                .font_family(look.mono.clone())
                .text_size(px(tokens.font_size))
                .text_color(theme.fg)
                .whitespace_nowrap()
                .child(SharedString::from(text.to_owned())),
        )
        .into_any_element()
}

/// Markdown blocks stacked.
pub fn blocks(items: &[Block], look: &Look, ids: &mut Ids) -> AnyElement {
    let tokens = &look.tokens;
    div()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing * 2.0))
        .children(items.iter().map(|block| block_element(block, look, ids)))
        .into_any_element()
}

fn block_element(block: &Block, look: &Look, ids: &mut Ids) -> AnyElement {
    let (theme, tokens) = (&look.theme, &look.tokens);
    match block {
        Block::Paragraph(content) => inline_text(content, look, false, ids.next("p")),
        Block::Heading { level, content } => {
            let size = match level {
                1 => tokens.font_size + 5.0,
                2 => tokens.font_size + 3.0,
                3 => tokens.font_size + 1.0,
                _ => tokens.font_size,
            };
            div()
                .mt(px(tokens.spacing))
                .text_size(px(size))
                .child(inline_text(content, look, true, ids.next("h")))
                .into_any_element()
        }
        Block::Code { lang, text } => code_block(text, lang.as_deref(), look, ids),
        Block::List {
            ordered,
            start,
            items,
        } => div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .children(items.iter().enumerate().map(|(index, item)| {
                let marker = if *ordered {
                    format!("{}.", start + index as u64)
                } else {
                    "•".to_owned()
                };
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(tokens.font_size * 1.4))
                            .text_color(theme.muted)
                            .child(SharedString::from(marker)),
                    )
                    .child(div().flex_1().min_w_0().child(blocks(item, look, ids)))
            }))
            .into_any_element(),
        Block::Quote(inner) => div()
            .pl(px(tokens.spacing * 2.5))
            .border_l_2()
            .border_color(theme.border)
            .text_color(theme.muted)
            .child(blocks(inner, look, ids))
            .into_any_element(),
        Block::Rule => div()
            .h(px(1.0))
            .w_full()
            .my(px(tokens.spacing))
            .bg(theme.border)
            .into_any_element(),
    }
}
