//! [`ChatView`]: the chat tab. A header (who, in what state), the virtualized transcript, an
//! optional banner, and the composer. Cards are drawn by `cards.rs`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, Focusable, FollowMode,
    InteractiveElement, IntoElement, KeyDownEvent, ListAlignment, ListState, MouseButton,
    ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled, StyledText,
    Subscription, TextRun, Window, div, font, list, prelude::*, px,
};
use hutan_chat_view::markdown::Block;
use hutan_client::ConnectionState;
use hutan_core::{ApprovalId, SessionId, SessionState};
use hutan_protocol::messages::SessionInfo;
use hutan_settings::SettingsStore;

use super::attach::Attaching;
use super::context::is_gateway_chat;
use super::input::TextInput;
use super::markdown_view::Look;
use super::model::{ChatModel, LoadStatus};
use super::widgets::{ButtonKind, button};
use crate::sessions::SessionsStore;
use crate::settings::ChatSettings;
use crate::workbench::actions::{self, RestartSessionById};
use crate::workbench::panels::{kind_icon, state_dot, tone_color};
use crate::workbench::render::probe;
use crate::workbench::view::Region;

pub(crate) type Regions = Rc<RefCell<HashMap<Region, Bounds<Pixels>>>>;

/// Key context of the chat view's root element, for keymap bindings.
pub const KEY_CONTEXT: &str = "HutanChat";

/// Tallest the composer's text grows before it scrolls.
const COMPOSER_MAX_HEIGHT: f32 = 220.0;
/// The caret drawn inside the composer's text.
const CARET: &str = "▏";

pub struct ChatView {
    pub(super) session: SessionId,
    pub(super) model: Entity<ChatModel>,
    pub(super) sessions: Entity<SessionsStore>,
    settings: SettingsStore,
    pub(super) regions: Regions,
    focus_handle: FocusHandle,
    pub(super) list: ListState,
    seen_epoch: u64,
    synced_len: usize,
    synced_rev: u64,
    /// Things the user opened or closed, against each card's default: `(entry id, part)`.
    pub(super) toggled: HashSet<(u64, u32)>,
    /// Parsed markdown by entry id, with the entry revision it was parsed at.
    pub(super) parsed: HashMap<u64, (u64, Rc<Vec<Block>>)>,
    /// Requests waiting for an answer, oldest first (the keys 1–4 answer the first).
    pub(super) pending: Vec<ApprovalId>,
    /// The Attach menu or form of an assistant chat.
    pub(super) attach: Option<Attaching>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for ChatView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatView")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Whether the composer may be used, and why not.
#[derive(Debug, Clone, Default)]
struct Gate {
    /// A turn is in progress (the interrupt button is offered, sending is not).
    running: bool,
    /// Why nothing can be sent now.
    blocked: Option<SharedString>,
    /// The session ended: it can be restarted.
    restartable: bool,
}

impl ChatView {
    pub fn new(
        session: SessionId,
        model: Entity<ChatModel>,
        sessions: Entity<SessionsStore>,
        settings: SettingsStore,
        regions: Regions,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(800.0));
        list.set_follow_mode(FollowMode::Tail);
        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
        ];
        Self {
            session,
            model,
            sessions,
            settings,
            regions,
            focus_handle: cx.focus_handle(),
            list,
            seen_epoch: 0,
            synced_len: 0,
            synced_rev: 0,
            toggled: HashSet::new(),
            parsed: HashMap::new(),
            pending: Vec::new(),
            attach: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session
    }

    /// Whether the composer has the keyboard focus.
    pub fn focus_handle_is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    pub fn model(&self) -> &Entity<ChatModel> {
        &self.model
    }

    /// Brings the list's item heights in line with the transcript: items whose entry changed are
    /// measured again, new ones are added; a transcript that was replaced starts a new list.
    fn sync_list(&mut self, cx: &App) {
        let transcript = self.model.read(cx).transcript();
        let len = transcript.len();
        if transcript.epoch() != self.seen_epoch || len < self.synced_len {
            self.list.reset(len);
            self.parsed.clear();
            self.seen_epoch = transcript.epoch();
        } else {
            for (at, entry) in transcript
                .entries()
                .iter()
                .enumerate()
                .take(self.synced_len)
            {
                if entry.rev > self.synced_rev {
                    self.list.splice(at..at + 1, 1);
                }
            }
            if len > self.synced_len {
                self.list
                    .splice(self.synced_len..self.synced_len, len - self.synced_len);
            }
        }
        self.synced_len = len;
        self.synced_rev = transcript.rev();
    }

    /// Flips a part of an entry (a diff, an output, reasoning) against its default.
    pub(super) fn toggle(&mut self, index: usize, entry: u64, part: u32, cx: &mut Context<Self>) {
        if !self.toggled.remove(&(entry, part)) {
            self.toggled.insert((entry, part));
        }
        // The card grows (or shrinks) where it is; following the tail would scroll it away from
        // under the pointer. Scrolling back to the end follows again.
        self.list.pause_following_tail();
        self.list.splice(index..index + 1, 1);
        cx.notify();
    }

    /// Whether `part` of `entry` is open: `default`, unless the user flipped it.
    pub(super) fn open(&self, entry: u64, part: u32, default: bool) -> bool {
        default != self.toggled.contains(&(entry, part))
    }

    // ---- state -----------------------------------------------------------------------------

    fn info(&self, cx: &App) -> Option<SessionInfo> {
        self.sessions.read(cx).session(self.session).cloned()
    }

    fn gate(&self, info: Option<&SessionInfo>, cx: &App) -> Gate {
        let store = self.sessions.read(cx);
        let transcript = self.model.read(cx).transcript();
        let Some(info) = info else {
            return Gate {
                blocked: Some("This session no longer exists. Close the tab.".into()),
                ..Gate::default()
            };
        };
        let mut gate = Gate::default();
        match &info.state {
            SessionState::Starting => gate.blocked = Some("The agent is starting…".into()),
            SessionState::Running | SessionState::Idle | SessionState::WaitingApproval => {
                gate.running = info
                    .chat
                    .as_ref()
                    .map_or(transcript.turn_active(), |chat| chat.turn_active);
            }
            SessionState::Exited { .. } | SessionState::Failed { .. } | SessionState::Stopped => {
                gate.blocked = Some("The session ended.".into());
                gate.restartable = true;
            }
        }
        if gate.blocked.is_none() && store.connection_state() != ConnectionState::Connected {
            gate.blocked = Some("Reconnecting to hutand…".into());
        }
        gate
    }

    /// Why nothing can be sent right now (the session ended, it is starting, the connection is
    /// down); `None` when the composer works.
    pub fn blocked_reason(&self, cx: &App) -> Option<String> {
        let info = self.info(cx);
        self.gate(info.as_ref(), cx).blocked.map(|r| r.to_string())
    }

    /// Whether the composer's Send would do something now.
    pub fn can_send(&self, cx: &App) -> bool {
        let info = self.info(cx);
        let gate = self.gate(info.as_ref(), cx);
        let model = self.model.read(cx);
        gate.blocked.is_none()
            && !gate.running
            && !model.sending()
            && !model.draft_message().is_empty()
    }

    // ---- commands --------------------------------------------------------------------------

    pub fn send(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.can_send(cx) {
            return false;
        }
        // Sending follows the conversation to its end again.
        self.list.set_follow_mode(FollowMode::Tail);
        self.attach = None;
        self.model.update(cx, |model, cx| model.send(cx))
    }

    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        let info = self.info(cx);
        if self.gate(info.as_ref(), cx).running {
            self.model.update(cx, |model, cx| model.interrupt(cx));
        }
    }

    /// Answers `approval` with `option` through this window's connection.
    pub fn answer(&mut self, approval: ApprovalId, option: String, cx: &mut Context<Self>) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.respond_approval(approval, option, cx));
        let model = self.model.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = task.await {
                model.update(cx, |model, cx| {
                    model.set_error(format!("Could not answer: {error}"), cx);
                });
            }
        })
        .detach();
    }

    // ---- keys ------------------------------------------------------------------------------

    fn edit(&mut self, change: impl FnOnce(&mut TextInput), cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| model.edit(change, cx));
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        let key = keystroke.key.as_str();
        let plain = !mods.platform && !mods.control && !mods.alt && !mods.shift && !mods.function;

        // The attach form's field takes the typing while a form is open.
        if self.on_attach_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        // Tab turns the mention at the caret (`@file src/lib.rs`) into what it offers.
        if plain && key == "tab" && self.accept_mention(cx) {
            cx.stop_propagation();
            return;
        }

        // 1–4 answer the oldest request while the composer is empty.
        if plain
            && let Some(number) = key.parse::<usize>().ok().filter(|n| (1..=4).contains(n))
            && self.model.read(cx).input().is_empty()
            && let Some(approval) = self.pending.first().copied()
        {
            let option = self
                .model
                .read(cx)
                .transcript()
                .approval(approval)
                .and_then(|a| a.options.get(number - 1))
                .map(|o| o.id.clone());
            if let Some(option) = option {
                self.answer(approval, option, cx);
                cx.stop_propagation();
                return;
            }
        }

        if mods.platform {
            match key {
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.edit(|input| input.insert(&text), cx);
                    }
                }
                "backspace" => self.edit(TextInput::delete_to_line_start, cx),
                "left" => self.edit(TextInput::home, cx),
                "right" => self.edit(TextInput::end, cx),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        if mods.control {
            match key {
                "a" => self.edit(TextInput::home, cx),
                "e" => self.edit(TextInput::end, cx),
                "b" => self.edit(TextInput::left, cx),
                "f" => self.edit(TextInput::right, cx),
                "u" => self.edit(TextInput::delete_to_line_start, cx),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        if mods.alt {
            match key {
                "left" => self.edit(TextInput::word_left, cx),
                "right" => self.edit(TextInput::word_right, cx),
                "backspace" => self.edit(TextInput::delete_word_back, cx),
                _ => {
                    // Option composes characters (é, ø): handled as typing below.
                    if !self.type_text(keystroke.key_char.as_deref(), cx) {
                        return;
                    }
                }
            }
            cx.stop_propagation();
            return;
        }
        match key {
            "enter" => {
                let send_on_enter = self.settings.get::<ChatSettings>().send_on_enter;
                if send_on_enter && !mods.shift {
                    self.send(cx);
                } else {
                    self.edit(|input| input.insert("\n"), cx);
                }
            }
            "backspace" => self.edit(TextInput::backspace, cx),
            "delete" => self.edit(TextInput::delete, cx),
            "left" => self.edit(TextInput::left, cx),
            "right" => self.edit(TextInput::right, cx),
            "home" => self.edit(TextInput::home, cx),
            "end" => self.edit(TextInput::end, cx),
            "up" => {
                self.edit(
                    |input| {
                        input.up();
                    },
                    cx,
                );
            }
            "down" => {
                self.edit(
                    |input| {
                        input.down();
                    },
                    cx,
                );
            }
            _ => {
                if !self.type_text(keystroke.key_char.as_deref(), cx) {
                    return;
                }
            }
        }
        cx.stop_propagation();
    }

    fn type_text(&mut self, text: Option<&str>, cx: &mut Context<Self>) -> bool {
        match text {
            Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => {
                let text = text.to_owned();
                self.edit(move |input| input.insert(&text), cx);
                true
            }
            _ => false,
        }
    }

    // ---- rendering -------------------------------------------------------------------------

    fn render_header(
        &self,
        info: Option<&SessionInfo>,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let Some(info) = info else {
            return div().into_any_element();
        };
        let presentation = self.sessions.read(cx).presentation(info);
        let chat = info.chat.as_ref();
        let detail = chat.map(|chat| {
            if is_gateway_chat(&chat.agent) {
                format!(
                    "{} · {}",
                    chat.provider_id.as_deref().unwrap_or("provider"),
                    chat.model
                )
            } else {
                format!(
                    "{} · {} · approvals {} · sandbox {}",
                    chat.agent, chat.model, chat.approval_policy, chat.sandbox
                )
            }
        });
        let pending = self.pending.len();
        let session = self.session;
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .h(px(tokens.pane_header))
            .px(px(tokens.spacing * 3.0))
            .border_b_1()
            .border_color(theme.border)
            .child(div().text_color(theme.muted).child(kind_icon(info.kind)))
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(info.title.clone())),
            )
            .child(state_dot(theme, &presentation))
            .child(
                div()
                    .text_color(tone_color(theme, presentation.tone))
                    .child(SharedString::from(presentation.label.clone())),
            )
            .children(detail.map(|text| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted)
                    .text_size(px(tokens.section_font_size + 1.0))
                    .child(SharedString::from(text))
            }))
            .when(chat.is_some_and(|chat| is_gateway_chat(&chat.agent)), |s| {
                s.child(
                    button(
                        "chat-switch-model",
                        "Switch model…",
                        ButtonKind::Plain,
                        chat.is_some_and(|chat| !chat.turn_active),
                        look,
                    )
                    .when(chat.is_some_and(|chat| !chat.turn_active), |b| {
                        b.on_click(move |_, window, cx| {
                            window.dispatch_action(
                                Box::new(actions::SwitchChatModelById(session.to_string())),
                                cx,
                            );
                        })
                    })
                    .child(probe(&self.regions, Region::Chat("switch-model"))),
                )
            })
            .when(pending > 0, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.warning)
                        .child(SharedString::from(format!(
                            "● {pending} approval{} waiting",
                            if pending == 1 { "" } else { "s" }
                        ))),
                )
            })
            .child(probe(&self.regions, Region::Chat("header")))
            .into_any_element()
    }

    pub(super) fn provider_error_action(
        &self,
        message: &str,
        look: &Look,
        cx: &App,
    ) -> Option<AnyElement> {
        let info = self.sessions.read(cx).session(self.session)?;
        let chat = info
            .chat
            .as_ref()
            .filter(|chat| is_gateway_chat(&chat.agent))?;
        let session = self.session;
        if message.contains("HTTP 401")
            || message.contains("HTTP 403")
            || message.contains("no API key")
        {
            let id = chat.provider_id.clone()?;
            Some(
                button("chat-fix-key", "Set key…", ButtonKind::Plain, true, look)
                    .child(probe(&self.regions, Region::Chat("fix-key")))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(actions::SetProviderKey(id.clone())), cx)
                    })
                    .into_any_element(),
            )
        } else if message.contains("not found:") {
            Some(
                button(
                    "chat-fix-model",
                    "Pick another model…",
                    ButtonKind::Plain,
                    !chat.turn_active,
                    look,
                )
                .child(probe(&self.regions, Region::Chat("fix-model")))
                .when(!chat.turn_active, |b| {
                    b.on_click(move |_, window, cx| {
                        window.dispatch_action(
                            Box::new(actions::SwitchChatModelById(session.to_string())),
                            cx,
                        );
                    })
                })
                .into_any_element(),
            )
        } else {
            None
        }
    }

    fn render_banner(&self, look: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let model = self.model.read(cx);
        let (text, color, dismissable) = match (model.error(), model.status()) {
            (Some(error), _) => (error.to_string(), theme.error, true),
            (None, LoadStatus::Failed(error)) => (
                format!("The conversation could not be loaded: {error}"),
                theme.warning,
                false,
            ),
            (None, _) if model.history_cut() => (
                "Older history is not shown (`hutan chat log` has all of it).".to_owned(),
                theme.muted,
                false,
            ),
            _ => return None,
        };
        let hover = theme.hover;
        let recovery = self.provider_error_action(&text, look, cx);
        Some(
            div()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .border_b_1()
                .border_color(theme.border)
                .text_color(color)
                .child(div().flex_1().min_w_0().child(SharedString::from(text)))
                .children(recovery)
                .when(dismissable, |s| {
                    s.child(
                        div()
                            .id("chat-error-dismiss")
                            .px(px(tokens.spacing))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.update(cx, |model, cx| model.clear_error(cx));
                            }))
                            .child("×"),
                    )
                })
                .child(probe(&self.regions, Region::Chat("banner")))
                .into_any_element(),
        )
    }

    fn render_empty(&self, look: &Look, cx: &App) -> AnyElement {
        let theme = &look.theme;
        let text = match self.model.read(cx).status() {
            LoadStatus::Loading => "Loading the conversation…".to_owned(),
            LoadStatus::Failed(_) => "No conversation to show.".to_owned(),
            LoadStatus::Ready if self.supports_context(cx) => {
                "Ask the assistant. Nothing is shared with it unless you attach it: @shell, @omp, @agent, @file or @search, or the Attach button."
                    .to_owned()
            }
            LoadStatus::Ready => {
                "Ask the agent to do something. Commands, edits and approvals appear here."
                    .to_owned()
            }
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.muted)
            .child(SharedString::from(text))
            .into_any_element()
    }

    /// The composer's text with the caret in it (when the view has focus), or the placeholder.
    fn input_text(&self, focused: bool, look: &Look, cx: &App) -> AnyElement {
        let theme = &look.theme;
        let input = self.model.read(cx).input();
        if input.is_empty() {
            let placeholder = div()
                .text_color(theme.muted)
                .child(if self.supports_context(cx) {
                    "Ask the assistant… (@shell @file @search to attach context)"
                } else {
                    "Message the agent…"
                });
            return if focused {
                div()
                    .flex()
                    .items_center()
                    .child(div().text_color(theme.accent).child(CARET))
                    .child(placeholder)
                    .into_any_element()
            } else {
                placeholder.into_any_element()
            };
        }
        let text = input.text();
        if !focused {
            return div()
                .child(SharedString::from(text.to_owned()))
                .into_any_element();
        }
        let (before, after) = text.split_at(input.cursor());
        let shown = format!("{before}{CARET}{after}");
        let run = |len: usize, color: gpui::Hsla| TextRun {
            len,
            font: font(".SystemUIFont"),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs: Vec<TextRun> = [
            (before.len(), theme.fg),
            (CARET.len(), theme.accent),
            (after.len(), theme.fg),
        ]
        .into_iter()
        .filter(|(len, _)| *len > 0)
        .map(|(len, color)| run(len, color))
        .collect();
        div()
            .child(StyledText::new(shown).with_runs(runs))
            .into_any_element()
    }

    fn render_composer(
        &mut self,
        gate: &Gate,
        window: &mut Window,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let focused = self.focus_handle.is_focused(window);
        let send_on_enter = self.settings.get::<ChatSettings>().send_on_enter;
        let (chips, context, sending, can_send) = {
            let model = self.model.read(cx);
            (
                model.chips().to_vec(),
                model.context().to_vec(),
                model.sending(),
                self.can_send(cx),
            )
        };
        let supports_context = self.supports_context(cx);
        let mention_hint = self.render_mention_hint(look, cx);
        let attach_button = supports_context.then(|| self.render_attach_button(look, cx));
        let hint = if gate.running {
            "⌘. interrupts the turn".to_owned()
        } else if send_on_enter {
            "↩ send · ⇧↩ newline".to_owned()
        } else {
            "⌘↩ send · ↩ newline".to_owned()
        };
        let session = self.session;
        let chip_row = div().flex().flex_wrap().gap(px(tokens.spacing)).children(
            chips.iter().enumerate().map(|(index, chip)| {
                let hover = theme.hover;
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .pl(px(tokens.spacing * 2.0))
                    .pr(px(tokens.spacing))
                    .h(px(tokens.sidebar_row - 4.0))
                    .rounded(px(tokens.radius))
                    .border_1()
                    .border_color(theme.accent.opacity(0.5))
                    .bg(theme.accent.opacity(0.12))
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.section_font_size + 1.0))
                    .child(SharedString::from(chip.label.clone()))
                    .child(
                        div()
                            .id(SharedString::from(format!("chat-chip-remove-{index}")))
                            .px(px(tokens.spacing))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model
                                    .update(cx, |model, cx| model.remove_chip(index, cx));
                            }))
                            .child("×"),
                    )
                    .child(probe(&self.regions, Region::ChatChip(index)))
            }),
        );
        let context_row = div().flex().flex_wrap().gap(px(tokens.spacing)).children(
            context.iter().enumerate().map(|(index, chip)| {
                let hover = theme.hover;
                let color = if chip.refused.is_some() {
                    theme.error
                } else {
                    theme.accent
                };
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .pl(px(tokens.spacing * 2.0))
                    .pr(px(tokens.spacing))
                    .h(px(tokens.sidebar_row - 4.0))
                    .rounded(px(tokens.radius))
                    .border_1()
                    .border_color(color.opacity(0.6))
                    .bg(color.opacity(0.12))
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.section_font_size + 1.0))
                    .child(SharedString::from(chip.label()))
                    .when(chip.refused.is_some(), |s| {
                        s.child(div().text_color(theme.error).child("· refused"))
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("chat-context-remove-{index}")))
                            .px(px(tokens.spacing))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model
                                    .update(cx, |model, cx| model.remove_context(index, cx));
                            }))
                            .child("×"),
                    )
                    .child(probe(&self.regions, Region::ChatContextChip(index)))
            }),
        );
        let input = div()
            .id("chat-input")
            .relative()
            .max_h(px(COMPOSER_MAX_HEIGHT))
            .min_h(px(tokens.font_size * 3.2))
            .overflow_y_scroll()
            .child(self.input_text(focused && gate.blocked.is_none(), look, cx));

        let send = button("chat-send", "Send", ButtonKind::Primary, can_send, look)
            .when(can_send, |s| {
                s.on_click(cx.listener(|this, _, _, cx| {
                    this.send(cx);
                }))
            })
            .child(probe(&self.regions, Region::Chat("send")));
        let interrupt = button(
            "chat-interrupt",
            "■ Interrupt",
            ButtonKind::Deny,
            true,
            look,
        )
        .on_click(cx.listener(|this, _, _, cx| this.interrupt(cx)))
        .child(probe(&self.regions, Region::Chat("interrupt")));
        let restart = button("chat-restart", "Restart", ButtonKind::Plain, true, look)
            .on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(RestartSessionById(session.to_string())), cx);
            })
            .child(probe(&self.regions, Region::Chat("restart")));

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing * 1.5))
            .m(px(tokens.spacing * 3.0))
            .p(px(tokens.spacing * 2.5))
            .rounded(px(tokens.radius + 2.0))
            .border_1()
            .border_color(if focused { theme.accent } else { theme.border })
            .bg(theme.overlay)
            .when(!chips.is_empty(), |s| s.child(chip_row))
            .when(!context.is_empty(), |s| s.child(context_row))
            .child(input)
            .children(mention_hint)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(tokens.spacing * 2.0))
                            .children(attach_button)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.muted)
                                    .text_size(px(tokens.section_font_size + 1.0))
                                    .child(SharedString::from(
                                        gate.blocked
                                            .as_ref()
                                            .map_or(hint, std::string::ToString::to_string),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(tokens.spacing * 2.0))
                            .when(gate.restartable, |s| s.child(restart))
                            .when(gate.running, |s| s.child(interrupt))
                            .when(!gate.running && !gate.restartable, |s| {
                                s.child(send.opacity(if sending { 0.6 } else { 1.0 }))
                            }),
                    ),
            )
            .child(probe(&self.regions, Region::Chat("composer")))
            .into_any_element()
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_list(cx);
        self.pending = self
            .model
            .read(cx)
            .transcript()
            .pending_approvals()
            .into_iter()
            .map(|approval| approval.id)
            .collect();
        let look = Look::of(cx);
        let info = self.info(cx);
        let gate = self.gate(info.as_ref(), cx);
        let header = self.render_header(info.as_ref(), &look, cx);
        let banner = self.render_banner(&look, cx);
        let attach = self.render_attach(&look, cx);
        let composer = self.render_composer(&gate, window, &look, cx);
        let empty = self.model.read(cx).transcript().is_empty();
        let body = if empty {
            self.render_empty(&look, cx)
        } else {
            div()
                .size_full()
                .child(
                    list(self.list.clone(), cx.processor(Self::render_entry))
                        .size_full()
                        .into_any_element(),
                )
                .into_any_element()
        };
        let theme = look.theme;
        div()
            .id("chat")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .on_action(cx.listener(|this, _: &actions::chat::SendMessage, _, cx| {
                this.send(cx);
            }))
            .on_action(
                cx.listener(|this, _: &actions::chat::InterruptTurn, _, cx| {
                    this.interrupt(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // The workbench re-focuses itself on this very mouse down; focus the chat
                    // once the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.focus_handle, cx);
                    });
                    let _ = this;
                }),
            )
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .child(header)
            .children(banner)
            .child(div().relative().flex_1().min_h_0().child(body))
            .children(attach)
            .child(composer)
    }
}
