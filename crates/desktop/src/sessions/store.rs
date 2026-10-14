//! [`SessionsStore`]: the desktop's view of the daemon's sessions, as a GPUI entity.
//!
//! Bridges the [`Connection`]'s `state()` watch and `notices()` broadcast (Tokio types that
//! work on any executor) onto GPUI's executor, tracks attention, hands out one
//! [`TerminalSession`] per terminal, and runs the session lifecycle commands.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Entity, Task, WeakEntity};
use heeler_client::{Connection, ConnectionState, Notice, StoreState};
use heeler_core::{SessionId, TerminalId, WorkspaceId};
use heeler_protocol::messages::{
    RecentProject, RecentProjects, Request, SessionCreate, SessionInfo, SessionRef, SessionRename,
    SessionResult, SessionSpec, TerminalSpec, WorkspaceInfo, WorkspaceKind, WorkspaceMove,
    WorkspaceOpen, WorkspaceOpened, WorkspacePin, WorkspaceRecent, WorkspaceRef, WorkspaceRename,
    WorkspaceRoot,
};
use heeler_terminal_view::DefaultColors;
use tokio::sync::broadcast::error::RecvError;

use super::attention::{AttentionTracker, Presentation, Unread, present};
use super::terminal::TerminalSession;

/// Notifications that arrive this soon after (re)connecting are replays of what happened before
/// the desktop was there; they must not flag attention.
pub const REPLAY_WINDOW: Duration = Duration::from_millis(400);

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
    _tasks: Vec<Task<()>>,
}

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
            _tasks: vec![state_task, notice_task],
        }
    }

    fn apply_state(&mut self, latest: Arc<StoreState>, cx: &mut Context<Self>) {
        let was_connected = self.state.connection == ConnectionState::Connected;
        let connected = latest.connection == ConnectionState::Connected;
        if connected && !was_connected {
            self.connected_since = Some(Instant::now());
            heeler_terminal_view::trace::startup("connected");
        } else if !connected {
            // Notices that arrive while (re)connecting replay what happened meanwhile.
            self.connected_since = None;
        }
        self.state = latest;
        if connected {
            self.attention
                .observe(self.state.epoch, &self.state.sessions);
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
        cx.notify();
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
            // Exits are read off the session state; bells are not attention in M2.
            Notice::SessionExited { .. } | Notice::Bell { .. } | Notice::DaemonRestarted { .. } => {
            }
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

    pub fn presentation(&self, info: &SessionInfo) -> Presentation {
        present(info, self.attention.unread(info.id))
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

    /// The session that had focus most recently (still the focused one while it has focus).
    pub fn last_focused(&self) -> Option<SessionId> {
        self.focused.or(self.last_focused)
    }

    // ---- attention -------------------------------------------------------------------------

    /// The session shown in the focused item changed (`None`: no session has focus).
    pub fn set_focused(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        if self.focused != session {
            self.focused = session;
            if session.is_some() {
                self.last_focused = session;
            }
            cx.notify();
        }
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

    /// Starts a login shell in `workspace` (the daemon's default when `None`) at `cwd`.
    pub fn create_terminal(
        &mut self,
        workspace: Option<WorkspaceId>,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.create_session(
            workspace,
            TerminalSpec {
                cwd,
                ..TerminalSpec::default()
            },
            cx,
        )
    }

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
