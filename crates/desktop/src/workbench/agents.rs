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
use hutan_core::agent::ModelInfo;
use hutan_core::{ApprovalId, SessionId, WorkspaceId};
use hutan_protocol::blocks::BlockPart;
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::SessionInfo;

use super::actions::palette;
use super::chat::{ButtonKind, ChatModel, ChatView, Chip, ContextChip, Look, button, cut_to};
use super::harnesses::{self, Detection, HARNESSES};
use super::model::{FocusTarget, ItemRef, PaneId};
use super::panels::AssistantPanel;
use super::render::probe;
use super::terminals::session_of_item;
use super::ui::{self, StyledExt as _};
use super::view::{Region, Workbench};
use crate::settings::ChatSettings;
use crate::theme::DensityExt as _;

pub use super::harnesses::CODEX;

/// The models a picker may offer once the daemon answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Models {
    Loading,
    Ready(Vec<ModelInfo>),
    Failed(String),
}

/// The ⌘⇧T dialog: which harness (or direct model) and model a new chat starts with, and where.
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
    /// The harness a new chat starts with (`omp`, `claude` or `codex`); the harness of the chat
    /// when the dialog switches a model. A direct chat has none (`provider_id` says what).
    pub agent: String,
    /// The user picked the harness: what the daemon finds installed no longer moves it.
    pub agent_chosen: bool,
}

impl AgentPicker {
    /// The models the dialog lists. OMP's are grouped by provider (stable within a group).
    pub fn shown(&self) -> Vec<&ModelInfo> {
        match &self.models {
            Models::Ready(models) => {
                let query = self.model_query.to_lowercase();
                let mut shown: Vec<&ModelInfo> = models
                    .iter()
                    .filter(|m| {
                        !m.hidden
                            && (query.is_empty()
                                || m.id.to_lowercase().contains(&query)
                                || m.display_name.to_lowercase().contains(&query))
                    })
                    .collect();
                if self.provider_id.is_none() {
                    let agent = self.agent.as_str();
                    shown.sort_by_key(|m| harnesses::model_group(agent, m));
                }
                shown
            }
            _ => Vec::new(),
        }
    }

    /// The model a new chat would start with.
    pub fn chosen(&self) -> Option<&ModelInfo> {
        self.shown().get(self.selected).copied()
    }

    /// Whether the harness offers no model to choose from, so that a chat starts with the
    /// harness's own default.
    pub fn offers_no_model(&self) -> bool {
        self.provider_id.is_none()
            && self.switch_session.is_none()
            && matches!(&self.models, Models::Ready(models) if models.iter().all(|m| m.hidden))
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
        self.open_agent_picker_for(direct, None, window, cx);
    }

    fn open_agent_picker_for(
        &mut self,
        direct: bool,
        switch: Option<(SessionId, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (connected, supported, workspace, cwd) = {
            let store = self.sessions.read(cx);
            let workspace = self.target_workspace(store);
            (
                store.connection_state() == hutan_client::ConnectionState::Connected,
                store.connection().supports_chat(),
                workspace.map(|w| w.id),
                workspace
                    .and_then(|w| w.roots.first().cloned())
                    .or_else(std::env::home_dir),
            )
        };
        if !connected {
            self.set_notice("Not connected to hutand", cx);
            return;
        }
        if !supported {
            self.set_notice(
                "This hutand does not run agent chats; restart it (Daemon → Restart)",
                cx,
            );
            return;
        }
        self.palette = None;
        self.picker = None;
        self.context_menu = None;
        let (agent, switch_session, agent_chosen) = match switch {
            Some((session, agent)) => (agent, Some(session), true),
            None => (self.default_harness(cx), None, false),
        };
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
            switch_session,
            direct,
            agent,
            agent_chosen,
        });
        window.focus(&self.agent_focus, cx);
        if direct {
            self.refresh_providers(cx);
            cx.notify();
            return;
        }
        // What is installed and logged in, and the models of the harness shown first.
        if switch_session.is_none() {
            self.refresh_accounts(cx);
        }
        self.load_harness_models(cx);
        cx.notify();
        if switch_session.is_none() {
            self.refresh_providers(cx);
        }
    }

    /// The model dialog of a running harness chat (OMP, Claude Code): the models its harness
    /// offers; the daemon switches them between turns where the harness allows it.
    pub(crate) fn open_harness_model_switch(
        &mut self,
        session: SessionId,
        agent: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_agent_picker_for(false, Some((session, agent)), window, cx);
    }

    /// Whether the daemon can start chats with `harness`: Codex always, the ACP harnesses when
    /// it advertises them.
    fn harness_available(&self, harness: &str, cx: &gpui::App) -> bool {
        harness == CODEX || self.sessions.read(cx).connection().supports_harness_chat()
    }

    /// The harness a picker preselects: the first of OMP, Claude Code, Codex that the daemon
    /// can run and has not found missing.
    fn default_harness(&self, cx: &gpui::App) -> String {
        let directory = self.env.harnesses.read(cx);
        HARNESSES
            .iter()
            .find(|h| self.harness_available(h.id, cx) && directory.startable(h.id))
            .map_or(CODEX, |h| h.id)
            .to_owned()
    }

    /// Asks the shown harness for its models (the picker follows the directory's answer).
    fn load_harness_models(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.agent_picker else {
            return;
        };
        if picker.direct {
            return;
        }
        picker.models = Models::Loading;
        picker.selected = 0;
        let agent = picker.agent.clone();
        self.env
            .harnesses
            .update(cx, |directory, cx| directory.load_models(&agent, cx));
    }

    /// Picks the harness a new chat starts with (a click on its row). One the daemon cannot
    /// find or run is not pickable.
    pub fn agent_picker_choose_agent(&mut self, agent: &str, cx: &mut Context<Self>) {
        if !self.harness_available(agent, cx) || !self.env.harnesses.read(cx).startable(agent) {
            return;
        }
        let Some(picker) = &mut self.agent_picker else {
            return;
        };
        if picker.creating || picker.switch_session.is_some() {
            return;
        }
        picker.agent = agent.to_owned();
        picker.agent_chosen = true;
        picker.direct = false;
        picker.provider_id = None;
        picker.error = None;
        picker.model_query.clear();
        self.load_harness_models(cx);
        cx.notify();
    }

    /// What the directory learned (accounts, models) reaches the open picker: the preselected
    /// harness follows what is installed until the user picks one, and the model list is the
    /// shown harness's.
    pub(crate) fn on_harnesses_changed(&mut self, cx: &mut Context<Self>) {
        self.auto_install_step(cx);
        let Some(picker) = &self.agent_picker else {
            return;
        };
        if picker.direct || picker.provider_id.is_some() {
            return;
        }
        let preselect = (!picker.agent_chosen && picker.switch_session.is_none())
            .then(|| self.default_harness(cx));
        if let Some(agent) = preselect
            && picker.agent != agent
        {
            if let Some(picker) = &mut self.agent_picker {
                picker.agent = agent;
            }
            self.load_harness_models(cx);
            cx.notify();
            return;
        }
        let models = self.env.harnesses.read(cx).models(&picker.agent).cloned();
        if let Some(models) = models
            && let Some(picker) = &mut self.agent_picker
            && picker.models != models
        {
            picker.models = models;
            picker.selected = picker
                .shown()
                .iter()
                .position(|model| model.is_default)
                .unwrap_or(0);
        }
        cx.notify();
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
        let model = picker.chosen().map(|m| m.id.clone());
        // A harness that offers no model starts with its own default.
        if model.is_none() && !picker.offers_no_model() {
            return;
        }
        let Some(cwd) = picker.cwd.clone() else {
            return;
        };
        picker.creating = true;
        picker.error = None;
        if let Some(session) = picker.switch_session {
            let provider_id = picker.provider_id.clone();
            let Some(model) = model else {
                picker.creating = false;
                return;
            };
            self.confirm_model_switch(session, provider_id, model, window, cx);
            return;
        }
        let request = ChatCreate {
            workspace_id: picker.workspace,
            cwd,
            agent: if picker.provider_id.is_some() {
                "direct".to_owned()
            } else {
                picker.agent.clone()
            },
            model,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
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
        let hover = theme.surface_hover;
        let look = Look::of(cx);
        let directory = self.env.harnesses.read(cx);
        let agents: Vec<AnyElement> = HARNESSES
            .iter()
            .map(|harness| {
                let id = harness.id;
                let detection = directory.detection(id);
                let available = self.harness_available(id, cx);
                let pickable =
                    available && !matches!(detection, Detection::NotFound(_)) && !picker.creating;
                let selected = !picker.direct && picker.provider_id.is_none() && picker.agent == id;
                let (state, state_color) = match (&detection, available) {
                    (_, false) => ("needs a newer hutand", theme.warning),
                    (Detection::Installed, _) => ("installed", theme.success),
                    (Detection::NotFound(_), _) => ("not found", theme.warning),
                    (Detection::Checking, _) => ("checking…", theme.muted),
                    (Detection::Unknown, _) => ("unknown", theme.muted),
                };
                // Installed: which providers it is logged in to. Missing: how to install it.
                let detail = match (&detection, available) {
                    (_, false) => {
                        "Restart hutand (Daemon → Restart) to run the version of this app."
                            .to_owned()
                    }
                    (Detection::NotFound(how), _) => how.clone(),
                    _ => harnesses::login_summary(directory, id),
                };
                div()
                    .id(ElementId::from(SharedString::from(format!("agent-{id}"))))
                    .relative()
                    .flex()
                    .items_start()
                    .gap(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .when(pickable, |s| {
                        s.cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.agent_picker_choose_agent(id, cx);
                            }))
                    })
                    .when(!pickable && !picker.creating, |s| s.opacity(0.6))
                    .child(div().w(px(14.0)).text_color(theme.muted).child("◆"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().medium().child(SharedString::from(harness.name)))
                            .child(
                                div()
                                    .text_color(theme.muted)
                                    .text_size(px(tokens.secondary_font_size))
                                    .child(SharedString::from(detail)),
                            ),
                    )
                    .child(ui::pill(state, state_color, &tokens))
                    .child(probe(&self.regions, Region::AgentHarness(id)))
                    .into_any_element()
            })
            .collect();
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
                    ButtonKind::Secondary,
                    !picker.creating,
                    &look,
                )
                .when(selected, |s| s.bg(theme.accent_soft))
                .on_click(cx.listener(move |this, _, _, cx| this.choose_direct_provider(&id, cx)))
                .into_any_element()
            })
            .collect();
        let shown = picker.shown();
        let mut models: Vec<AnyElement> = Vec::new();
        let mut group: Option<Option<String>> = None;
        for (index, model) in shown.iter().enumerate() {
            let model_group = harnesses::model_group(&picker.agent, model)
                .filter(|_| picker.provider_id.is_none());
            // OMP's models come grouped by provider; a header opens each group.
            if model_group.is_some() && group.as_ref() != Some(&model_group) {
                models.push(
                    ui::section_label(model_group.clone().unwrap_or_default(), &theme, &tokens)
                        .px(px(tokens.spacing * 2.0))
                        .pt(px(tokens.spacing))
                        .into_any_element(),
                );
            }
            group = Some(model_group);
            let selected = index == picker.selected;
            models.push(
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
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .when(selected, |s| s.bg(theme.accent_soft))
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
                            .text_size(px(tokens.secondary_font_size))
                            .child(SharedString::from(model.description.clone())),
                    )
                    .child(probe(&self.regions, Region::AgentModel(index)))
                    .into_any_element(),
            );
        }
        let note: Option<(SharedString, gpui::Hsla)> = match (&picker.models, &picker.error) {
            (_, Some(error)) => Some((format!("Could not start: {error}").into(), theme.danger)),
            (Models::Loading, _) => Some(("Loading the models…".into(), theme.muted)),
            (Models::Failed(error), _) => Some((
                format!("Could not list the models: {error}").into(),
                theme.danger,
            )),
            (Models::Ready(_), _) if picker.offers_no_model() => Some((
                "The harness offers no model to choose: it starts with its own default.".into(),
                theme.muted,
            )),
            (Models::Ready(_), _) if picker.shown().is_empty() => {
                Some(("The agent offers no models.".into(), theme.muted))
            }
            _ => None,
        };
        let can_start = (picker.chosen().is_some() || picker.offers_no_model())
            && picker.cwd.is_some()
            && !picker.creating;
        let cwd = picker
            .cwd
            .as_ref()
            .map_or_else(|| "no folder".to_owned(), |cwd| cwd.display().to_string());
        let section = |text: &'static str| {
            div()
                .px(px(tokens.spacing * 2.0))
                .pt(px(tokens.spacing * 2.0))
                .text_size(px(tokens.section_font_size))
                .font_weight(gpui::FontWeight::MEDIUM)
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
                        .popover(&theme, &tokens)
                        .p(px(tokens.spacing * 2.0))
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
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(if picker.switch_session.is_some() {
                                    "Switch model"
                                } else if picker.direct {
                                    "New direct chat"
                                } else {
                                    "New agent"
                                }),
                        )
                        .when(picker.switch_session.is_none(), |s| {
                            s.child(section("Agent")).children(agents)
                        })
                        .children(providers)
                        .when(self.providers.providers.is_empty(), |s| {
                            s.child(
                                button(
                                    "agent-manage-models",
                                    "Direct model · Add a provider…",
                                    ButtonKind::Secondary,
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
                        .child(section("Model"))
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
                        .children(note.as_ref().map(|(text, color)| {
                            div()
                                .px(px(tokens.spacing * 2.0))
                                .text_color(*color)
                                .child(text.clone())
                        }))
                        .children(
                            note.as_ref()
                                .and_then(|(text, _)| {
                                    crate::harness_install::harness_of_missing(text)
                                })
                                .filter(|_| self.harness_install_available())
                                .map(|harness| {
                                    div().px(px(tokens.spacing * 2.0)).child(
                                        button(
                                            "agent-install-harness",
                                            "Install…",
                                            ButtonKind::Secondary,
                                            true,
                                            &look,
                                        )
                                        .on_click(
                                            move |_, window, cx| {
                                                window.dispatch_action(
                                                    Box::new(super::actions::InstallHarness(
                                                        harness.to_owned(),
                                                    )),
                                                    cx,
                                                );
                                            },
                                        ),
                                    )
                                }),
                        )
                        .child(section("Folder"))
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
                                        ButtonKind::Secondary,
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
                                        ButtonKind::Ghost,
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
    /// composer of the last chat the user looked at (the assistant the Assistant panel shows,
    /// once they opened it after the last chat tab), or of the one they pick. An assistant takes
    /// the block as a structured attachment that its daemon reads (`shell_block`), never as text
    /// read here; the menu offers "Send to Assistant" (the assistant of the terminal's
    /// workspace, opened on demand) when none of the running chats is one.
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
        let shown_assistant = self
            .assistant_last
            .then(|| {
                self.panel::<AssistantPanel>("assistant")
                    .and_then(|panel| panel.read(cx).session(cx))
            })
            .flatten();
        let last = shown_assistant
            .or(self.last_chat)
            .filter(|last| candidates.iter().any(|c| c.id == *last));
        let target = match (last, candidates.as_slice()) {
            (Some(last), _) => Some(last),
            (None, [only]) => Some(only.id),
            (None, []) => {
                self.set_notice(
                    "No agent chat is running: start one with ⌘⇧T, or open the Assistant from ⌘K",
                    cx,
                );
                return;
            }
            (None, _) => None,
        };
        match target {
            Some(chat) => self.attach_block(session, block, chat, window, cx),
            None => {
                let mut entries: Vec<_> = candidates
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
                let has_assistant = candidates
                    .iter()
                    .any(|c| c.chat.as_ref().is_some_and(|chat| chat.is_assistant()));
                if self.sessions.read(cx).connection().supports_assistant() && !has_assistant {
                    entries.push(Self::entry(
                        "Send to Assistant".to_owned(),
                        move |this, window, cx| {
                            this.send_block_to_assistant(session, block, window, cx);
                        },
                    ));
                }
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
        let (command, exit_code) = self.block_facts(session, block, cx);
        if self.is_assistant(chat, cx) {
            let title = self
                .sessions
                .read(cx)
                .session(session)
                .map_or_else(|| "terminal".to_owned(), |info| info.title.clone());
            let chip = ContextChip::block(&title, terminal, block, command.as_deref(), exit_code);
            self.attach_to_assistant(chat, chip, window, cx);
            return;
        }
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
            self.assistant_last = false;
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
