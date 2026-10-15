//! [`SessionsStore`]: the desktop's view of the daemon's sessions, as a GPUI entity.
//!
//! Bridges the [`Connection`]'s `state()` watch and `notices()` broadcast (Tokio types that
//! work on any executor) onto GPUI's executor, tracks attention, hands out one
//! [`TerminalSession`] per terminal, and runs the session lifecycle commands.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity};
use hutan_client::{Connection, ConnectionState, Notice, StoreState};
use hutan_core::agent::ModelInfo;
use hutan_core::{AgentObservation, ApprovalId, Epoch, SessionId, TerminalId, WorkspaceId};
use hutan_protocol::chat::{ApprovalInfo, ChatCreate};
use hutan_protocol::messages::{
    RecentProject, RecentProjects, Request, SessionCreate, SessionInfo, SessionRef, SessionRename,
    SessionResult, SessionSpec, TerminalSpec, WorkspaceInfo, WorkspaceKind, WorkspaceMove,
    WorkspaceOpen, WorkspaceOpened, WorkspacePin, WorkspaceRecent, WorkspaceRef, WorkspaceRename,
    WorkspaceRoot,
};
use hutan_protocol::worktree::{
    WorktreeCreate, WorktreeInfo, WorktreeList, WorktreeListRequest, WorktreeRemove,
    WorktreeRemoved,
};
use hutan_terminal_view::DefaultColors;
use tokio::sync::broadcast::error::RecvError;

use super::agents::{self, AgentAttentionChanged, AgentBadge};
use super::attention::{
    AttentionEntry, AttentionKind, AttentionTracker, Presentation, Unread, classify, present,
};
use super::terminal::TerminalSession;

/// Notifications that arrive this soon after (re)connecting are replays of what happened before
/// the desktop was there; they must not flag attention.
pub const REPLAY_WINDOW: Duration = Duration::from_millis(400);

/// A `ui.open` older than this many milliseconds when it reaches the desktop is a replay of
/// something the user did a while ago, not a request to move the window now.
pub const UI_OPEN_MAX_AGE_MS: u64 = 15_000;

/// Emitted by [`SessionsStore`] when the daemon asks this window to show a session (the
/// "Open" action of a notification intent, `ui.open`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiOpenRequested {
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
}

/// Whether a `ui.open` sent at `at_ms` is still current at `now_ms` (both Unix milliseconds on
/// this machine; a stamp from the future counts as current).
fn ui_open_is_fresh(at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(at_ms) <= UI_OPEN_MAX_AGE_MS
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub struct SessionsStore {
    connection: Connection,
    state: Arc<StoreState>,
    attention: AttentionTracker,
    focused: Option<SessionId>,
    /// The session that had focus most recently; the inspector keeps showing it while a dock or
    /// panel has focus (clicking inside the inspector must not empty it).
    last_focused: Option<SessionId>,
    /// The workspace the workbench shows (its layout, settings and new sessions).
    active_workspace: Option<WorkspaceId>,
    terminals: HashMap<SessionId, WeakEntity<TerminalSession>>,
    /// Sessions this desktop created or changed whose events have not reached `state` yet.
    recent: HashMap<SessionId, SessionInfo>,
    colors: Option<DefaultColors>,
    connected_since: Option<Instant>,
    /// The user stopped the daemon from the app; attached terminals say so.
    daemon_stopped: bool,
    /// Permission requests this window answered, and with which option (the cards say
    /// "answered here" for them, "answered elsewhere" for the rest).
    answered: HashMap<ApprovalId, String>,
    /// The blocked nodes of each session as of the last synchronised state, once the first
    /// state of the daemon epoch has been taken as the baseline (what was already blocked when
    /// the desktop arrived is not news).
    blocked_seen: Option<HashMap<SessionId, BTreeSet<String>>>,
    blocked_epoch: Option<Epoch>,
    /// The focus the daemon last acknowledged for this connection (`client.focus`); `None`
    /// when nothing was reported on the current connection.
    /// With the [`Connection::generation`] it was acknowledged on.
    focus_sent: Option<(u64, Option<SessionId>)>,
    /// The `client.focus` request in flight; at most one, so reports cannot overtake each other.
    /// With the generation it was started on: a request that outlives its connection is
    /// cancelled, not waited for.
    focus_task: Option<(u64, Task<()>)>,
    /// The session of the Assistant panel while the panel has the focus.
    assistant_focused: Option<SessionId>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<AgentAttentionChanged> for SessionsStore {}
impl EventEmitter<UiOpenRequested> for SessionsStore {}

impl std::fmt::Debug for SessionsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionsStore")
            .field("connection", &self.state.connection)
            .field("sessions", &self.state.sessions.len())
            .finish_non_exhaustive()
    }
}

impl SessionsStore {
    pub fn new(connection: Connection, cx: &mut Context<Self>) -> Self {
        let mut state = connection.state();
        let mut notices = connection.notices();
        let state_task = cx.spawn(async move |this, cx| {
            loop {
                let latest = state.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| this.apply_state(latest, cx))
                    .is_err()
                {
                    break;
                }
                if state.changed().await.is_err() {
                    break;
                }
            }
        });
        let notice_task = cx.spawn(async move |this, cx| {
            loop {
                match notices.recv().await {
                    Ok(notice) => {
                        if this
                            .update(cx, |this, cx| this.apply_notice(&notice, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(missed)) => {
                        tracing::debug!("missed {missed} daemon notices");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
        Self {
            connection,
            state: Arc::new(StoreState {
                connection: ConnectionState::Connecting,
                epoch: None,
                workspaces: Vec::new(),
                sessions: Vec::new(),
                approvals: Vec::new(),
                trees: Default::default(),
            }),
            attention: AttentionTracker::new(),
            focused: None,
            last_focused: None,
            active_workspace: None,
            terminals: HashMap::new(),
            recent: HashMap::new(),
            colors: None,
            connected_since: None,
            daemon_stopped: false,
            answered: HashMap::new(),
            blocked_seen: None,
            blocked_epoch: None,
            focus_sent: None,
            focus_task: None,
            assistant_focused: None,
            _tasks: vec![state_task, notice_task],
        }
    }

    fn apply_state(&mut self, latest: Arc<StoreState>, cx: &mut Context<Self>) {
        let was_connected = self.state.connection == ConnectionState::Connected;
        let connected = latest.connection == ConnectionState::Connected;
        if connected && !was_connected {
            self.connected_since = Some(Instant::now());
            hutan_terminal_view::trace::startup("connected");
        } else if !connected {
            // Notices that arrive while (re)connecting replay what happened meanwhile.
            self.connected_since = None;
        }
        self.state = latest;
        if connected {
            self.attention
                .observe(self.state.epoch, &self.state.sessions);
            self.observe_blocked_nodes(cx);
            let known: Vec<SessionId> = self.state.sessions.iter().map(|s| s.id).collect();
            self.recent.retain(|id, _| !known.contains(id));
        }
        // Tell every attached terminal what the daemon now says about its session. While
        // disconnected the last known state stays (a reconnect resyncs it).
        if connected {
            let live: Vec<(SessionId, WeakEntity<TerminalSession>)> = self
                .terminals
                .iter()
                .map(|(id, t)| (*id, t.clone()))
                .collect();
            for (id, terminal) in live {
                let info = self.session(id).cloned();
                match terminal.upgrade() {
                    Some(terminal) => {
                        terminal.update(cx, |terminal, cx| terminal.update_info(info, cx));
                    }
                    None => {
                        self.terminals.remove(&id);
                    }
                }
            }
        }
        // The daemon forgets a connection's focus with the connection: say it again on the new
        // one.
        if connected {
            self.report_focus(cx);
        } else {
            self.focus_sent = None;
            // The request, if any, was for the connection that is gone.
            self.focus_task = None;
        }
        cx.notify();
    }

    /// Emits [`AgentAttentionChanged`] for every session in which a node became blocked since
    /// the last state. The first state of an epoch is the baseline.
    fn observe_blocked_nodes(&mut self, cx: &mut Context<Self>) {
        if self.blocked_epoch != self.state.epoch {
            self.blocked_epoch = self.state.epoch;
            self.blocked_seen = None;
        }
        let now: HashMap<SessionId, Vec<String>> = self
            .state
            .trees
            .iter()
            .filter_map(|(session, tree)| {
                let blocked: Vec<String> = agents::blocked_nodes(tree)
                    .into_iter()
                    .map(|n| n.node_id)
                    .collect();
                (!blocked.is_empty()).then_some((*session, blocked))
            })
            .collect();
        if let Some(before) = &self.blocked_seen {
            let mut sessions: Vec<SessionId> = now.keys().copied().collect();
            sessions.sort();
            for session in sessions {
                let blocked = &now[&session];
                let known = before.get(&session);
                if blocked
                    .iter()
                    .any(|id| !known.is_some_and(|k| k.contains(id)))
                {
                    cx.emit(AgentAttentionChanged {
                        session_id: session,
                        blocked_nodes: blocked.clone(),
                    });
                }
            }
        }
        self.blocked_seen = Some(
            now.into_iter()
                .map(|(session, ids)| (session, ids.into_iter().collect()))
                .collect(),
        );
    }

    fn apply_notice(&mut self, notice: &Notice, cx: &mut Context<Self>) {
        match notice {
            Notice::TerminalNotification { terminal, .. } => {
                // Right after (re)connecting the daemon replays what was missed; that is history,
                // not news. Before the connection is seen as up there is nothing to attribute.
                let replay = self
                    .connected_since
                    .is_none_or(|since| since.elapsed() < REPLAY_WINDOW);
                if replay {
                    return;
                }
                if let Some(session) = self.session_of(*terminal) {
                    self.attention.notified(session);
                    cx.notify();
                }
            }
            // Blocks go to the attachment that shows the terminal, if there is one.
            Notice::TerminalBlock { terminal, block } => {
                let attached: Vec<_> = self
                    .terminals
                    .values()
                    .filter_map(WeakEntity::upgrade)
                    .collect();
                for session in attached {
                    session.update(cx, |session, cx| {
                        session.apply_block_event(*terminal, block.clone(), cx);
                    });
                }
            }
            // A notification's "Open" (`ui.open`): this window shows the session.
            Notice::UiOpen {
                session,
                workspace,
                at_ms,
            } => {
                if ui_open_is_fresh(*at_ms, unix_ms()) {
                    cx.emit(UiOpenRequested {
                        session_id: *session,
                        workspace_id: *workspace,
                    });
                }
            }
            // Intents are for notifiers (M6-B) and `hutan notify watch`; the desktop shows
            // approvals and agent state from its own state.
            Notice::SessionExited { .. }
            | Notice::Bell { .. }
            | Notice::DaemonRestarted { .. }
            | Notice::NotifyIntent { .. }
            // The desktop lists worktrees when a command asks for them.
            | Notice::WorktreesChanged { .. }
            // The tasks panel is M7-C's.
            | Notice::TaskUpdated { .. }
            | Notice::TaskRemoved { .. } => {}
        }
    }

    // ---- read access -----------------------------------------------------------------------

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn state(&self) -> &StoreState {
        &self.state
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.state.connection
    }

    /// Whether the first synchronisation with the daemon has completed.
    pub fn synced(&self) -> bool {
        self.state.epoch.is_some()
    }

    pub fn sessions(&self) -> &[SessionInfo] {
        &self.state.sessions
    }

    pub fn workspaces(&self) -> &[WorkspaceInfo] {
        &self.state.workspaces
    }

    pub fn session(&self, id: SessionId) -> Option<&SessionInfo> {
        self.state
            .sessions
            .iter()
            .find(|s| s.id == id)
            .or_else(|| self.recent.get(&id))
    }

    pub fn workspace(&self, id: WorkspaceId) -> Option<&WorkspaceInfo> {
        self.state.workspaces.iter().find(|w| w.id == id)
    }

    /// The Home workspace (`kind == home`); the daemon keeps exactly one.
    pub fn home_workspace(&self) -> Option<&WorkspaceInfo> {
        self.state
            .workspaces
            .iter()
            .find(|w| w.kind == WorkspaceKind::Home)
    }

    /// The workspace the workbench shows, when the daemon knows it.
    pub fn active_workspace(&self) -> Option<&WorkspaceInfo> {
        self.active_workspace.and_then(|id| self.workspace(id))
    }

    /// The id of the workspace the workbench shows, known or not.
    pub fn active_workspace_id(&self) -> Option<WorkspaceId> {
        self.active_workspace
    }

    /// Tells the panels which workspace the workbench shows.
    pub fn set_active_workspace(&mut self, workspace: Option<WorkspaceId>, cx: &mut Context<Self>) {
        if self.active_workspace != workspace {
            self.active_workspace = workspace;
            cx.notify();
        }
    }

    /// How many sessions of each workspace need the user.
    pub fn needs_you_by_workspace(&self) -> HashMap<WorkspaceId, usize> {
        let mut counts = HashMap::new();
        for session in self
            .state
            .sessions
            .iter()
            .filter(|s| self.presentation(s).needs_you)
        {
            *counts.entry(session.workspace_id).or_insert(0) += 1;
        }
        counts
    }

    pub fn session_of(&self, terminal: TerminalId) -> Option<SessionId> {
        self.state
            .sessions
            .iter()
            .find(|s| s.terminal_id == Some(terminal))
            .map(|s| s.id)
    }

    pub fn unread(&self, id: SessionId) -> Option<Unread> {
        self.attention.unread(id)
    }

    /// How `info` shows up: its own state, except that a live session in which an agent of the
    /// observation tree is blocked always needs the user, whatever the session summary says.
    pub fn presentation(&self, info: &SessionInfo) -> Presentation {
        let presentation = present(info, self.attention.unread(info.id));
        if !presentation.needs_you
            && info.state.is_live()
            && let Some(tree) = self.state.trees.get(&info.id)
        {
            let blocked = agents::blocked_nodes(tree);
            if !blocked.is_empty() {
                return agents::blocked_presentation(&blocked);
            }
        }
        presentation
    }

    /// The observation tree of `session` (capability `agent.tree`), when it has agents.
    pub fn agent_tree(&self, session: SessionId) -> Option<&AgentObservation> {
        self.state.trees.get(&session)
    }

    /// Every observation tree, by session.
    pub fn agent_trees(&self) -> impl Iterator<Item = &AgentObservation> {
        self.state.trees.values()
    }

    /// What the row, tab or card of `session` says about its agents (empty without a tree).
    pub fn agent_badge(&self, session: SessionId) -> AgentBadge {
        self.state
            .trees
            .get(&session)
            .map(agents::badge)
            .unwrap_or_default()
    }

    /// Sessions that need the user, in daemon order.
    pub fn needs_you(&self) -> Vec<SessionId> {
        self.state
            .sessions
            .iter()
            .filter(|s| self.presentation(s).needs_you)
            .map(|s| s.id)
            .collect()
    }

    /// The sessions that need the user for the menu bar item and the Dock badge: blocked
    /// agents, pending approvals and unread failures, in daemon order. See [`classify`].
    pub fn attention_entries(&self) -> Vec<AttentionEntry> {
        let mut pending: HashMap<SessionId, usize> = HashMap::new();
        for approval in &self.state.approvals {
            *pending.entry(approval.session_id).or_default() += 1;
        }
        self.state
            .sessions
            .iter()
            .filter_map(|info| {
                let presentation = self.presentation(info);
                let approvals = pending.get(&info.id).copied().unwrap_or(0);
                let kind = classify(&presentation, approvals)?;
                let detail = match kind {
                    AttentionKind::Approval if approvals == 1 => String::new(),
                    AttentionKind::Approval => format!("{approvals} requests"),
                    AttentionKind::Blocked | AttentionKind::Failed => presentation.detail,
                };
                Some(AttentionEntry {
                    session: info.id,
                    title: info.title.clone(),
                    kind,
                    detail,
                })
            })
            .collect()
    }

    /// The session after `current` that needs the user (wrapping).
    pub fn next_needing_attention(&self, current: Option<SessionId>) -> Option<SessionId> {
        let needing = self.needs_you();
        let order: Vec<SessionId> = self.state.sessions.iter().map(|s| s.id).collect();
        let start = current
            .and_then(|c| order.iter().position(|id| *id == c))
            .map_or(0, |i| i + 1);
        (0..order.len())
            .map(|offset| order[(start + offset) % order.len()])
            .find(|id| needing.contains(id))
    }

    pub fn focused(&self) -> Option<SessionId> {
        self.focused
    }

    // ---- approvals -------------------------------------------------------------------------

    /// Permission requests of every agent chat waiting for an answer, oldest first.
    pub fn approvals(&self) -> &[ApprovalInfo] {
        &self.state.approvals
    }

    /// The option this window chose for `approval`, if it answered it.
    pub fn answered_here(&self, approval: ApprovalId) -> Option<&str> {
        self.answered.get(&approval).map(String::as_str)
    }

    /// `approval.respond`. The answer is remembered before the daemon replies (the resolved
    /// event can overtake the reply) and forgotten again when the daemon refuses it.
    pub fn respond_approval(
        &mut self,
        approval: ApprovalId,
        option_id: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.answered.insert(approval, option_id.clone());
        cx.notify();
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let answered = connection
                .approve(approval, option_id)
                .await
                .map_err(|e| e.to_string());
            if answered.is_err() {
                this.update(cx, |this, cx| {
                    this.answered.remove(&approval);
                    cx.notify();
                })
                .ok();
            }
            answered
        })
    }

    /// The session that had focus most recently (still the focused one while it has focus).
    pub fn last_focused(&self) -> Option<SessionId> {
        self.focused.or(self.last_focused)
    }

    /// The focus the daemon acknowledged for the current connection (`client.focus`): `None`
    /// before the first acknowledgement and after the connection was lost, `Some(None)` once
    /// "nothing is looked at" was acknowledged.
    pub fn focus_reported(&self) -> Option<Option<SessionId>> {
        let generation = self.connection.generation();
        self.focus_sent
            .filter(|(sent_on, _)| *sent_on == generation)
            .map(|(_, focus)| focus)
    }

    /// The session the user is looking at for `client.focus`: the focused item's session, else
    /// the docked Assistant's while it has the focus.
    fn looked_at(&self) -> Option<SessionId> {
        self.focused.or(self.assistant_focused)
    }

    /// The Assistant panel has (or no longer has) the focus with `session` shown. Only the
    /// daemon's idea of "looked at" changes; attention and the inspector follow
    /// [`set_focused`](Self::set_focused) alone.
    pub fn set_assistant_focused(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        if self.assistant_focused != session {
            self.assistant_focused = session;
            self.report_focus(cx);
        }
    }

    // ---- attention -------------------------------------------------------------------------

    /// The session shown in the focused item changed (`None`: no session has focus).
    pub fn set_focused(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        if self.focused != session {
            self.focused = session;
            if session.is_some() {
                self.last_focused = session;
            }
            self.report_focus(cx);
            cx.notify();
        }
    }

    /// Tells the daemon which session this window shows (`client.focus`), so that it raises no
    /// "turn finished" notification for a session the user is looking at. One request at a
    /// time; when the focus changed while it was in flight the next one follows.
    fn report_focus(&mut self, cx: &mut Context<Self>) {
        if self.state.connection != ConnectionState::Connected || !self.connection.supports_notify()
        {
            return;
        }
        let generation = self.connection.generation();
        let focused = self.looked_at();
        match &self.focus_task {
            // One request at a time: this one's answer decides whether another follows.
            Some((started_on, _)) if *started_on == generation => return,
            // It was for a connection that is gone: its answer would stamp this one with a
            // focus the daemon never got.
            Some(_) => self.focus_task = None,
            None => {}
        }
        if self.focus_sent == Some((generation, focused)) {
            return;
        }
        let connection = self.connection.clone();
        let task = cx.spawn(async move |this, cx| {
            let sent = connection.client_focus(focused).await;
            this.update(cx, |this, cx| {
                this.focus_task = None;
                match sent {
                    Ok(sent_on) => {
                        this.focus_sent = Some((sent_on, focused));
                        this.report_focus(cx);
                    }
                    Err(error) => tracing::debug!(%error, "could not report the focus"),
                }
            })
            .ok();
        });
        self.focus_task = Some((generation, task));
    }

    /// The user looked at `session`.
    pub fn mark_read(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.attention.mark_read(session) {
            cx.notify();
        }
    }

    // ---- terminals -------------------------------------------------------------------------

    /// The shared attachment of `session`'s terminal: attached on first use, detached when the
    /// last holder drops it. `None` when the session is unknown or has no terminal.
    pub fn terminal_session(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalSession>> {
        if let Some(existing) = self.terminals.get(&session).and_then(WeakEntity::upgrade) {
            return Some(existing);
        }
        let info = self.session(session)?.clone();
        let terminal = TerminalSession::attach(info, self.connection.clone(), self.colors, cx)?;
        if self.daemon_stopped {
            terminal.update(cx, |terminal, cx| terminal.set_daemon_stopped(true, cx));
        }
        self.terminals.insert(session, terminal.downgrade());
        Some(terminal)
    }

    /// Whether the user stopped the daemon from the app (it stays down until they start it).
    pub fn daemon_stopped(&self) -> bool {
        self.daemon_stopped
    }

    /// Tells every attached terminal that the daemon was stopped on purpose (or started again).
    pub fn set_daemon_stopped(&mut self, stopped: bool, cx: &mut Context<Self>) {
        if self.daemon_stopped == stopped {
            return;
        }
        self.daemon_stopped = stopped;
        for terminal in self.terminals.values().filter_map(WeakEntity::upgrade) {
            terminal.update(cx, |terminal, cx| terminal.set_daemon_stopped(stopped, cx));
        }
        cx.notify();
    }

    /// The attachment of `session`'s terminal if some view holds it right now; never attaches.
    pub fn attached(&self, session: SessionId) -> Option<Entity<TerminalSession>> {
        self.terminals.get(&session).and_then(WeakEntity::upgrade)
    }

    /// Number of terminals attached right now.
    pub fn attached_count(&self) -> usize {
        self.terminals
            .values()
            .filter(|t| t.upgrade().is_some())
            .count()
    }

    /// Tell every attached terminal the colors the terminal theme draws with, so OSC 10/11/12
    /// replies match what the user sees; terminals attached later get them on attach.
    pub fn set_terminal_colors(&mut self, colors: DefaultColors, cx: &mut Context<Self>) {
        if self.colors == Some(colors) {
            return;
        }
        self.colors = Some(colors);
        for terminal in self.terminals.values().filter_map(WeakEntity::upgrade) {
            terminal.update(cx, |terminal, _| terminal.set_colors(colors));
        }
    }

    // ---- lifecycle commands ----------------------------------------------------------------

    /// Starts the terminal session `spec` describes in `workspace` (the daemon's default when
    /// `None`).
    pub fn create_session(
        &mut self,
        workspace: Option<WorkspaceId>,
        spec: TerminalSpec,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        let request = Request::SessionCreate(SessionCreate {
            workspace_id: workspace,
            spec: SessionSpec::Terminal(spec),
        });
        self.run(request, cx)
    }

    /// `chat.create`: starts an agent conversation as a session (resolves once the agent is
    /// ready).
    pub fn create_chat(
        &mut self,
        request: ChatCreate,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let info = connection
                .chat_create(request)
                .await
                .map_err(|e| e.to_string())?;
            let remembered = info.clone();
            this.update(cx, |this, _| {
                if !this.state.sessions.iter().any(|s| s.id == remembered.id) {
                    this.recent.insert(remembered.id, remembered);
                }
            })
            .ok();
            Ok(info)
        })
    }

    /// `chat.models`: what `agent` offers.
    pub fn chat_models(
        &mut self,
        agent: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<ModelInfo>, String>> {
        let connection = self.connection.clone();
        let agent = agent.to_owned();
        cx.spawn(async move |_, _| {
            connection
                .chat_models(&agent)
                .await
                .map(|answer| answer.models)
                .map_err(|e| e.to_string())
        })
    }

    pub fn stop(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.run(
            Request::SessionStop(SessionRef {
                session_id: session,
            }),
            cx,
        )
    }

    pub fn restart(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.run(
            Request::SessionRestart(SessionRef {
                session_id: session,
            }),
            cx,
        )
    }

    pub fn rename(
        &mut self,
        session: SessionId,
        title: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.run(
            Request::SessionRename(SessionRename {
                session_id: session,
                title,
            }),
            cx,
        )
    }

    fn run(
        &mut self,
        request: Request,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let result = connection
                .call::<SessionResult>(request)
                .await
                .map_err(|e| e.to_string())?;
            let info = result.session;
            let remembered = info.clone();
            this.update(cx, |this, _| {
                if !this.state.sessions.iter().any(|s| s.id == remembered.id) {
                    this.recent.insert(remembered.id, remembered);
                }
            })
            .ok();
            Ok(info)
        })
    }

    // ---- worktree commands -----------------------------------------------------------------

    /// `worktree.list`: the worktrees of the repositories of `workspace`.
    pub fn list_worktrees(
        &self,
        workspace: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<WorktreeInfo>, String>> {
        let request = Request::WorktreeList(WorktreeListRequest {
            workspace_id: Some(workspace),
        });
        self.call(request, |list: WorktreeList| list.worktrees, cx)
    }

    /// `worktree.create`.
    pub fn create_worktree(
        &self,
        request: WorktreeCreate,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeInfo, String>> {
        self.call(
            Request::WorktreeCreate(request),
            |created: hutan_protocol::worktree::WorktreeCreated| created.worktree,
            cx,
        )
    }

    /// `worktree.remove`.
    pub fn remove_worktree(
        &self,
        request: WorktreeRemove,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeRemoved, String>> {
        self.call(Request::WorktreeRemove(request), |removed| removed, cx)
    }

    // ---- workspace commands ----------------------------------------------------------------

    /// One request on the connection, its answer decoded as `T` and mapped by `done`. The store
    /// itself follows through the events the daemon publishes for the change.
    fn call<T, R>(
        &self,
        request: Request,
        done: impl FnOnce(T) -> R + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<R, String>>
    where
        T: serde::de::DeserializeOwned + Send + 'static,
        R: Send + 'static,
    {
        let connection = self.connection.clone();
        cx.background_spawn(async move {
            connection
                .call::<T>(request)
                .await
                .map(done)
                .map_err(|e| e.to_string())
        })
    }

    /// `workspace.open`: the workspace containing `path`, created when there is none.
    pub fn open_workspace(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorkspaceOpened, String>> {
        let request = Request::WorkspaceOpen(WorkspaceOpen {
            path,
            created_by: None,
        });
        self.call(request, |opened: WorkspaceOpened| opened, cx)
    }

    /// `workspace.recent`: the open and remembered projects, newest first.
    pub fn recent_projects(
        &mut self,
        limit: u32,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<RecentProject>, String>> {
        let request = Request::WorkspaceRecent(WorkspaceRecent { limit: Some(limit) });
        self.call(request, |recent: RecentProjects| recent.projects, cx)
    }

    /// `workspace.move`: `workspace` right before `before`, or last.
    pub fn move_workspace(
        &mut self,
        workspace: WorkspaceId,
        before: Option<WorkspaceId>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceMove(WorkspaceMove {
            workspace_id: workspace,
            before,
        });
        self.call(request, |_: serde_json::Value| (), cx)
    }

    pub fn pin_workspace(
        &mut self,
        workspace: WorkspaceId,
        pinned: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspacePin(WorkspacePin {
            workspace_id: workspace,
            pinned,
        });
        self.call(request, |_: WorkspaceInfo| (), cx)
    }

    pub fn rename_workspace(
        &mut self,
        workspace: WorkspaceId,
        name: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceRename(WorkspaceRename {
            workspace_id: workspace,
            name,
        });
        self.call(request, |_: WorkspaceInfo| (), cx)
    }

    pub fn add_workspace_root(
        &mut self,
        workspace: WorkspaceId,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceAddRoot(WorkspaceRoot {
            workspace_id: workspace,
            path,
        });
        self.call(request, |_: WorkspaceInfo| (), cx)
    }

    pub fn remove_workspace_root(
        &mut self,
        workspace: WorkspaceId,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceRemoveRoot(WorkspaceRoot {
            workspace_id: workspace,
            path,
        });
        self.call(request, |_: WorkspaceInfo| (), cx)
    }

    pub fn close_workspace(
        &mut self,
        workspace: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceClose(WorkspaceRef {
            workspace_id: workspace,
        });
        self.call(request, |_: serde_json::Value| (), cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ui_open_is_current_for_fifteen_seconds() {
        assert!(ui_open_is_fresh(1_000, 1_000));
        assert!(ui_open_is_fresh(1_000, 16_000));
        assert!(!ui_open_is_fresh(1_000, 16_001));
        assert!(ui_open_is_fresh(5_000, 1_000), "a future stamp is current");
    }
}
