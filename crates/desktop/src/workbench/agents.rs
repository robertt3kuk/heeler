//! Workbench behavior around agent chats (M4.4): chat tabs, the ⌘⇧T new-agent picker, global
//! approvals, and "Send to agent" from terminal blocks.
//!
//! A chat tab is a [`ChatView`] per `(pane, session)` over one [`ChatModel`] per session, held
//! while some tab shows the session (the transcript is read again from the daemon's journal when
//! a tab is opened later). The daemon owns everything else: the conversation, the approvals,
//! the agent process.

use std::collections::HashSet;
use std::path::PathBuf;

use gpui::{
    AnyElement, AppContext as _, Context, ElementId, Entity, Focusable as _, InteractiveElement,
    IntoElement, MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Window, div, prelude::*, px,
};
use heeler_core::agent::ModelInfo;
use heeler_core::{ApprovalId, SessionId, WorkspaceId};
use heeler_protocol::blocks::BlockPart;
use heeler_protocol::chat::ChatCreate;
use heeler_protocol::messages::SessionInfo;

use super::actions::palette;
use super::chat::{ButtonKind, ChatModel, ChatView, Chip, Look, button, cut_to};
use super::model::{FocusTarget, ItemRef, PaneId};
use super::render::probe;
use super::terminals::session_of_item;
use super::view::{Region, Workbench};
use crate::settings::ChatSettings;
use crate::theme::DensityExt as _;

/// The agent every chat starts with today.
pub const CODEX: &str = "codex";

/// Agents the picker lists: id, name, and the milestone that brings it (`None`: available).
const AGENTS: [(&str, &str, Option<&str>); 4] = [
    (CODEX, "Codex", None),
    ("claude", "Claude Code", Some("M8")),
    ("opencode", "OpenCode", Some("M8")),
    ("omp", "OMP", Some("M8")),
];

/// The models a picker may offer once the daemon answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Models {
    Loading,
    Ready(Vec<ModelInfo>),
    Failed(String),
}

/// The ⌘⇧T dialog: which agent and model a new chat starts with, and where.
#[derive(Debug, Clone)]
pub struct AgentPicker {
    pub models: Models,
    /// Index into the models that are shown (the hidden ones are not).
    pub selected: usize,
    pub workspace: Option<WorkspaceId>,
    pub cwd: Option<PathBuf>,
    /// `chat.create` is in flight.
    pub creating: bool,
    pub error: Option<String>,
    pub provider_id: Option<String>,
    pub model_query: String,
    pub system_prompt: Option<String>,
    pub switch_session: Option<SessionId>,
    pub direct: bool,
}

impl AgentPicker {
    /// The models the dialog lists.
    pub fn shown(&self) -> Vec<&ModelInfo> {
        match &self.models {
            Models::Ready(models) => {
                let query = self.model_query.to_lowercase();
                models
                    .iter()
                    .filter(|m| {
                        !m.hidden
                            && (query.is_empty()
                                || m.id.to_lowercase().contains(&query)
                                || m.display_name.to_lowercase().contains(&query))
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The model a new chat would start with.
    pub fn chosen(&self) -> Option<&ModelInfo> {
        self.shown().get(self.selected).copied()
    }
}

impl Workbench {
    // ---- chat tabs -------------------------------------------------------------------------

    /// Every `(pane, session)` that has a chat tab, in tree order.
    fn open_chat_items(&self) -> Vec<(PaneId, SessionId)> {
        self.state
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| {
                pane.items.iter().filter_map(move |item| match item {
                    ItemRef::Chat { session } => {
                        session.parse().ok().map(|session| (pane.id, session))
                    }
                    _ => None,
                })
            })
            .collect()
    }

    /// Creates the models and views the open chat tabs need and drops the ones no tab shows.
    pub(crate) fn sync_chats(&mut self, cx: &mut Context<Self>) {
        let items = self.open_chat_items();
        let mut wanted: HashSet<SessionId> = items.iter().map(|(_, session)| *session).collect();
        // Chats in the overview's stash keep their transcript.
        if let Some(saved) = &self.state.stashed {
            for pane in saved.center.panes() {
                for item in &pane.items {
                    if let ItemRef::Chat { session } = item
                        && let Ok(session) = session.parse()
                    {
                        wanted.insert(session);
                    }
                }
            }
        }
        self.chat_models
            .retain(|session, _| wanted.contains(session));
        let keep: HashSet<(PaneId, SessionId)> = items.into_iter().collect();
        let gone: Vec<(PaneId, SessionId)> = self
            .chats
            .keys()
            .filter(|key| !keep.contains(*key))
            .copied()
            .collect();
        for key in gone {
            if let Some(view) = self.chats.remove(&key) {
                // Focus may still sit on it; `rescue_orphaned_focus` checks before it is dropped.
                self.pruned_chats.push(view);
            }
        }
        for session in wanted {
            self.ensure_chat_model(session, cx);
        }
    }

    pub(crate) fn ensure_chat_model(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Entity<ChatModel> {
        if let Some(model) = self.chat_models.get(&session) {
            return model.clone();
        }
        let connection = self.sessions.read(cx).connection().clone();
        let model = cx.new(|cx| ChatModel::new(session, connection, cx));
        self.chat_models.insert(session, model.clone());
        model
    }

    /// The view of `session` in `pane`, created on first use.
    pub(crate) fn chat_view(
        &mut self,
        pane: PaneId,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Entity<ChatView> {
        if let Some(view) = self.chats.get(&(pane, session)) {
            return view.clone();
        }
        let model = self.ensure_chat_model(session, cx);
        let sessions = self.sessions.clone();
        let settings = self.settings.clone();
        let regions = self.regions.clone();
        let view = cx.new(|cx| ChatView::new(session, model, sessions, settings, regions, cx));
        self.chats.insert((pane, session), view.clone());
        view
    }

    /// The view of `session` in any pane (tests, inspection).
    pub fn chat_view_of(&self, session: SessionId) -> Option<Entity<ChatView>> {
        self.chats
            .iter()
            .find(|((_, s), _)| *s == session)
            .map(|(_, view)| view.clone())
    }

    /// The transcript and composer of `session`, while a tab holds it.
    pub fn chat_model_of(&self, session: SessionId) -> Option<Entity<ChatModel>> {
        self.chat_models.get(&session).cloned()
    }

    /// The view of the focused pane's active tab, if it is a chat.
    pub fn focused_chat_view(&self) -> Option<Entity<ChatView>> {
        let FocusTarget::Pane(_) = self.state.focus else {
            return None;
        };
        let pane = self.state.active_pane_id();
        match self.state.active_pane()?.active_item()? {
            ItemRef::Chat { session } => {
                let session = session.parse().ok()?;
                self.chats.get(&(pane, session)).cloned()
            }
            _ => None,
        }
    }

    /// Whether the session is an agent chat (it opens as a chat tab).
    pub(crate) fn is_chat(&self, session: SessionId, cx: &gpui::App) -> bool {
        self.sessions
            .read(cx)
            .session(session)
            .is_some_and(|info| info.chat.is_some())
    }

    /// Body of a chat tab.
    pub(crate) fn render_chat_item(
        &mut self,
        pane: PaneId,
        session: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Ok(id) = session.parse::<SessionId>() else {
            return self.placeholder_body(format!("Invalid session id {session}"), cx);
        };
        self.rendered_chats.insert((pane, id));
        let view = self.chat_view(pane, id, cx);
        div().size_full().child(view).into_any_element()
    }

    // ---- new agent -------------------------------------------------------------------------

    /// The open new-agent dialog, if any.
    pub fn agent_picker(&self) -> Option<&AgentPicker> {
        self.agent_picker.as_ref()
    }

    /// ⌘⇧T: asks which agent and model to start, in the active workspace.
    pub fn new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_agent_picker(false, window, cx);
    }

    pub fn new_direct_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_agent_picker(true, window, cx);
    }

    fn open_agent_picker(&mut self, direct: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (connected, supported, workspace, cwd) = {
            let store = self.sessions.read(cx);
            let workspace = self.target_workspace(store);
            (
                store.connection_state() == heeler_client::ConnectionState::Connected,
                store.connection().supports_chat(),
                workspace.map(|w| w.id),
                workspace
                    .and_then(|w| w.roots.first().cloned())
                    .or_else(std::env::home_dir),
            )
        };
        if !connected {
            self.set_notice("Not connected to heelerd", cx);
            return;
        }
        if !supported {
            self.set_notice(
                "This heelerd does not run agent chats; restart it (Daemon → Restart)",
                cx,
            );
            return;
        }
        self.palette = None;
        self.picker = None;
        self.context_menu = None;
        self.agent_picker = Some(AgentPicker {
            models: Models::Loading,
            selected: 0,
            workspace,
            cwd,
            creating: false,
            error: None,
            provider_id: None,
            model_query: String::new(),
            system_prompt: None,
            switch_session: None,
            direct,
        });
        window.focus(&self.agent_focus, cx);
        if direct {
            self.refresh_providers(cx);
            cx.notify();
            return;
        }
        let task = self
            .sessions
            .update(cx, |store, cx| store.chat_models(CODEX, cx));
        cx.spawn(async move |this, cx| {
            let answer = task.await;
            this.update(cx, |this, cx| {
                let Some(picker) = &mut this.agent_picker else {
                    return;
                };
                if picker.direct {
                    return;
                }
                picker.models = match answer {
                    Ok(models) => Models::Ready(models),
                    Err(error) => Models::Failed(error),
                };
                picker.selected = picker
                    .shown()
                    .iter()
                    .position(|model| model.is_default)
                    .unwrap_or(0);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
        self.refresh_providers(cx);
    }

    pub fn close_agent_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent_picker.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn agent_picker_select(&mut self, next: bool, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.agent_picker {
            let last = picker.shown().len().saturating_sub(1);
            picker.selected = if next {
                (picker.selected + 1).min(last)
            } else {
                picker.selected.saturating_sub(1)
            };
            cx.notify();
        }
    }

    /// Picks model row `index` (a click).
    pub fn agent_picker_choose(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.agent_picker
            && index < picker.shown().len()
        {
            picker.selected = index;
            cx.notify();
        }
    }

    /// Starts the chat the dialog describes and shows it.
    pub fn agent_picker_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.agent_picker else {
            return;
        };
        if picker.creating {
            return;
        }
        let (Some(model), Some(cwd)) = (picker.chosen().map(|m| m.id.clone()), picker.cwd.clone())
        else {
            return;
        };
        picker.creating = true;
        picker.error = None;
        if let Some(session) = picker.switch_session {
            let provider_id = picker.provider_id.clone();
            self.confirm_model_switch(session, provider_id, model, window, cx);
            return;
        }
        let request = ChatCreate {
            workspace_id: picker.workspace,
            cwd,
            agent: if picker.provider_id.is_some() {
                "direct"
            } else {
                CODEX
            }
            .to_owned(),
            model: Some(model),
            approval_policy: None,
            sandbox: None,
            title: None,
            provider_id: picker.provider_id.clone(),
            system_prompt: picker.system_prompt.clone(),
        };
        let task = self
            .sessions
            .update(cx, |store, cx| store.create_chat(request, cx));
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(info) => {
                this.update_in(cx, |this, window, cx| {
                    this.agent_picker = None;
                    this.open_session(info.id, window, cx);
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    if let Some(picker) = &mut this.agent_picker {
                        picker.creating = false;
                        picker.error = Some(error);
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn render_agent_picker(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.agent_picker.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.hover;
        let look = Look::of(cx);
        let agents = AGENTS.map(|(id, name, later)| {
            let row = div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .h(px(tokens.sidebar_row))
                .px(px(tokens.spacing * 2.0))
                .rounded(px(tokens.radius - 2.0))
                .child(div().w(px(14.0)).text_color(theme.muted).child("◆"))
                .child(div().child(name));
            match later {
                None => row
                    .id("agent-codex")
                    .when(!picker.direct, |s| s.bg(theme.accent.opacity(0.25)))
                    .cursor_pointer()
                    .when(!picker.creating, |s| {
                        s.on_click(cx.listener(|this, _, window, cx| this.new_agent(window, cx)))
                    })
                    .into_any_element(),
                Some(milestone) => row
                    .id(ElementId::from(SharedString::from(format!("agent-{id}"))))
                    .opacity(0.5)
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child(SharedString::from(format!("arrives with {milestone}"))),
                    )
                    .into_any_element(),
            }
        });
        let providers: Vec<AnyElement> = self
            .providers
            .providers
            .iter()
            .map(|provider| {
                let id = provider.id.clone();
                let selected = picker.provider_id.as_deref() == Some(id.as_str());
                button(
                    format!("agent-provider-{id}"),
                    format!("Direct model · {}", provider.name),
                    ButtonKind::Plain,
                    !picker.creating,
                    &look,
                )
                .when(selected, |s| s.bg(theme.accent.opacity(0.25)))
                .on_click(cx.listener(move |this, _, _, cx| this.choose_direct_provider(&id, cx)))
                .into_any_element()
            })
            .collect();
        let models: Vec<AnyElement> = picker
            .shown()
            .iter()
            .enumerate()
            .map(|(index, model)| {
                let selected = index == picker.selected;
                div()
                    .id(ElementId::from(SharedString::from(format!(
                        "agent-model-{index}"
                    ))))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius - 2.0))
                    .cursor_pointer()
                    .when(selected, |s| s.bg(theme.accent.opacity(0.25)))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.agent_picker_choose(index, cx);
                    }))
                    .child(
                        div()
                            .w(px(14.0))
                            .text_color(theme.muted)
                            .child(if selected { "●" } else { "○" }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(SharedString::from(model.display_name.clone())),
                    )
                    .when(model.is_default, |s| {
                        s.child(div().text_color(theme.muted).child("default"))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted)
                            .text_size(px(tokens.section_font_size + 1.0))
                            .child(SharedString::from(model.description.clone())),
                    )
                    .child(probe(&self.regions, Region::AgentModel(index)))
                    .into_any_element()
            })
            .collect();
        let note: Option<(SharedString, gpui::Hsla)> = match (&picker.models, &picker.error) {
            (_, Some(error)) => Some((format!("Could not start: {error}").into(), theme.error)),
            (Models::Loading, _) => Some(("Loading the models…".into(), theme.muted)),
            (Models::Failed(error), _) => Some((
                format!("Could not list the models: {error}").into(),
                theme.error,
            )),
            (Models::Ready(_), _) if picker.shown().is_empty() => {
                Some(("The agent offers no models.".into(), theme.muted))
            }
            _ => None,
        };
        let can_start = picker.chosen().is_some() && picker.cwd.is_some() && !picker.creating;
        let cwd = picker
            .cwd
            .as_ref()
            .map_or_else(|| "no folder".to_owned(), |cwd| cwd.display().to_string());
        let section = |text: &'static str| {
            div()
                .px(px(tokens.spacing * 2.0))
                .pt(px(tokens.spacing * 2.0))
                .text_size(px(tokens.section_font_size))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted)
                .child(text)
        };
        Some(
            div()
                .id("agent-picker-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(80.0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_agent_picker(window, cx)),
                )
                .child(
                    div()
                        .id("agent-picker")
                        .key_context("AgentPicker")
                        .track_focus(&self.agent_focus)
                        .w(px(560.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .p(px(tokens.spacing * 2.0))
                        .rounded(px(tokens.radius + 4.0))
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                            let Some(picker) = &mut this.agent_picker else {
                                return;
                            };
                            if event.keystroke.key == "backspace" {
                                picker.model_query.pop();
                            } else if !event.keystroke.modifiers.platform
                                && !event.keystroke.modifiers.control
                                && let Some(text) = &event.keystroke.key_char
                            {
                                picker.model_query.push_str(text);
                            } else {
                                return;
                            }
                            picker.selected = 0;
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectNext, _, cx| {
                            this.agent_picker_select(true, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectPrev, _, cx| {
                            this.agent_picker_select(false, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::Confirm, window, cx| {
                            this.agent_picker_confirm(window, cx);
                        }))
                        .child(
                            div()
                                .px(px(tokens.spacing * 2.0))
                                .h(px(tokens.sidebar_row + 6.0))
                                .flex()
                                .items_center()
                                .border_b_1()
                                .border_color(theme.border)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(if picker.switch_session.is_some() {
                                    "Switch model"
                                } else if picker.direct {
                                    "New direct chat"
                                } else {
                                    "New agent"
                                }),
                        )
                        .when(picker.switch_session.is_none(), |s| {
                            s.child(section("AGENT")).children(agents)
                        })
                        .children(providers)
                        .when(self.providers.providers.is_empty(), |s| {
                            s.child(
                                button(
                                    "agent-manage-models",
                                    "Direct model · Add a provider…",
                                    ButtonKind::Plain,
                                    true,
                                    &look,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.agent_picker = None;
                                        this.open_models_settings(window, cx);
                                    },
                                )),
                            )
                        })
                        .child(section("MODEL"))
                        .child(
                            div().child(SharedString::from(if picker.model_query.is_empty() {
                                "Type to filter models".to_owned()
                            } else {
                                format!("Filter: {}", picker.model_query)
                            })),
                        )
                        .child(
                            div()
                                .id("agent-model-list")
                                .max_h(px(260.0))
                                .overflow_y_scroll()
                                .children(models),
                        )
                        .children(note.map(|(text, color)| {
                            div()
                                .px(px(tokens.spacing * 2.0))
                                .text_color(color)
                                .child(text)
                        }))
                        .child(section("FOLDER"))
                        .child(
                            div()
                                .px(px(tokens.spacing * 2.0))
                                .truncate()
                                .text_color(theme.muted)
                                .child(SharedString::from(cwd)),
                        )
                        .when(
                            picker.provider_id.is_some() && picker.switch_session.is_none(),
                            |s| {
                                s.child(
                                    button(
                                        "agent-system-prompt",
                                        "System prompt…",
                                        ButtonKind::Plain,
                                        true,
                                        &look,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.edit_chat_system_prompt(window, cx)
                                        },
                                    )),
                                )
                            },
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(tokens.spacing * 2.0))
                                .pt(px(tokens.spacing * 2.0))
                                .child(
                                    button(
                                        "agent-cancel",
                                        "Cancel",
                                        ButtonKind::Plain,
                                        true,
                                        &look,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.close_agent_picker(window, cx);
                                        },
                                    )),
                                )
                                .child(
                                    button(
                                        "agent-start",
                                        if picker.creating {
                                            "Starting…"
                                        } else if picker.switch_session.is_some() {
                                            "Switch ↩"
                                        } else {
                                            "Start ↩"
                                        },
                                        ButtonKind::Primary,
                                        can_start,
                                        &look,
                                    )
                                    .when(can_start, |s| {
                                        s.on_click(cx.listener(|this, _, window, cx| {
                                            this.agent_picker_confirm(window, cx);
                                        }))
                                    })
                                    .child(probe(&self.regions, Region::AgentPicker("start"))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    // ---- approvals -------------------------------------------------------------------------

    /// Answers a pending request from outside its chat (the inspector, the review picker).
    pub fn answer_approval(
        &mut self,
        approval: ApprovalId,
        option: String,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.respond_approval(approval, option, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Could not answer: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Shows the permission request waiting longest: its chat, where the card is.
    pub fn show_oldest_approval(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self
            .sessions
            .read(cx)
            .approvals()
            .first()
            .map(|approval| approval.session_id);
        match session {
            Some(session) => self.open_session(session, window, cx),
            None => self.set_notice("No approvals are waiting", cx),
        }
    }

    /// Reveals the inspector when a permission request arrives that this window has not seen
    /// (`panels.inspector.reveal_on_approval`).
    pub(crate) fn on_approvals_changed(&mut self, cx: &mut Context<Self>) {
        let seen: HashSet<ApprovalId> = self
            .sessions
            .read(cx)
            .approvals()
            .iter()
            .map(|a| a.id)
            .collect();
        let arrived = seen.iter().any(|id| !self.known_approvals.contains(id));
        let first_look = !self.sessions.read(cx).synced();
        self.known_approvals = seen;
        if !arrived || first_look {
            return;
        }
        let settings = self
            .settings
            .get::<crate::settings::InspectorPanelSettings>();
        if settings.reveal_on_approval {
            self.open_panel("inspector", cx);
        }
    }

    // ---- send to agent ---------------------------------------------------------------------

    /// "Send to agent" of a terminal block: its command and output become a chip in the
    /// composer of the last chat the user looked at, or of the one they pick.
    pub(crate) fn send_block_to_agent(
        &mut self,
        session: SessionId,
        block: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let candidates: Vec<SessionInfo> = self
            .sessions
            .read(cx)
            .sessions()
            .iter()
            .filter(|s| s.chat.is_some() && s.state.is_live())
            .cloned()
            .collect();
        let last = self
            .last_chat
            .filter(|last| candidates.iter().any(|c| c.id == *last));
        let target = match (last, candidates.as_slice()) {
            (Some(last), _) => Some(last),
            (None, [only]) => Some(only.id),
            (None, []) => {
                self.set_notice("No agent chat is running: start one with ⌘⇧T", cx);
                return;
            }
            (None, _) => None,
        };
        match target {
            Some(chat) => self.attach_block(session, block, chat, window, cx),
            None => {
                let entries = candidates
                    .iter()
                    .map(|info| {
                        let chat = info.id;
                        Self::entry(
                            format!("Send to {}", info.title),
                            move |this, window, cx| {
                                this.attach_block(session, block, chat, window, cx);
                            },
                        )
                    })
                    .collect();
                self.open_menu(window.mouse_position(), entries, window, cx);
            }
        }
    }

    /// Fetches the block's text from the daemon (cut at `chat.attach_max_bytes`) and attaches it
    /// to `chat`'s composer, opening the chat when no tab shows it.
    fn attach_block(
        &mut self,
        session: SessionId,
        block: u64,
        chat: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self
            .sessions
            .read(cx)
            .session(session)
            .and_then(|info| info.terminal_id)
        else {
            return;
        };
        let (command, exit_code) = self
            .terminal_view_of(session)
            .and_then(|view| {
                view.read(cx)
                    .model()
                    .blocks()
                    .get(block)
                    .map(|b| (b.command.clone(), b.exit_code))
            })
            .unwrap_or((None, None));
        let max = self.settings.get::<ChatSettings>().attach_max_bytes;
        let max = u32::try_from(max).unwrap_or(u32::MAX);
        let connection = self.sessions.read(cx).connection().clone();
        let model = self.ensure_chat_model(chat, cx);
        // The chat is shown so the user sees where the context went.
        self.open_session(chat, window, cx);
        cx.spawn(async move |this, cx| {
            let output = connection
                .terminal_block_text(terminal, block, BlockPart::Output, max)
                .await;
            let command_text = match command {
                Some(command) => Some(command),
                None => connection
                    .terminal_block_text(terminal, block, BlockPart::Command, 16 * 1024)
                    .await
                    .ok()
                    .map(|answer| answer.text),
            };
            match output {
                Ok(output) => {
                    let (text, cut) = cut_to(&output.text, max as usize);
                    let chip = Chip::from_block(
                        command_text.as_deref(),
                        exit_code,
                        &text,
                        output.truncated || cut,
                    );
                    model.update(cx, |model, cx| model.attach(chip, cx));
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.set_notice(format!("Could not read the block: {error}"), cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Remembers which chat the user looked at last (the target of "Send to agent").
    pub(crate) fn note_focused_chat(&mut self, cx: &gpui::App) {
        if let Some(session) = self.focused_session()
            && self.is_chat(session, cx)
        {
            self.last_chat = Some(session);
        }
    }

    /// Focuses the chat view of the focused pane's active tab.
    pub(crate) fn focus_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = self
            .state
            .active_pane()
            .and_then(|pane| pane.active_item())
            .cloned()
        else {
            return false;
        };
        let Some(session) = session_of_item(&item).filter(|_| matches!(item, ItemRef::Chat { .. }))
        else {
            return false;
        };
        let pane = self.state.active_pane_id();
        let view = self.chat_view(pane, session, cx);
        window.focus(&view.focus_handle(cx), cx);
        true
    }
}
