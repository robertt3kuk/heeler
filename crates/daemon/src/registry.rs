//! `SessionRegistry` actor: owns workspaces and terminal sessions and publishes their events.
//! Everything reaches it through one bounded mailbox; replies travel over `oneshot`s.
//!
//! The actor never waits on a child process. Stopping a session signals its terminal and parks
//! the reply until the terminal's watcher task reports the exit back through the same mailbox.
//!
//! Every change is written through to `heeler-storage` (fire-and-forget into the writer thread,
//! so request handling never waits for the disk), and [`spawn`] rebuilds the state from what was
//! stored (ADR 0009 step 1): live sessions come back as new terminals with the same ids.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::{Instant, sleep_until};

use heeler_core::{SessionEvent, SessionId, SessionKind, SessionState, TerminalId, WorkspaceId};
use heeler_protocol::control::{ErrorBody, ErrorCode};
use heeler_protocol::messages::{
    Event, SessionCreate, SessionInfo, SessionList, SessionListFilter, SessionRename,
    SessionSnapshot, SessionSpec, TerminalSpec, WorkspaceCreate, WorkspaceInfo, WorkspaceList,
    WorkspaceRename,
};
use heeler_storage::{SessionRow, Storage, StorageError, WorkspaceRow, Write};
use heeler_terminal::{ExitStatus, RenderTracker, Terminal, TerminalConfig, TerminalMeta};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, warn};

use crate::bus::{EventBus, PublishError};
use crate::config::RestoreMode;
use crate::restore::{self, DEFAULT_WORKSPACE_KEY, Stored};
use crate::terminals::{TerminalEntry, TerminalHub};

/// Commands waiting in the mailbox before senders wait their turn.
const MAILBOX_CAPACITY: usize = 256;

/// Time a terminal gets to react to SIGHUP before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

pub const DEFAULT_WORKSPACE_NAME: &str = "default";

/// Title and cwd changes are coalesced: a session is written at most this often because of them.
const DIRTY_WRITE_INTERVAL: Duration = Duration::from_secs(1);

type Reply<T> = oneshot::Sender<Result<T, ErrorBody>>;

#[derive(Debug)]
enum Command {
    CreateWorkspace(WorkspaceCreate, Reply<WorkspaceInfo>),
    ListWorkspaces(Reply<WorkspaceList>),
    RenameWorkspace(WorkspaceRename, Reply<WorkspaceInfo>),
    CloseWorkspace(WorkspaceId, Reply<()>),
    Snapshot(Reply<SessionSnapshot>),
    SessionCounts(Reply<SessionCounts>),
    CreateSession(SessionCreate, Reply<SessionInfo>),
    ListSessions(SessionListFilter, Reply<SessionList>),
    /// Answered once the session has actually stopped.
    StopSession(SessionId, Reply<SessionInfo>),
    RestartSession(SessionId, Reply<SessionInfo>),
    RenameSession(SessionRename, Reply<SessionInfo>),
    /// Answered once no session is live any more. The flag removes the sessions from the
    /// restore set (`--forget`).
    StopAll {
        forget: bool,
        reply: Reply<()>,
    },
    /// From a terminal's watcher task.
    TerminalMeta(TerminalId, TerminalMeta),
    TerminalExited(TerminalId, ExitStatus),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCounts {
    pub total: u32,
    pub live: u32,
}

/// Handle to the registry actor; cheap to clone.
#[derive(Debug, Clone)]
pub struct RegistryHandle {
    tx: mpsc::Sender<Command>,
}

impl RegistryHandle {
    async fn ask<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, ErrorBody> {
        let (reply, response) = oneshot::channel();
        let unavailable = || ErrorBody::new(ErrorCode::Internal, "session registry unavailable");
        self.tx
            .send(command(reply))
            .await
            .map_err(|_| unavailable())?;
        response.await.map_err(|_| unavailable())?
    }

    pub async fn create_workspace(&self, req: WorkspaceCreate) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::CreateWorkspace(req, r)).await
    }

    pub async fn list_workspaces(&self) -> Result<WorkspaceList, ErrorBody> {
        self.ask(Command::ListWorkspaces).await
    }

    pub async fn rename_workspace(&self, req: WorkspaceRename) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::RenameWorkspace(req, r)).await
    }

    pub async fn close_workspace(&self, id: WorkspaceId) -> Result<(), ErrorBody> {
        self.ask(|r| Command::CloseWorkspace(id, r)).await
    }

    pub async fn snapshot(&self) -> Result<SessionSnapshot, ErrorBody> {
        self.ask(Command::Snapshot).await
    }

    pub async fn session_counts(&self) -> Result<SessionCounts, ErrorBody> {
        self.ask(Command::SessionCounts).await
    }

    pub async fn create_session(&self, req: SessionCreate) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::CreateSession(req, r)).await
    }

    pub async fn list_sessions(&self, filter: SessionListFilter) -> Result<SessionList, ErrorBody> {
        self.ask(|r| Command::ListSessions(filter, r)).await
    }

    /// Hangs up the session's process (killing it after [`STOP_GRACE`]) and returns the
    /// session once it is `stopped`.
    pub async fn stop_session(&self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::StopSession(id, r)).await
    }

    pub async fn restart_session(&self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::RestartSession(id, r)).await
    }

    pub async fn rename_session(&self, req: SessionRename) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::RenameSession(req, r)).await
    }

    /// Terminates every live session concurrently and returns once all have ended.
    ///
    /// The stopped sessions stay in the restore set (the next daemon respawns them) unless
    /// `forget`, which records them as stopped.
    pub async fn stop_all(&self, forget: bool) -> Result<(), ErrorBody> {
        self.ask(|reply| Command::StopAll { forget, reply }).await
    }
}

/// One terminal session and the daemon-side state around it.
#[derive(Debug)]
struct SessionEntry {
    info: SessionInfo,
    /// What a restart spawns again.
    spec: TerminalSpec,
    default_title: String,
    /// The user named the session (at creation or by rename); terminal titles no longer rename it.
    title_locked: bool,
    /// Last title the program set through OSC 0/2.
    osc_title: Option<String>,
    /// Kept after the process ends so clients can still read the final screen.
    terminal: Option<Arc<TerminalEntry>>,
    /// A stop was requested; the next exit counts as `stopped`.
    stopping: bool,
    stop_waiters: Vec<Reply<SessionInfo>>,
    ended_at_ms: Option<u64>,
    /// Native resume reference (JSON) recorded by an agent integration; kept as stored.
    resume_ref_json: Option<String>,
    /// Command to run instead of the spec's at the next spawn (a resume reference); used once.
    restore_argv: Option<Vec<String>>,
    /// The daemon is shutting down and takes this session with it: the database keeps this
    /// live state so the next daemon respawns it.
    keep_live: Option<SessionState>,
    /// Title or cwd changed since the last write.
    dirty: bool,
}

impl SessionEntry {
    fn advance(&mut self, event: SessionEvent) {
        match self.info.state.transition(event) {
            Ok(next) => {
                self.info.state = next;
                self.ended_at_ms = (!self.info.state.is_live()).then(now_ms);
            }
            Err(error) => error!(session = %self.info.id, %error, "invalid session transition"),
        }
    }

    /// Signals the process once; the exit arrives later through the watcher.
    fn request_stop(&mut self) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        if let Some(entry) = &self.terminal
            && let Err(error) = entry.terminal().terminate(STOP_GRACE)
        {
            warn!(session = %self.info.id, %error, "could not signal the terminal");
        }
    }
}

#[derive(Debug)]
struct SessionRegistry {
    bus: Arc<EventBus>,
    hub: Arc<TerminalHub>,
    /// Exported to children as `HEELER_SOCKET`.
    socket: PathBuf,
    /// Installed shell-integration scripts handed to every new terminal; `None` = no injection.
    shell_integration_dir: Option<PathBuf>,
    /// Weak so the actor stops once every [`RegistryHandle`] is gone.
    mailbox: mpsc::WeakSender<Command>,
    default_workspace: WorkspaceId,
    workspaces: Vec<WorkspaceInfo>,
    /// `position` for the next workspace row.
    next_position: i64,
    sessions: Vec<SessionEntry>,
    stop_all_waiters: Vec<Reply<()>>,
    storage: Storage,
    /// When sessions with unwritten title/cwd changes are written next.
    flush_at: Option<Instant>,
}

/// What [`spawn`] needs besides the shared services.
#[derive(Debug)]
pub struct RegistryConfig {
    /// Exported to children as `HEELER_SOCKET`.
    pub socket: PathBuf,
    /// Installed shell-integration scripts handed to every new terminal; `None` = no injection.
    pub shell_integration_dir: Option<PathBuf>,
    pub storage: Storage,
    pub restore: RestoreMode,
}

/// Starts the actor from the stored state. It stops when every [`RegistryHandle`] is dropped.
///
/// Sessions that were live when the previous daemon stopped are spawned again here (same id,
/// title, and workspace; last known cwd) unless `config.restore` is `Stopped`. Must run inside the
/// Tokio runtime.
pub fn spawn(
    bus: Arc<EventBus>,
    hub: Arc<TerminalHub>,
    config: RegistryConfig,
    stored: Stored,
) -> RegistryHandle {
    let (tx, rx) = mpsc::channel(MAILBOX_CAPACITY);
    let (workspaces, default_workspace, next_position) =
        restored_workspaces(&stored, &config.storage);
    let mut registry = SessionRegistry {
        bus,
        hub,
        socket: config.socket,
        shell_integration_dir: config.shell_integration_dir,
        mailbox: tx.downgrade(),
        default_workspace,
        workspaces,
        next_position,
        sessions: Vec::new(),
        stop_all_waiters: Vec::new(),
        storage: config.storage,
        flush_at: None,
    };
    registry.restore_sessions(stored.sessions, config.restore);
    tokio::spawn(registry.run(rx));
    RegistryHandle { tx }
}

/// The stored workspaces, the default one among them (created and stored when missing), and the
/// next free position.
fn restored_workspaces(
    stored: &Stored,
    storage: &Storage,
) -> (Vec<WorkspaceInfo>, WorkspaceId, i64) {
    let mut workspaces: Vec<WorkspaceInfo> = stored
        .workspaces
        .iter()
        .map(|w| WorkspaceInfo {
            id: w.id,
            name: w.name.clone(),
            roots: w.roots.clone(),
        })
        .collect();
    let mut next_position = stored
        .workspaces
        .iter()
        .map(|w| w.position)
        .max()
        .map_or(0, |last| last + 1);
    let existing = stored
        .default_workspace
        .filter(|id| workspaces.iter().any(|w| w.id == *id));
    let default_workspace = existing.unwrap_or_else(|| {
        let id = WorkspaceId::new();
        workspaces.push(WorkspaceInfo {
            id,
            name: DEFAULT_WORKSPACE_NAME.to_owned(),
            roots: Vec::new(),
        });
        log_write(
            storage.batch(vec![
                Write::InsertWorkspace(WorkspaceRow {
                    id,
                    name: DEFAULT_WORKSPACE_NAME.to_owned(),
                    roots: Vec::new(),
                    created_at_ms: now_ms(),
                    position: next_position,
                }),
                Write::SetMeta {
                    key: DEFAULT_WORKSPACE_KEY.to_owned(),
                    value: id.to_string(),
                },
            ]),
            "the default workspace",
        );
        next_position += 1;
        id
    });
    (workspaces, default_workspace, next_position)
}

/// Storage failures never take request handling down: the in-memory state stays authoritative
/// and the next write may succeed.
fn log_write(result: Result<(), StorageError>, what: &str) {
    match result {
        Ok(()) => {}
        // The writer is closed once the daemon has finished shutting down; nothing is lost then.
        Err(StorageError::Closed) => debug!("storage is closed; dropped a write of {what}"),
        Err(error) => error!(%error, "could not store {what}"),
    }
}

impl SessionRegistry {
    async fn run(mut self, mut mailbox: mpsc::Receiver<Command>) {
        loop {
            let command = match self.flush_at {
                Some(deadline) => tokio::select! {
                    command = mailbox.recv() => command,
                    () = sleep_until(deadline) => {
                        self.flush_dirty();
                        continue;
                    }
                },
                None => mailbox.recv().await,
            };
            let Some(command) = command else { break };
            // A dropped reply means the asking connection went away; nothing to do.
            match command {
                Command::CreateWorkspace(req, reply) => {
                    let _ = reply.send(self.create_workspace(req));
                }
                Command::ListWorkspaces(reply) => {
                    let _ = reply.send(Ok(WorkspaceList {
                        workspaces: self.workspaces.clone(),
                    }));
                }
                Command::RenameWorkspace(req, reply) => {
                    let _ = reply.send(self.rename_workspace(req));
                }
                Command::CloseWorkspace(id, reply) => {
                    let _ = reply.send(self.close_workspace(id));
                }
                Command::Snapshot(reply) => {
                    // The actor is the only publisher of the state it snapshots, so the bus's
                    // last seq is exactly the watermark of this state.
                    let _ = reply.send(Ok(SessionSnapshot {
                        workspaces: self.workspaces.clone(),
                        sessions: self.sessions.iter().map(|s| s.info.clone()).collect(),
                        seq: self.bus.last_seq(),
                    }));
                }
                Command::SessionCounts(reply) => {
                    let _ = reply.send(Ok(SessionCounts {
                        total: self.sessions.len() as u32,
                        live: self.live_count() as u32,
                    }));
                }
                Command::CreateSession(req, reply) => {
                    let _ = reply.send(self.create_session(req));
                }
                Command::ListSessions(filter, reply) => {
                    let _ = reply.send(Ok(SessionList {
                        sessions: self
                            .sessions
                            .iter()
                            .filter(|s| {
                                filter
                                    .workspace_id
                                    .is_none_or(|id| id == s.info.workspace_id)
                            })
                            .map(|s| s.info.clone())
                            .collect(),
                        seq: self.bus.last_seq(),
                    }));
                }
                Command::StopSession(id, reply) => self.stop_session(id, reply),
                Command::RestartSession(id, reply) => {
                    let _ = reply.send(self.restart_session(id));
                }
                Command::RenameSession(req, reply) => {
                    let _ = reply.send(self.rename_session(req));
                }
                Command::StopAll { forget, reply } => self.stop_all(forget, reply),
                Command::TerminalMeta(terminal, meta) => self.on_meta(terminal, meta),
                Command::TerminalExited(terminal, status) => self.on_exited(terminal, &status),
            }
        }
        self.flush_dirty();
    }

    fn live_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.info.state.is_live())
            .count()
    }

    // Workspaces ------------------------------------------------------------------------------

    fn create_workspace(&mut self, req: WorkspaceCreate) -> Result<WorkspaceInfo, ErrorBody> {
        let name = validated_name(req.name, "workspace")?;
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            name,
            roots: req.roots,
        };
        self.publish(&Event::WorkspaceUpdated {
            workspace: workspace.clone(),
        })?;
        let position = self.next_position;
        self.next_position += 1;
        log_write(
            self.storage.workspaces().insert(WorkspaceRow {
                id: workspace.id,
                name: workspace.name.clone(),
                roots: workspace.roots.clone(),
                created_at_ms: now_ms(),
                position,
            }),
            "a new workspace",
        );
        self.workspaces.push(workspace.clone());
        Ok(workspace)
    }

    fn rename_workspace(&mut self, req: WorkspaceRename) -> Result<WorkspaceInfo, ErrorBody> {
        let name = validated_name(req.name, "workspace")?;
        let index = self.index_of(req.workspace_id)?;
        let mut workspace = self.workspaces[index].clone();
        workspace.name = name;
        self.publish(&Event::WorkspaceUpdated {
            workspace: workspace.clone(),
        })?;
        self.workspaces[index] = workspace.clone();
        log_write(
            self.storage
                .workspaces()
                .rename(workspace.id, workspace.name.clone()),
            "a workspace rename",
        );
        Ok(workspace)
    }

    fn close_workspace(&mut self, id: WorkspaceId) -> Result<(), ErrorBody> {
        let index = self.index_of(id)?;
        if id == self.default_workspace {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the default workspace cannot be closed",
            ));
        }
        let live = self
            .sessions
            .iter()
            .filter(|s| s.info.workspace_id == id && s.info.state.is_live())
            .count();
        if live > 0 {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("workspace has {live} live session(s); stop them first"),
            ));
        }
        self.publish(&Event::WorkspaceClosed { workspace_id: id })?;
        self.workspaces.remove(index);
        // The sessions' rows go with the workspace's.
        log_write(self.storage.workspaces().delete(id), "a workspace removal");
        // Ended sessions go with their workspace, and so do their terminals.
        let hub = &self.hub;
        self.sessions.retain(|s| {
            let keep = s.info.workspace_id != id;
            if !keep && let Some(terminal) = &s.terminal {
                hub.remove(terminal.id());
            }
            keep
        });
        Ok(())
    }

    fn index_of(&self, id: WorkspaceId) -> Result<usize, ErrorBody> {
        self.workspaces
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("workspace {id} not found")))
    }

    // Sessions --------------------------------------------------------------------------------

    fn session_index(&self, id: SessionId) -> Result<usize, ErrorBody> {
        self.sessions
            .iter()
            .position(|s| s.info.id == id)
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("session {id} not found")))
    }

    fn terminal_index(&self, terminal: TerminalId) -> Option<usize> {
        self.sessions
            .iter()
            .position(|s| s.terminal.as_ref().is_some_and(|t| t.id() == terminal))
    }

    fn create_session(&mut self, req: SessionCreate) -> Result<SessionInfo, ErrorBody> {
        let workspace_id = req.workspace_id.unwrap_or(self.default_workspace);
        self.index_of(workspace_id)?;
        let SessionSpec::Terminal(spec) = req.spec;
        if let Some(size) = spec.size
            && (size.cols == 0 || size.rows == 0)
        {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("invalid terminal size {}x{}", size.cols, size.rows),
            ));
        }

        let default_title = default_title(&spec);
        let chosen = spec
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        let info = SessionInfo {
            id: SessionId::new(),
            workspace_id,
            kind: SessionKind::Terminal,
            title: chosen.clone().unwrap_or_else(|| default_title.clone()),
            state: SessionState::Starting,
            command: spec.command.clone(),
            cwd: spec.cwd.clone(),
            terminal_id: None,
            created_at_ms: now_ms(),
            restored: false,
        };
        self.publish(&Event::SessionUpdated {
            session: info.clone(),
        })?;
        self.sessions.push(SessionEntry {
            info,
            spec,
            default_title,
            title_locked: chosen.is_some(),
            osc_title: None,
            terminal: None,
            stopping: false,
            stop_waiters: Vec::new(),
            ended_at_ms: None,
            resume_ref_json: None,
            restore_argv: None,
            keep_live: None,
            dirty: false,
        });
        let index = self.sessions.len() - 1;
        self.start_terminal(index);
        Ok(self.sessions[index].info.clone())
    }

    /// Spawns the process for a `Starting` session and moves it to `Running` or `Failed`.
    fn start_terminal(&mut self, index: usize) {
        let restore_argv = self.sessions[index].restore_argv.take();
        let session = &self.sessions[index];
        let mut env: BTreeMap<String, String> = session.spec.env.clone();
        // Daemon-managed variables win over whatever the client asked for.
        env.insert(
            "HEELER_SOCKET".to_owned(),
            self.socket.to_string_lossy().into_owned(),
        );
        env.insert("HEELER_SESSION_ID".to_owned(), session.info.id.to_string());
        env.insert(
            "HEELER_WORKSPACE_ID".to_owned(),
            session.info.workspace_id.to_string(),
        );
        let mut config = TerminalConfig {
            argv: restore_argv.unwrap_or_else(|| session.spec.command.clone()),
            cwd: session.info.cwd.clone(),
            env,
            shell_integration_dir: self.shell_integration_dir.clone(),
            ..TerminalConfig::default()
        };
        if let Some(size) = session.spec.size {
            config.size = size;
        }

        match Terminal::spawn(config) {
            Ok(terminal) => {
                // Subscribed immediately so output the child produces right away is not lost.
                let meta = terminal.metadata();
                let entry = self.hub.register(terminal.clone());
                tokio::spawn(watch_terminal(
                    entry.id(),
                    terminal,
                    meta,
                    self.mailbox.clone(),
                ));
                let session = &mut self.sessions[index];
                session.info.terminal_id = Some(entry.id());
                session.terminal = Some(entry);
                session.advance(SessionEvent::Spawned);
            }
            Err(error) => {
                warn!(session = %session.info.id, %error, "terminal spawn failed");
                self.sessions[index].advance(SessionEvent::Failed {
                    reason: error.to_string(),
                });
            }
        }
        self.emit_session(index);
        self.persist_session(index);
    }

    fn stop_session(&mut self, id: SessionId, reply: Reply<SessionInfo>) {
        let index = match self.session_index(id) {
            Ok(index) => index,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let session = &mut self.sessions[index];
        if !session.info.state.is_live() {
            let _ = reply.send(Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("session {id} is not running"),
            )));
            return;
        }
        session.request_stop();
        session.stop_waiters.push(reply);
    }

    fn restart_session(&mut self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        let index = self.session_index(id)?;
        let session = &mut self.sessions[index];
        if session.info.state.is_live() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("session {id} is still running; stop it first"),
            ));
        }
        session.advance(SessionEvent::Restart);
        if let Some(old) = session.terminal.take() {
            // Attached clients are dropped; they re-attach to the new terminal id.
            self.hub.remove(old.id());
        }
        session.stopping = false;
        session.keep_live = None;
        session.info.restored = false;
        session.osc_title = None;
        session.info.terminal_id = None;
        // Restart where the program last said it was (OSC 7) when that directory still exists.
        session.info.cwd = session
            .info
            .cwd
            .take()
            .filter(|cwd| cwd.is_dir())
            .or_else(|| session.spec.cwd.clone());
        if !session.title_locked {
            session.info.title.clone_from(&session.default_title);
        }
        self.emit_session(index);
        self.start_terminal(index);
        Ok(self.sessions[index].info.clone())
    }

    fn rename_session(&mut self, req: SessionRename) -> Result<SessionInfo, ErrorBody> {
        let title = validated_name(req.title, "session")?;
        let index = self.session_index(req.session_id)?;
        let mut info = self.sessions[index].info.clone();
        info.title = title;
        self.publish(&Event::SessionUpdated {
            session: info.clone(),
        })?;
        let session = &mut self.sessions[index];
        session.info = info.clone();
        session.title_locked = true;
        self.persist_session(index);
        Ok(info)
    }

    fn stop_all(&mut self, forget: bool, reply: Reply<()>) {
        for session in self.sessions.iter_mut().filter(|s| s.info.state.is_live()) {
            // A session the user is already stopping stays stopped; the others die with the
            // daemon but remain in the restore set unless the user asked to forget them.
            if !session.stopping && !forget {
                session.keep_live = Some(session.info.state.clone());
            }
            session.request_stop();
        }
        self.flush_dirty();
        self.stop_all_waiters.push(reply);
        self.settle_stop_all();
    }

    fn settle_stop_all(&mut self) {
        if self.live_count() == 0 {
            for waiter in self.stop_all_waiters.drain(..) {
                let _ = waiter.send(Ok(()));
            }
        }
    }

    // Terminal notifications ------------------------------------------------------------------

    fn on_meta(&mut self, terminal: TerminalId, meta: TerminalMeta) {
        // Stale notifications of a replaced terminal find no session and are dropped.
        let Some(index) = self.terminal_index(terminal) else {
            return;
        };
        match meta {
            TerminalMeta::Title(title) => {
                let title = (!title.is_empty()).then_some(title);
                let session = &mut self.sessions[index];
                if session.osc_title == title {
                    return;
                }
                session.osc_title.clone_from(&title);
                let session_title = if session.title_locked {
                    None
                } else {
                    Some(
                        title
                            .clone()
                            .unwrap_or_else(|| session.default_title.clone()),
                    )
                };
                self.emit(&Event::TerminalTitle {
                    terminal_id: terminal,
                    title,
                });
                if let Some(session_title) = session_title
                    && self.sessions[index].info.title != session_title
                {
                    self.sessions[index].info.title = session_title;
                    self.emit_session(index);
                    self.mark_dirty(index);
                }
            }
            TerminalMeta::Bell => self.emit(&Event::TerminalBell {
                terminal_id: terminal,
            }),
            TerminalMeta::Notification { title, body } => self.emit(&Event::TerminalNotification {
                terminal_id: terminal,
                title: (!title.is_empty()).then_some(title),
                body,
            }),
            TerminalMeta::Cwd(cwd) => {
                let session = &mut self.sessions[index];
                if session.info.cwd.as_deref() != Some(cwd.as_path()) {
                    session.info.cwd = Some(cwd);
                    self.emit_session(index);
                    self.mark_dirty(index);
                }
            }
            // Attention from prompt marks arrives with the agent-state work.
            TerminalMeta::PromptMark(_) => {}
        }
    }

    fn on_exited(&mut self, terminal: TerminalId, status: &ExitStatus) {
        let Some(index) = self.terminal_index(terminal) else {
            return;
        };
        self.hub.mark_ended(terminal);
        let code = status.code;
        let signal = status.signal;
        let session = &mut self.sessions[index];
        session.advance(if session.stopping {
            SessionEvent::StopCompleted
        } else {
            SessionEvent::ProcessExited { code, signal }
        });
        session.stopping = false;
        let info = session.info.clone();
        let waiters = std::mem::take(&mut session.stop_waiters);
        self.emit_session(index);
        self.persist_session(index);
        self.emit(&Event::SessionExited {
            session_id: info.id,
            code,
            signal,
        });
        for waiter in waiters {
            let _ = waiter.send(Ok(info.clone()));
        }
        self.settle_stop_all();
    }

    // Storage ---------------------------------------------------------------------------------

    /// Rebuilds the sessions from their rows. Live ones are spawned again (`Respawn`) or listed
    /// as stopped; ended ones stay as they were, marked `restored` because their screen is gone.
    fn restore_sessions(&mut self, rows: Vec<SessionRow>, mode: RestoreMode) {
        let mut respawn = Vec::new();
        let mut rewrite = Vec::new();
        for row in rows {
            if !self.workspaces.iter().any(|w| w.id == row.workspace_id) {
                warn!(session = %row.id, "stored session has no workspace; skipping it");
                continue;
            }
            let was_live = row.state.is_live();
            let (spec, unusable) = match serde_json::from_str::<TerminalSpec>(&row.spec_json) {
                Ok(spec) if row.kind == SessionKind::Terminal => (spec, None),
                Ok(spec) => (
                    spec,
                    Some(format!("unsupported session kind {:?}", row.kind)),
                ),
                Err(error) => {
                    warn!(session = %row.id, %error, "stored session spec is unreadable");
                    (
                        TerminalSpec::default(),
                        Some(format!("stored spec unreadable: {error}")),
                    )
                }
            };
            let mut state = row.state.clone();
            let mut ended_at_ms = row.ended_at_ms;
            let mut restored = true;
            let mut restore_argv = None;
            let mut cwd = row.cwd.clone();
            let mut action = None;
            if was_live {
                ended_at_ms = Some(now_ms());
                match (unusable, mode) {
                    (Some(reason), _) => {
                        state = SessionState::Failed { reason };
                        action = Some(false);
                    }
                    (None, RestoreMode::Stopped) => {
                        state = SessionState::Stopped;
                        action = Some(false);
                    }
                    (None, RestoreMode::Respawn) => {
                        state = SessionState::Starting;
                        ended_at_ms = None;
                        restored = false;
                        restore_argv = row
                            .resume_ref_json
                            .as_deref()
                            .and_then(restore::resume_argv);
                        // A directory that vanished meanwhile falls back to the spec's, then to
                        // the shell's default.
                        cwd = cwd
                            .filter(|dir| dir.is_dir())
                            .or_else(|| spec.cwd.clone().filter(|dir| dir.is_dir()));
                        action = Some(true);
                    }
                }
            }
            let default_title = default_title(&spec);
            self.sessions.push(SessionEntry {
                info: SessionInfo {
                    id: row.id,
                    workspace_id: row.workspace_id,
                    kind: row.kind,
                    title: row.title,
                    state,
                    command: spec.command.clone(),
                    cwd,
                    terminal_id: None,
                    created_at_ms: row.created_at_ms,
                    restored,
                },
                spec,
                default_title,
                title_locked: row.title_locked,
                osc_title: None,
                terminal: None,
                stopping: false,
                stop_waiters: Vec::new(),
                ended_at_ms,
                resume_ref_json: row.resume_ref_json,
                restore_argv,
                keep_live: None,
                dirty: false,
            });
            let index = self.sessions.len() - 1;
            match action {
                Some(true) => respawn.push(index),
                Some(false) => rewrite.push(index),
                None => {}
            }
        }
        // `start_terminal` stores the session itself once it is running.
        for index in respawn {
            self.start_terminal(index);
        }
        for index in rewrite {
            self.persist_session(index);
        }
    }

    fn session_row(&self, index: usize) -> SessionRow {
        let session = &self.sessions[index];
        // While the daemon takes a session down with itself the row keeps saying it was alive.
        let (state, ended_at_ms) = match &session.keep_live {
            Some(live) => (live.clone(), None),
            None => (session.info.state.clone(), session.ended_at_ms),
        };
        let spec = restore::persistable_spec(&session.spec);
        SessionRow {
            id: session.info.id,
            workspace_id: session.info.workspace_id,
            kind: session.info.kind,
            title: session.info.title.clone(),
            title_locked: session.title_locked,
            spec_json: serde_json::to_string(&spec).unwrap_or_else(|error| {
                error!(session = %session.info.id, %error, "could not encode a session spec");
                "{}".to_owned()
            }),
            cwd: session.info.cwd.clone(),
            state,
            created_at_ms: session.info.created_at_ms,
            ended_at_ms,
            resume_ref_json: session.resume_ref_json.clone(),
        }
    }

    /// Writes the whole session row now.
    fn persist_session(&mut self, index: usize) {
        self.sessions[index].dirty = false;
        let row = self.session_row(index);
        log_write(self.storage.sessions().upsert(row), "a session");
    }

    /// Title and cwd change with every prompt; they are written together at most once per
    /// [`DIRTY_WRITE_INTERVAL`].
    fn mark_dirty(&mut self, index: usize) {
        self.sessions[index].dirty = true;
        self.flush_at
            .get_or_insert_with(|| Instant::now() + DIRTY_WRITE_INTERVAL);
    }

    fn flush_dirty(&mut self) {
        self.flush_at = None;
        for index in 0..self.sessions.len() {
            if self.sessions[index].dirty {
                self.persist_session(index);
            }
        }
    }

    // Events ----------------------------------------------------------------------------------

    fn emit_session(&self, index: usize) {
        self.emit(&Event::SessionUpdated {
            session: self.sessions[index].info.clone(),
        });
    }

    /// For changes that already happened: a rejected event is logged, not rolled back.
    fn emit(&self, event: &Event) {
        if let Err(error) = self.bus.publish(event) {
            error!(%error, "failed to publish event");
        }
    }

    /// Publishes before the caller commits, so a rejected event leaves state untouched.
    fn publish(&self, event: &Event) -> Result<(), ErrorBody> {
        match self.bus.publish(event) {
            Ok(_) => Ok(()),
            Err(PublishError::TooLarge { len }) => Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("description too large ({len} bytes)"),
            )),
            Err(e) => {
                error!(error = %e, "failed to publish event");
                Err(ErrorBody::new(
                    ErrorCode::Internal,
                    "failed to publish event",
                ))
            }
        }
    }
}

/// Forwards one terminal's metadata and its exit to the registry.
async fn watch_terminal(
    id: TerminalId,
    terminal: Terminal,
    mut meta: broadcast::Receiver<TerminalMeta>,
    mailbox: mpsc::WeakSender<Command>,
) {
    let send = |command: Command| {
        let mailbox = mailbox.clone();
        async move {
            match mailbox.upgrade() {
                Some(tx) => tx.send(command).await.is_ok(),
                None => false,
            }
        }
    };

    // A title set before the subscription only exists in the engine state.
    let early_title = terminal.snapshot(&mut RenderTracker::new()).title;
    if let Some(title) = early_title
        && !send(Command::TerminalMeta(id, TerminalMeta::Title(title))).await
    {
        return;
    }

    let exit = terminal.wait_exit();
    tokio::pin!(exit);
    let status = loop {
        tokio::select! {
            biased;
            received = meta.recv() => match received {
                Ok(event) => {
                    if !send(Command::TerminalMeta(id, event)).await {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(terminal = %id, skipped, "terminal metadata lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break exit.await,
            },
            status = &mut exit => break status,
        }
    };
    // Metadata the child emitted just before exiting.
    loop {
        match meta.try_recv() {
            Ok(event) => {
                if !send(Command::TerminalMeta(id, event)).await {
                    return;
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    debug!(terminal = %id, ?status, "terminal exited");
    send(Command::TerminalExited(id, status)).await;
}

fn validated_name(name: String, what: &str) -> Result<String, ErrorBody> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("{what} name must not be empty"),
        ));
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// argv[0]'s basename, or the login shell's name when the spec runs the default shell.
fn default_title(spec: &TerminalSpec) -> String {
    let program = match spec.command.first() {
        Some(program) => program.clone(),
        None => heeler_terminal::default_shell(&spec.env),
    };
    Path::new(&program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "terminal".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_title_is_the_program_basename() {
        let spec = |command: &[&str], shell: Option<&str>| TerminalSpec {
            command: command.iter().map(|c| (*c).to_owned()).collect(),
            env: shell
                .map(|s| BTreeMap::from([("SHELL".to_owned(), s.to_owned())]))
                .unwrap_or_default(),
            ..TerminalSpec::default()
        };
        assert_eq!(default_title(&spec(&["/usr/bin/omp", "-x"], None)), "omp");
        assert_eq!(default_title(&spec(&["htop"], None)), "htop");
        assert_eq!(
            default_title(&spec(&[], Some("/opt/homebrew/bin/fish"))),
            "fish"
        );
    }
}
