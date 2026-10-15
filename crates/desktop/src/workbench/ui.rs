//! The component kit of the visual system (design.md § Visual system, v2): buttons, pills,
//! popover and card surfaces, prose text, section labels and the mascot empty state. Views call
//! these instead of spelling out colors, radii and fonts, so the look stays in one place.

use std::sync::{Arc, LazyLock};

use gpui::{
    AnyElement, FontFallbacks, FontWeight, Hsla, Image, ImageFormat, InteractiveElement,
    MouseButton, ObjectFit, ParentElement, SharedString, Stateful, Styled, StyledImage, div, img,
    prelude::FluentBuilder as _, px, relative,
};

use crate::theme::{MASCOT_SIZE, PILL_RADIUS, Theme, Tokens};

/// Prose font: the system serif "New York" (macOS 10.15+), Georgia where it is missing.
pub const PROSE_FONT: &str = "New York";
pub const PROSE_FALLBACK: &str = "Georgia";

/// A GPUI `Font` for prose text with the Georgia fallback.
pub fn prose_font() -> gpui::Font {
    let mut face = gpui::font(PROSE_FONT);
    face.fallbacks = Some(FontFallbacks::from_fonts(vec![PROSE_FALLBACK.to_owned()]));
    face
}

/// Style helpers on any GPUI element.
pub trait StyledExt: Styled + Sized {
    /// Prose type: serif, 15 px, line height 1.6.
    fn prose(mut self, tokens: &Tokens) -> Self {
        let text = self.text_style();
        text.font_family = Some(PROSE_FONT.into());
        text.font_fallbacks = Some(FontFallbacks::from_fonts(vec![PROSE_FALLBACK.to_owned()]));
        self.text_size(px(tokens.prose_font_size))
            .line_height(relative(tokens.prose_line_height))
    }

    /// Medium weight: the heaviest weight the UI uses besides markdown strong text.
    fn medium(self) -> Self {
        self.font_weight(FontWeight::MEDIUM)
    }

    /// A card: `surface`, hairline border, card radius. No shadow.
    fn card(self, theme: &Theme, tokens: &Tokens) -> Self {
        self.bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(tokens.card_radius))
    }

    /// A floating surface (popover, menu, palette, modal): a card with the one soft shadow and
    /// 6 px inner padding.
    fn popover(mut self, theme: &Theme, tokens: &Tokens) -> Self {
        self.style().box_shadow = Some(theme.shadow());
        self.card(theme, tokens).p(px(6.0))
    }

    /// The shared geometry of a row in a list or menu; the caller adds the hover and selected
    /// fills (`surface_hover`, `accent_soft`).
    fn row(self, height: f32, tokens: &Tokens) -> Self {
        self.flex()
            .items_center()
            .h(px(height))
            .px(px(tokens.spacing + 2.0))
            .rounded(px(tokens.radius))
    }
}

impl<T: Styled + Sized> StyledExt for T {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// `accent` fill, white text.
    Primary,
    /// `surface` with a `border`.
    Secondary,
    /// Text only, hover fill.
    Ghost,
}

/// A 28 px text button with an 8 px radius. A disabled one is drawn dim and ignores the mouse
/// (the caller adds the click handler only when enabled).
pub fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> Stateful<gpui::Div> {
    let hover = match kind {
        ButtonKind::Primary => theme.accent.opacity(0.88),
        ButtonKind::Secondary | ButtonKind::Ghost => theme.surface_hover,
    };
    let mut button = div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(tokens.spacing))
        .h(px(tokens.control_height))
        .px(px(12.0))
        .rounded(px(tokens.radius))
        .text_size(px(tokens.font_size))
        .medium()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label.into());
    button = match kind {
        ButtonKind::Primary => button.bg(theme.accent).text_color(theme.on_accent()),
        ButtonKind::Secondary => button
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .text_color(theme.fg),
        ButtonKind::Ghost => button.text_color(theme.fg),
    };
    if enabled {
        button.cursor_pointer().hover(move |s| s.bg(hover))
    } else {
        button.text_color(theme.faint).opacity(0.7)
    }
}

/// A round icon button (the composer's send / stop and attach): `primary` fills with `accent`,
/// otherwise a hairline-bordered `surface` circle. A disabled one is dim and inert.
pub fn round_button(
    id: impl Into<gpui::ElementId>,
    glyph: impl Into<SharedString>,
    primary: bool,
    enabled: bool,
    size: f32,
    theme: &Theme,
) -> Stateful<gpui::Div> {
    let hover = if primary {
        theme.accent.opacity(0.88)
    } else {
        theme.surface_hover
    };
    let base = div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(PILL_RADIUS))
        .medium()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(glyph.into());
    let base = if primary {
        base.bg(theme.accent).text_color(theme.on_accent())
    } else {
        base.border_1()
            .border_color(theme.border)
            .text_color(theme.muted)
    };
    if enabled {
        base.cursor_pointer().hover(move |s| s.bg(hover))
    } else {
        base.opacity(0.45)
    }
}

/// A small label in a tone's color on a faint tint of it ("exit 0", "running"). Fully rounded.
pub fn pill(text: impl Into<SharedString>, color: Hsla, tokens: &Tokens) -> gpui::Div {
    div()
        .flex_none()
        .px(px(tokens.spacing * 2.0))
        .rounded(px(PILL_RADIUS))
        .bg(color.opacity(0.12))
        .text_color(color)
        .text_size(px(tokens.secondary_font_size))
        .child(text.into())
}

/// A counter pill: tabular figures are the platform default for SF digits in this size.
pub fn counter(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .flex_none()
        .px(px(tokens.spacing + 2.0))
        .rounded(px(PILL_RADIUS))
        .bg(theme.surface_hover)
        .text_color(theme.muted)
        .text_size(px(tokens.secondary_font_size))
        .child(text.into())
}

/// A section label: 11 px medium, `muted`, sentence case.
pub fn section_label(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .text_size(px(tokens.section_font_size))
        .medium()
        .text_color(theme.muted)
        .child(text.into())
}

/// Which mascot image an empty state shows (`assets/brand/05-mascot/png`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mascot {
    /// Nothing here yet.
    Empty,
    /// Loading, or an agent is working.
    Thinking,
    /// All clear, finished.
    Success,
    /// Something failed.
    Error,
    /// The daemon is not running: idle.
    Sleep,
}

impl Mascot {
    pub const ALL: [Mascot; 5] = [
        Mascot::Empty,
        Mascot::Thinking,
        Mascot::Success,
        Mascot::Error,
        Mascot::Sleep,
    ];

    /// The file name under `assets/brand/05-mascot/png`.
    pub fn file(self) -> &'static str {
        match self {
            Mascot::Empty => "state-empty@2x.png",
            Mascot::Thinking => "state-thinking@2x.png",
            Mascot::Success => "state-success@2x.png",
            Mascot::Error => "state-error@2x.png",
            Mascot::Sleep => "state-sleep@2x.png",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Mascot::Empty => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-empty@2x.png")
            }
            Mascot::Thinking => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-thinking@2x.png")
            }
            Mascot::Success => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-success@2x.png")
            }
            Mascot::Error => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-error@2x.png")
            }
            Mascot::Sleep => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-sleep@2x.png")
            }
        }
    }

    fn image(self) -> Arc<Image> {
        static IMAGES: LazyLock<[Arc<Image>; 5]> = LazyLock::new(|| {
            Mascot::ALL.map(|m| Arc::new(Image::from_bytes(ImageFormat::Png, m.bytes().to_vec())))
        });
        let index = Mascot::ALL.iter().position(|m| *m == self).unwrap_or(0);
        IMAGES[index].clone()
    }
}

/// The mascot image at `size` px.
pub fn mascot(state: Mascot, size: f32) -> gpui::Img {
    img(state.image())
        .size(px(size))
        .flex_none()
        .object_fit(ObjectFit::Contain)
}

/// An empty state: the mascot at 96 px, one serif sentence, optionally one action button.
pub fn empty_state(
    state: Mascot,
    text: impl Into<SharedString>,
    action: Option<AnyElement>,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(tokens.spacing * 3.0))
        .p(px(tokens.spacing * 4.0))
        .child(mascot(state, MASCOT_SIZE))
        .child(
            div()
                .max_w(px(420.0))
                .text_center()
                .text_color(theme.muted)
                .prose(tokens)
                .child(text.into()),
        )
        .when_some(action, |s, action| s.child(action))
}
