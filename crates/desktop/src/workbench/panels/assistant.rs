//! The Assistant panel (M16-B): the Global (Home) or the active workspace's assistant chat,
//! docked like any other panel.
//!
//! The panel asks the daemon for the default assistant of a scope (`assistant.open`) and shows
//! the returned session with the ordinary chat model and view (`workbench::chat`): the
//! transcript, the composer and its context chips are the chat view's. Each scope keeps its own
//! conversation: switching scope shows another session and never moves a chip, a draft or a
//! transcript from one to the other. Opening an assistant starts no terminal and no vendor
//! agent: it is a direct chat of the provider named under `[assistant]` in the daemon's
//! `daemon.toml`. Without that setting the panel says what is missing and links to Models; it
//! never picks a provider or model of its own.

use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::*, px,
};
use hutan_client::{ClientError, ConnectionState};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{AGENT_ASSISTANT, AssistantOpen, AssistantScope};
use hutan_protocol::messages::{WorkspaceInfo, WorkspaceKind};

use crate::workbench::actions::ManageProviders;
use crate::workbench::chat::{ButtonKind, ChatModel, ChatView, ContextChip, Look, button};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::view::Region;

/// A conversation the panel keeps: one per scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKey {
    /// The Home workspace's assistant.
    Global,
    /// The assistant of one project workspace.
    Workspace(WorkspaceId),
}

impl ScopeKey {
    /// The scope the workspace's assistant has.
    pub fn of(workspace: &WorkspaceInfo) -> Self {
        if workspace.kind == WorkspaceKind::Home {
            Self::Global
        } else {
            Self::Workspace(workspace.id)
        }
    }
}

/// Which scope the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The Home assistant.
    Global,
    /// The assistant of the active workspace (the Home one while Home is active).
    Active,
    /// A workspace other than the active one, opened from a history hit.
    Pinned(WorkspaceId),
}

/// Why a scope has no assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The daemon has no `[assistant] provider` (the daemon's own words).
    Unconfigured(String),
    /// The daemon does not speak protocol 0.12.
    Unsupported,
    /// Anything else the daemon or the connection answered.
    Other(String),
}

/// Where opening a scope's assistant stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Idle,
    Opening,
    Ready(SessionId),
    Failed(Failure),
}

/// One of the two scope buttons of the header.
struct ScopeButton {
    id: &'static str,
    label: String,
    selected: bool,
    enabled: bool,
    choose: Choice,
}

struct Chat {
    model: Entity<ChatModel>,
    view: Entity<ChatView>,
}

pub struct AssistantPanel {
    env: PanelEnv,
    choice: Choice,
    scopes: HashMap<ScopeKey, Opened>,
    chats: HashMap<SessionId, Chat>,
    /// Context to attach once a scope's assistant is open ("Send to assistant" of a block).
    waiting: Vec<(ScopeKey, ContextChip)>,
    /// Put the keyboard in the composer as soon as it is drawn.
    focus_wanted: bool,
    was_connected: bool,
    _subscription: Subscription,
}

impl std::fmt::Debug for AssistantPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantPanel")
            .field("choice", &self.choice)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl AssistantPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&env.sessions, |this, _, cx| this.on_sessions(cx));
        let connected = env.sessions.read(cx).connection_state() == ConnectionState::Connected;
        Self {
            env,
            choice: Choice::Active,
            scopes: HashMap::new(),
            chats: HashMap::new(),
            waiting: Vec::new(),
            focus_wanted: false,
            was_connected: connected,
            _subscription: subscription,
        }
    }

    // ---- state ---------------------------------------------------------------------------

    /// The active workspace when it is a project (the Home workspace has the global scope).
    fn active_project(&self, cx: &App) -> Option<WorkspaceInfo> {
        self.env
            .sessions
            .read(cx)
            .active_workspace()
            .filter(|w| w.kind != WorkspaceKind::Home)
            .cloned()
    }

    /// The scope the panel shows now.
    pub fn key(&self, cx: &App) -> ScopeKey {
        match self.choice {
            Choice::Global => ScopeKey::Global,
            Choice::Pinned(id) => ScopeKey::Workspace(id),
            Choice::Active => self
                .active_project(cx)
                .map_or(ScopeKey::Global, |w| ScopeKey::Workspace(w.id)),
        }
    }

    pub fn choice(&self) -> Choice {
        self.choice
    }

    /// How opening `key`'s assistant stands.
    pub fn state(&self, key: ScopeKey) -> Opened {
        self.scopes.get(&key).cloned().unwrap_or(Opened::Idle)
    }

    /// The session of the scope the panel shows, once it is open.
    pub fn session(&self, cx: &App) -> Option<SessionId> {
        match self.state(self.key(cx)) {
            Opened::Ready(session) => Some(session),
            _ => None,
        }
    }

    /// The session `key`'s scope has, whether or not it is the one shown.
    pub fn session_of(&self, key: ScopeKey) -> Option<SessionId> {
        match self.state(key) {
            Opened::Ready(session) => Some(session),
            _ => None,
        }
    }

    /// The chat view of `session`, once the panel has opened it.
    pub fn chat_view(&self, session: SessionId) -> Option<Entity<ChatView>> {
        self.chats.get(&session).map(|chat| chat.view.clone())
    }

    /// The chat model of `session`, once the panel has opened it.
    pub fn chat_model(&self, session: SessionId) -> Option<Entity<ChatModel>> {
        self.chats.get(&session).map(|chat| chat.model.clone())
    }

    fn connected(&self, cx: &App) -> bool {
        self.env.sessions.read(cx).connection_state() == ConnectionState::Connected
    }

    /// Reads what the store says: a reconnect tries again what failed, a daemon that forgot a
    /// session (another epoch without it) opens that scope again.
    fn on_sessions(&mut self, cx: &mut Context<Self>) {
        let (connected, synced) = {
            let store = self.env.sessions.read(cx);
            (
                store.connection_state() == ConnectionState::Connected,
                store.synced(),
            )
        };
        if connected && !self.was_connected {
            self.scopes
                .retain(|_, state| matches!(state, Opened::Ready(_)));
        }
        if synced {
            let store = self.env.sessions.read(cx);
            self.scopes.retain(|_, state| match state {
                Opened::Ready(session) => store.session(*session).is_some(),
                _ => true,
            });
        }
        self.was_connected = connected;
        cx.notify();
    }

    // ---- opening -------------------------------------------------------------------------

    /// Asks the daemon for the shown scope's assistant when that has not been asked yet.
    pub fn ensure_open(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        if self.state(key) == Opened::Idle {
            self.open(key, cx);
        }
    }

    fn open(&mut self, key: ScopeKey, cx: &mut Context<Self>) {
        let (connection, synced) = {
            let store = self.env.sessions.read(cx);
            (store.connection().clone(), store.synced())
        };
        if !self.connected(cx) || !synced {
            return;
        }
        if !connection.supports_assistant() {
            self.scopes
                .insert(key, Opened::Failed(Failure::Unsupported));
            return;
        }
        self.scopes.insert(key, Opened::Opening);
        let request = match key {
            ScopeKey::Global => AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            },
            ScopeKey::Workspace(id) => AssistantOpen {
                scope: AssistantScope::Workspace,
                workspace_id: Some(id),
                new: false,
            },
        };
        cx.spawn(async move |this, cx| {
            let result = connection.assistant_open(request).await;
            this.update(cx, |this, cx| this.opened(key, result, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn opened(
        &mut self,
        key: ScopeKey,
        result: Result<hutan_protocol::messages::SessionInfo, ClientError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(info) => {
                self.ensure_chat(info.id, cx);
                self.scopes.insert(key, Opened::Ready(info.id));
                self.flush(key, info.id, cx);
            }
            Err(ClientError::Remote(body)) if body.code == ErrorCode::InvalidState => {
                self.scopes
                    .insert(key, Opened::Failed(Failure::Unconfigured(body.message)));
                self.waiting.retain(|(waiting, _)| *waiting != key);
            }
            Err(error) => {
                self.scopes
                    .insert(key, Opened::Failed(Failure::Other(error.to_string())));
                self.waiting.retain(|(waiting, _)| *waiting != key);
            }
        }
        cx.notify();
    }

    fn ensure_chat(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.chats.contains_key(&session) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let model = cx.new(|cx| ChatModel::new(session, connection, cx));
        let (sessions, settings, regions) = (
            self.env.sessions.clone(),
            self.env.settings.clone(),
            self.env.regions.clone(),
        );
        let shown = model.clone();
        let view = cx.new(|cx| ChatView::new(session, shown, sessions, settings, regions, cx));
        self.chats.insert(session, Chat { model, view });
    }

    fn flush(&mut self, key: ScopeKey, session: SessionId, cx: &mut Context<Self>) {
        let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|(waiting, _)| *waiting == key);
        self.waiting = rest;
        if let Some(chat) = self.chats.get(&session) {
            for (_, chip) in ready {
                chat.model
                    .update(cx, |model, cx| model.attach_context(chip, cx));
            }
        }
    }

    /// Shows `choice`'s assistant (opening it on first use).
    pub fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.choice = choice;
        self.ensure_open(cx);
        cx.notify();
    }

    /// Tries again what failed for the shown scope.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        if matches!(self.state(key), Opened::Failed(_)) {
            self.scopes.remove(&key);
        }
        self.ensure_open(cx);
        cx.notify();
    }

    /// Shows the assistant `session` (a hit of the history index, say) in its own scope. `false`
    /// when it is not an assistant session this window knows.
    pub fn show_session(&mut self, session: SessionId, cx: &mut Context<Self>) -> bool {
        let found = {
            let store = self.env.sessions.read(cx);
            store
                .session(session)
                .filter(|info| {
                    info.chat
                        .as_ref()
                        .is_some_and(|chat| chat.agent == AGENT_ASSISTANT)
                })
                .and_then(|info| store.workspace(info.workspace_id))
                .map(|workspace| {
                    let key = ScopeKey::of(workspace);
                    let choice = match key {
                        ScopeKey::Global => Choice::Global,
                        ScopeKey::Workspace(id) if store.active_workspace_id() == Some(id) => {
                            Choice::Active
                        }
                        ScopeKey::Workspace(id) => Choice::Pinned(id),
                    };
                    (key, choice)
                })
        };
        let Some((key, choice)) = found else {
            return false;
        };
        self.ensure_chat(session, cx);
        self.scopes.insert(key, Opened::Ready(session));
        self.choice = choice;
        cx.notify();
        true
    }

    /// Attaches `chip` to the composer of the assistant `session` and shows it.
    pub fn attach_to_session(
        &mut self,
        session: SessionId,
        chip: ContextChip,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.show_session(session, cx) {
            return false;
        }
        match self.chats.get(&session) {
            Some(chat) => {
                chat.model
                    .update(cx, |model, cx| model.attach_context(chip, cx));
                true
            }
            None => false,
        }
    }

    /// Attaches `chip` to the composer of `key`'s assistant and shows it, opening the assistant
    /// first when need be. The chip waits for the answer of `assistant.open`.
    pub fn attach_to_scope(&mut self, key: ScopeKey, chip: ContextChip, cx: &mut Context<Self>) {
        self.choice = match key {
            ScopeKey::Global => Choice::Global,
            ScopeKey::Workspace(id)
                if self.env.sessions.read(cx).active_workspace_id() == Some(id) =>
            {
                Choice::Active
            }
            ScopeKey::Workspace(id) => Choice::Pinned(id),
        };
        match self.state(key) {
            Opened::Ready(session) => {
                if let Some(chat) = self.chats.get(&session) {
                    chat.model
                        .update(cx, |model, cx| model.attach_context(chip, cx));
                }
            }
            _ => {
                self.waiting.push((key, chip));
                if matches!(self.state(key), Opened::Failed(_)) {
                    self.scopes.remove(&key);
                }
                self.open(key, cx);
            }
        }
        cx.notify();
    }

    /// Puts the keyboard in the composer of the shown assistant (as soon as it is drawn).
    pub fn focus_composer(&mut self, cx: &mut Context<Self>) {
        self.focus_wanted = true;
        cx.notify();
    }

    // ---- rendering -----------------------------------------------------------------------

    fn scope_button(&self, scope: ScopeButton, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let ScopeButton {
            id,
            label,
            selected,
            enabled,
            choose,
        } = scope;
        button(
            format!("assistant-{id}"),
            label,
            if selected {
                ButtonKind::Primary
            } else {
                ButtonKind::Plain
            },
            enabled,
            look,
        )
        .when(enabled, |b| {
            b.on_click(cx.listener(move |this, _, _, cx| this.choose(choose, cx)))
        })
        .child(probe(&self.env.regions, Region::Assistant(id)))
        .into_any_element()
    }

    fn render_header(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let key = self.key(cx);
        let project = self.active_project(cx);
        let workspace_label = match (&self.choice, &project, key) {
            (Choice::Pinned(id), _, _) => self.env.sessions.read(cx).workspace(*id).map_or_else(
                || "Workspace".to_owned(),
                |w| format!("Workspace · {}", w.name),
            ),
            (_, Some(project), _) => format!("Workspace · {}", project.name),
            _ => "Workspace".to_owned(),
        };
        let pinned = matches!(self.choice, Choice::Pinned(_));
        let workspace_selected = matches!(key, ScopeKey::Workspace(_));
        let workspace_choice = if pinned { self.choice } else { Choice::Active };
        let scope_note = match key {
            ScopeKey::Global => "Sees the Home folder only".to_owned(),
            ScopeKey::Workspace(id) => {
                let store = self.env.sessions.read(cx);
                store.workspace(id).map_or_else(String::new, |w| {
                    let roots: Vec<String> =
                        w.roots.iter().map(|r| r.display().to_string()).collect();
                    format!("Sees {}", roots.join(", "))
                })
            }
        };
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
            .child(self.scope_button(
                ScopeButton {
                    id: "global",
                    label: "Global".to_owned(),
                    selected: !workspace_selected,
                    enabled: true,
                    choose: Choice::Global,
                },
                look,
                cx,
            ))
            .child(self.scope_button(
                ScopeButton {
                    id: "workspace",
                    label: workspace_label,
                    selected: workspace_selected,
                    enabled: project.is_some() || pinned,
                    choose: workspace_choice,
                },
                look,
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted)
                    .text_size(px(tokens.section_font_size + 1.0))
                    .child(SharedString::from(scope_note)),
            )
            .into_any_element()
    }

    fn notice(
        &self,
        title: &str,
        detail: Vec<String>,
        buttons: Vec<AnyElement>,
        look: &Look,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 4.0))
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(title.to_owned())),
            )
            .children(detail.into_iter().map(|line| {
                div()
                    .max_w(px(520.0))
                    .text_color(theme.muted)
                    .child(SharedString::from(line))
            }))
            .child(div().flex().gap(px(tokens.spacing * 2.0)).children(buttons))
            .into_any_element()
    }

    fn render_body(&mut self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let key = self.key(cx);
        let retry = button("assistant-retry", "Retry", ButtonKind::Plain, true, look)
            .on_click(cx.listener(|this, _, _, cx| this.retry(cx)))
            .child(probe(&self.env.regions, Region::Assistant("retry")))
            .into_any_element();
        let models = button(
            "assistant-models",
            "Open Models settings…",
            ButtonKind::Primary,
            true,
            look,
        )
        .on_click(|_, window, cx| window.dispatch_action(Box::new(ManageProviders), cx))
        .child(probe(&self.env.regions, Region::Assistant("models")))
        .into_any_element();
        if !self.connected(cx) {
            return self.notice(
                "Not connected to hutand",
                vec!["The assistant opens as soon as the daemon is back.".to_owned()],
                Vec::new(),
                look,
            );
        }
        match self.state(key) {
            Opened::Idle | Opened::Opening => {
                self.notice("Opening the assistant…", Vec::new(), Vec::new(), look)
            }
            Opened::Ready(session) => match self.chats.get(&session) {
                Some(chat) => div()
                    .size_full()
                    .child(chat.view.clone())
                    .into_any_element(),
                None => self.notice("Opening the assistant…", Vec::new(), Vec::new(), look),
            },
            Opened::Failed(Failure::Unconfigured(message)) => self.notice(
                "The assistant has no provider yet",
                vec![
                    message,
                    "Add a provider (and its key) in Models, then name it under [assistant] \
                     (`provider`, and `model` if it should not use the provider's default) in \
                     the daemon's daemon.toml and restart hutand. Hutan never picks a \
                     provider or model for you, and nothing is sent anywhere until you do."
                        .to_owned(),
                ],
                vec![models, retry],
                look,
            ),
            Opened::Failed(Failure::Unsupported) => self.notice(
                "This hutand has no assistant",
                vec![
                    "Restart the daemon (Daemon → Restart) to run the version of this app."
                        .to_owned(),
                ],
                vec![retry],
                look,
            ),
            Opened::Failed(Failure::Other(error)) => self.notice(
                "The assistant could not be opened",
                vec![error],
                vec![models, retry],
                look,
            ),
        }
    }
}

impl Render for AssistantPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_open(cx);
        if self.focus_wanted
            && let Some(chat) = self.session(cx).and_then(|s| self.chats.get(&s))
        {
            self.focus_wanted = false;
            let handle = chat.view.focus_handle(cx);
            window.focus(&handle, cx);
        }
        let look = Look::of(cx);
        let header = self.render_header(&look, cx);
        let body = self.render_body(&look, cx);
        let (theme, tokens) = (look.theme, look.tokens);
        div()
            .id("assistant-panel")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_color(theme.fg)
            .text_size(px(tokens.font_size))
            .child(header)
            .child(div().relative().flex_1().min_h_0().child(body))
    }
}
