//! A small confirm / prompt dialog (stop session, rename session, save layout as preset, quit
//! with live sessions).

use crate::theme::DensityExt as _;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};

use super::actions::modal as modal_actions;
use super::view::Workbench;

/// What the dialog hands its action: the prompt text and the state of the checkbox.
#[derive(Debug, Clone, Default)]
pub(crate) struct ModalResult {
    pub input: Option<String>,
    pub checked: bool,
}

pub(crate) type ModalAction =
    Rc<dyn Fn(&mut Workbench, ModalResult, &mut Window, &mut Context<Workbench>)>;

/// What a prompt shows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PromptSpec<'a> {
    pub title: &'a str,
    pub message: &'a str,
    pub initial: &'a str,
    pub confirm_label: &'a str,
}

/// A second choice next to the confirm button (for example "Stop sessions & daemon").
#[derive(Clone)]
pub struct ModalChoice {
    pub label: SharedString,
    pub(crate) run: ModalAction,
}

impl std::fmt::Debug for ModalChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModalChoice")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// A "Don't ask again" style checkbox.
#[derive(Debug, Clone)]
pub struct ModalCheckbox {
    pub label: SharedString,
    pub checked: bool,
}

/// What the dialog asks and what happens on OK. `input: Some(..)` makes it a prompt.
/// Cancel (button, Escape, click outside) closes it without running anything.
#[derive(Clone)]
pub struct Modal {
    pub title: SharedString,
    pub message: SharedString,
    pub confirm_label: SharedString,
    pub input: Option<String>,
    pub(crate) choice: Option<ModalChoice>,
    pub checkbox: Option<ModalCheckbox>,
    pub(crate) on_confirm: ModalAction,
}

impl std::fmt::Debug for Modal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Modal")
            .field("title", &self.title)
            .field("input", &self.input)
            .field("choice", &self.choice.as_ref().map(|c| &c.label))
            .field("checkbox", &self.checkbox)
            .finish_non_exhaustive()
    }
}

impl Workbench {
    pub(crate) fn open_confirm(
        &mut self,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
        on_confirm: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal(
            Modal {
                title: title.into(),
                message: message.into(),
                confirm_label: confirm_label.into(),
                input: None,
                choice: None,
                checkbox: None,
                on_confirm: Rc::new(move |this, _, window, cx| on_confirm(this, window, cx)),
            },
            window,
            cx,
        );
    }

    pub(crate) fn open_prompt(
        &mut self,
        prompt: PromptSpec<'_>,
        on_confirm: impl Fn(&mut Workbench, String, &mut Window, &mut Context<Workbench>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal(
            Modal {
                title: prompt.title.into(),
                message: prompt.message.into(),
                confirm_label: prompt.confirm_label.into(),
                input: Some(prompt.initial.to_owned()),
                choice: None,
                checkbox: None,
                on_confirm: Rc::new(move |this, result, window, cx| {
                    on_confirm(this, result.input.unwrap_or_default(), window, cx);
                }),
            },
            window,
            cx,
        );
    }

    pub(crate) fn open_modal(&mut self, modal: Modal, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.picker = None;
        self.context_menu = None;
        self.modal = Some(modal);
        window.focus(&self.modal_focus, cx);
        cx.notify();
    }

    /// The open dialog, if any.
    pub fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    pub fn modal_open(&self) -> bool {
        self.modal.is_some()
    }

    /// Replaces the prompt text.
    pub fn set_modal_input(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(Modal {
            input: Some(input), ..
        }) = &mut self.modal
        {
            *input = text.to_owned();
            cx.notify();
        }
    }

    pub fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    /// Ticks or unticks the dialog's checkbox, if it has one.
    pub fn set_modal_checkbox(&mut self, checked: bool, cx: &mut Context<Self>) {
        if let Some(Modal {
            checkbox: Some(checkbox),
            ..
        }) = &mut self.modal
        {
            checkbox.checked = checked;
            cx.notify();
        }
    }

    fn toggle_modal_checkbox(&mut self, cx: &mut Context<Self>) {
        let checked = self
            .modal
            .as_ref()
            .and_then(|m| m.checkbox.as_ref())
            .is_some_and(|c| c.checked);
        self.set_modal_checkbox(!checked, cx);
    }

    /// Closes the dialog and runs its confirm action (the primary button).
    pub fn confirm_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_modal(false, window, cx);
    }

    /// Closes the dialog and runs its second choice, if it has one.
    pub fn choose_modal_alternative(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_modal(true, window, cx);
    }

    fn finish_modal(&mut self, alternative: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        if modal
            .input
            .as_ref()
            .is_some_and(|text| text.trim().is_empty())
        {
            // Nothing to submit; keep the dialog open.
            self.modal = Some(modal);
            return;
        }
        if alternative && modal.choice.is_none() {
            self.modal = Some(modal);
            return;
        }
        let action = match &modal.choice {
            Some(choice) if alternative => Rc::clone(&choice.run),
            _ => Rc::clone(&modal.on_confirm),
        };
        let result = ModalResult {
            input: modal.input,
            checked: modal.checkbox.is_some_and(|c| c.checked),
        };
        self.focus_root(window, cx);
        action(self, result, window, cx);
        cx.notify();
    }

    pub(crate) fn render_modal(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.modal.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.hover;
        let button = |id: &'static str, label: SharedString, primary: bool| {
            div()
                .id(id)
                .px(px(tokens.spacing * 3.0))
                .h(px(tokens.sidebar_row))
                .flex()
                .items_center()
                .rounded(px(tokens.radius - 1.0))
                .border_1()
                .border_color(if primary { theme.accent } else { theme.border })
                .when(primary, |s| s.bg(theme.accent.opacity(0.25)))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(label)
        };
        Some(
            div()
                .id("modal-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(120.0))
                .bg(gpui::black().opacity(0.25))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_modal(window, cx)),
                )
                .child(
                    div()
                        .id("modal")
                        .key_context("Modal")
                        .track_focus(&self.modal_focus)
                        .w(px(440.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.0))
                        .p(px(tokens.spacing * 4.0))
                        .rounded(px(tokens.radius + 4.0))
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .text_size(px(tokens.font_size))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            let keystroke = &event.keystroke;
                            let Some(Modal {
                                input: Some(input), ..
                            }) = &mut this.modal
                            else {
                                return;
                            };
                            if keystroke.key == "backspace" {
                                input.pop();
                            } else if !keystroke.modifiers.platform
                                && !keystroke.modifiers.control
                                && !keystroke.modifiers.function
                                && let Some(text) = &keystroke.key_char
                            {
                                input.push_str(text);
                            } else {
                                return;
                            }
                            cx.notify();
                        }))
                        .on_action(cx.listener(|this, _: &modal_actions::Confirm, window, cx| {
                            this.confirm_modal(window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &modal_actions::Cancel, window, cx| {
                            this.close_modal(window, cx);
                        }))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(modal.title.clone()),
                        )
                        .child(div().text_color(theme.muted).child(modal.message.clone()))
                        .when_some(modal.input.clone(), |s, input| {
                            s.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .h(px(tokens.sidebar_row + 4.0))
                                    .px(px(tokens.spacing * 2.0))
                                    .rounded(px(tokens.radius - 1.0))
                                    .border_1()
                                    .border_color(theme.accent)
                                    .child(SharedString::from(input))
                                    .child(
                                        div()
                                            .w(px(1.0))
                                            .h(px(tokens.font_size + 2.0))
                                            .bg(theme.accent),
                                    ),
                            )
                        })
                        .when_some(modal.checkbox.clone(), |s, checkbox| {
                            s.child(
                                div()
                                    .id("modal-checkbox")
                                    .flex()
                                    .items_center()
                                    .gap(px(tokens.spacing * 2.0))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_modal_checkbox(cx);
                                    }))
                                    .child(if checkbox.checked { "☑" } else { "☐" })
                                    .child(checkbox.label),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(tokens.spacing * 2.0))
                                .child(button("modal-cancel", "Cancel".into(), false).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_modal(window, cx);
                                    }),
                                ))
                                .children(modal.choice.as_ref().map(|choice| {
                                    button("modal-choice", choice.label.clone(), false).on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.choose_modal_alternative(window, cx);
                                        }),
                                    )
                                }))
                                .child(
                                    button("modal-ok", modal.confirm_label.clone(), true).on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.confirm_modal(window, cx);
                                        }),
                                    ),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
