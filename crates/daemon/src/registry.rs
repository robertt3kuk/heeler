//! `SessionRegistry` actor: owns workspaces and terminal sessions and publishes their events.
//! Everything reaches it through one bounded mailbox; replies travel over `oneshot`s.
//!
//! The actor never waits on a child process. Stopping a session signals its terminal and parks
//! the reply until the terminal's watcher task reports the exit back through the same mailbox.
//!
//! Every change is written through to `hutan-storage` (fire-and-forget into the writer thread,
//! so request handling never waits for the disk), and [`spawn`] rebuilds the state from what was
//! stored (ADR 0009 step 1): live sessions come back as new terminals with the same ids.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::{Instant, sleep_until};

use hutan_agents::{CodexConfig, ExitInfo, Started};
use hutan_core::{
    NodeOutcome, NodeUpdate, ObservationTree, Patch, SessionEvent, SessionId, SessionKind,
    SessionState, TerminalId, WorkspaceId,
};
use hutan_protocol::chat::{ChatCreate, ChatInfo};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, CreatedBy, Event,
    RECENT_PROJECTS_DEFAULT_LIMIT, RECENT_PROJECTS_MAX_LIMIT, RecentProject, RecentProjects,
    SessionCreate, SessionInfo, SessionList, SessionListFilter, SessionRename, SessionSnapshot,
    SessionSpec, TerminalSpec, WorkspaceCreate, WorkspaceInfo, WorkspaceKind, WorkspaceList,
    WorkspaceMove, WorkspaceOpen, WorkspaceOpened, WorkspacePin, WorkspaceRename,
};
use hutan_protocol::provider::ChatSetModel;
use hutan_protocol::tree::{AgentTree, NodeReport};
use hutan_protocol::worktree::Placement;
use hutan_storage::{self as stored_rows, SessionRow, Storage, StorageError, WorkspaceRow, Write};
use hutan_terminal::{
    EnvInherit, ExitStatus, PromptMark, RenderTracker, Terminal, TerminalConfig, TerminalMeta,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, warn};

use crate::agent;
use crate::bus::{EventBus, PublishError};
use crate::chat::{
    AGENT_ASSISTANT, AGENT_CODEX, AGENT_DIRECT, ChatManager, ChatSpec, is_gateway_agent,
};
use crate::config::RestoreMode;
use crate::direct::MAX_SYSTEM_PROMPT_BYTES;
use crate::harness::{Harnesses, is_harness_agent};
use crate::placement::{Occupant, PlacementCache, shared_with};
use crate::projects::{self, Folder};
use crate::providers::ProviderService;
use crate::restore::{self, Stored};
use crate::terminals::{TerminalEntry, TerminalHub};
use crate::tree::{self, TreeState};

/// Commands waiting in the mailbox before senders wait their turn.
const MAILBOX_CAPACITY: usize = 256;

/// Time a terminal gets to react to SIGHUP before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

/// Name of the Home workspace when the daemon creates it.
pub const HOME_WORKSPACE_NAME: &str = "Home";

/// A workspace's `last_active_ms` changes at most this often (session creation excepted).
pub const ACTIVITY_INTERVAL: Duration = Duration::from_secs(10);

/// Terminals tell the registry about input and output at most this often each; the registry
/// applies [`ACTIVITY_INTERVAL`] per workspace.
pub const ACTIVITY_NOTIFY_INTERVAL: Duration = Duration::from_secs(1);

/// Title and cwd changes are coalesced: a session is written at most this often because of them.
const DIRTY_WRITE_INTERVAL: Duration = Duration::from_secs(1);

/// Longest accepted `source`, `agent`, `agent_session_id`, node id, or parent id.
const MAX_AGENT_FIELD_BYTES: usize = 256;
/// Longer agent messages are cut to this many characters.
const MAX_AGENT_MESSAGE_CHARS: usize = 512;
/// Longer node names, types and models are cut to this many characters.
const MAX_NODE_TEXT_CHARS: usize = 128;
/// Longest accepted `usage.kind`.
const MAX_USAGE_KIND_BYTES: usize = 64;
/// A transcript path longer than this is not kept.
const MAX_TRANSCRIPT_PATH_BYTES: usize = 1024;

type Reply<T> = oneshot::Sender<Result<T, ErrorBody>>;

#[derive(Debug)]
enum Command {
    CreateWorkspace(WorkspaceCreate, Reply<WorkspaceInfo>),
    ListWorkspaces(Reply<WorkspaceList>),
    RenameWorkspace(WorkspaceRename, Reply<WorkspaceInfo>),
    CloseWorkspace(WorkspaceId, Reply<()>),
    /// The folder is already resolved (canonical, repository root found).
    OpenWorkspace(Folder, Option<CreatedBy>, Reply<WorkspaceOpened>),
    AddRoot(WorkspaceId, Folder, Reply<WorkspaceInfo>),
    RemoveRoot(WorkspaceId, PathBuf, Reply<WorkspaceInfo>),
    RecentProjects(Option<u32>, Reply<RecentProjects>),
    MoveWorkspace(WorkspaceMove, Reply<WorkspaceList>),
    PinWorkspace(WorkspacePin, Reply<WorkspaceInfo>),
    /// Input or output of a terminal; from its watcher task and the input path.
    Activity(TerminalId),
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
    AgentReport(Box<AgentReportRequest>, Reply<AgentApplied>),
    AgentRelease(AgentReleaseRequest, Reply<AgentApplied>),
    AgentTree(Option<SessionId>, Reply<AgentTree>),
    /// `chat.create` with a canonical `cwd`; answered once the agent is up (or failed).
    CreateChat(ChatCreate, Reply<SessionInfo>),
    /// From the task that started a chat agent.
    ChatStarted {
        session: SessionId,
        outcome: Result<Started, ErrorBody>,
        reply: Option<Reply<SessionInfo>>,
    },
    /// From a chat session's pump: a turn started or ended (`turn`), and how many approvals
    /// are pending now.
    ChatState {
        session: SessionId,
        turn: Option<bool>,
        pending: u32,
    },
    /// From a chat session's pump: the agent process is gone.
    ChatExited {
        session: SessionId,
        exit: ExitInfo,
    },
    /// From a chat session's pump: the adapter reports on an agent of the session's tree.
    ChatNode {
        session: SessionId,
        update: Box<NodeUpdate>,
    },
    /// Whether the session is a chat session, and live.
    ChatTarget(SessionId, Reply<bool>),
    /// `chat.set_model`: answered once the switch is journaled.
    SetChatModel(ChatSetModel, Reply<SessionInfo>),
    /// From the task that switched the model of a harness chat: the agent's answer.
    ChatModelSwitched {
        session: SessionId,
        model: String,
        outcome: Result<(), ErrorBody>,
        reply: Reply<SessionInfo>,
    },
    /// A worktree was created or removed: the remembered checkouts are stale. Answered once
    /// every session's placement is derived again.
    InvalidatePlacements(Reply<()>),
}

/// How a chat session's pump reaches the registry.
#[derive(Debug, Clone)]
pub(crate) struct ChatNotifier {
    tx: mpsc::WeakSender<Command>,
}

impl ChatNotifier {
    async fn send(&self, command: Command) {
        if let Some(tx) = self.tx.upgrade() {
            // A closed mailbox means the daemon is going down.
            let _ = tx.send(command).await;
        }
    }

    pub(crate) async fn state(&self, session: SessionId, turn: Option<bool>, pending: u32) {
        self.send(Command::ChatState {
            session,
            turn,
            pending,
        })
        .await;
    }

    pub(crate) async fn exited(&self, session: SessionId, exit: ExitInfo) {
        self.send(Command::ChatExited { session, exit }).await;
    }

    /// The adapter saw something about an agent of the session (the main one or a subagent).
    pub(crate) async fn node(&self, session: SessionId, update: NodeUpdate) {
        self.send(Command::ChatNode {
            session,
            update: Box::new(update),
        })
        .await;
    }
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
    chats: Arc<ChatManager>,
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

    /// The workspace containing `req.path`, or a new one for the folder (its git root).
    pub async fn open_workspace(&self, req: WorkspaceOpen) -> Result<WorkspaceOpened, ErrorBody> {
        let folder = resolve_folder(req.path).await?;
        self.ask(|r| Command::OpenWorkspace(folder, req.created_by, r))
            .await
    }

    pub async fn add_root(
        &self,
        workspace_id: WorkspaceId,
        path: PathBuf,
    ) -> Result<WorkspaceInfo, ErrorBody> {
        let folder = resolve_folder(path).await?;
        self.ask(|r| Command::AddRoot(workspace_id, folder, r))
            .await
    }

    pub async fn remove_root(
        &self,
        workspace_id: WorkspaceId,
        path: PathBuf,
    ) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::RemoveRoot(workspace_id, path, r))
            .await
    }

    pub async fn recent_projects(&self, limit: Option<u32>) -> Result<RecentProjects, ErrorBody> {
        self.ask(|r| Command::RecentProjects(limit, r)).await
    }

    pub async fn move_workspace(&self, req: WorkspaceMove) -> Result<WorkspaceList, ErrorBody> {
        self.ask(|r| Command::MoveWorkspace(req, r)).await
    }

    pub async fn pin_workspace(&self, req: WorkspacePin) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::PinWorkspace(req, r)).await
    }

    /// Input reached `terminal`. Never waits: a full mailbox just misses one activity note.
    pub fn note_activity(&self, terminal: TerminalId) {
        let _ = self.tx.try_send(Command::Activity(terminal));
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

    /// A coding agent in the session reports its state and how to resume it.
    pub async fn report_agent(&self, req: AgentReportRequest) -> Result<AgentApplied, ErrorBody> {
        self.ask(|r| Command::AgentReport(Box::new(req), r)).await
    }

    /// The agent behind `req.source` is gone.
    pub async fn release_agent(&self, req: AgentReleaseRequest) -> Result<AgentApplied, ErrorBody> {
        self.ask(|r| Command::AgentRelease(req, r)).await
    }

    /// The agent trees of one session, or of every session that has one.
    pub async fn agent_tree(&self, session: Option<SessionId>) -> Result<AgentTree, ErrorBody> {
        self.ask(|r| Command::AgentTree(session, r)).await
    }

    /// The chat sessions' runtime: sending, interrupting, approvals, history.
    pub fn chats(&self) -> &Arc<ChatManager> {
        &self.chats
    }

    /// Opens an agent conversation as a session; returns once the agent is ready (state
    /// `idle`) or fails with `provider_error` when it could not start.
    pub async fn create_chat(&self, mut req: ChatCreate) -> Result<SessionInfo, ErrorBody> {
        req.cwd = canonical_dir(req.cwd).await?;
        self.ask(|r| Command::CreateChat(req, r)).await
    }

    /// Whether `id` is a chat session (else an error), and whether it is live.
    pub async fn chat_target(&self, id: SessionId) -> Result<bool, ErrorBody> {
        self.ask(|r| Command::ChatTarget(id, r)).await
    }

    /// Switches the model (and optionally the provider) of a running direct chat; its next
    /// turn uses it and the journal records the switch.
    pub async fn set_chat_model(&self, req: ChatSetModel) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::SetChatModel(req, r)).await
    }

    /// `worktree.*` changed the checkouts on disk: forgets what is known of directories and
    /// derives every session's placement again (announcing the ones that changed).
    pub async fn invalidate_placements(&self) -> Result<(), ErrorBody> {
        self.ask(Command::InvalidatePlacements).await
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
    /// The main agent's last `message` and conversation id (what `SessionInfo.agent` carries
    /// besides the state).
    agent_message: Option<String>,
    agent_session_id: Option<String>,
    /// The agents of the session: the main agent and its subagents, with the sequence numbers
    /// of their reporters (`agent.report` / `agent.release`).
    tree: TreeState,
    /// What an agent chat session is (`kind` harness); such a session has no terminal.
    chat: Option<ChatSpec>,
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
    fn request_stop(&mut self, chats: &ChatManager) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        if let Some(entry) = &self.terminal
            && let Err(error) = entry.terminal().terminate(STOP_GRACE)
        {
            warn!(session = %self.info.id, %error, "could not signal the terminal");
        }
        if self.chat.is_some() {
            chats.stop(self.info.id);
        }
    }

    /// The agent is gone — its process ended, or the shell prompts again — without having said
    /// so: every node of its tree has lost contact, and its message, conversation id and
    /// resume reference are forgotten. Returns whether a resume reference was dropped.
    fn forget_agent(&mut self, now_ms: u64) -> bool {
        self.tree.session_ended(now_ms);
        self.agent_message = None;
        self.agent_session_id = None;
        self.resume_ref_json.take().is_some()
    }
}

/// One workspace and the bookkeeping around it.
#[derive(Debug)]
struct WorkspaceEntry {
    /// What clients know: the last published state.
    info: WorkspaceInfo,
    /// Session activity happened that `info.last_active_ms` does not show yet (throttled).
    activity_pending: bool,
}

impl WorkspaceEntry {
    fn new(info: WorkspaceInfo) -> Self {
        Self {
            info,
            activity_pending: false,
        }
    }
}

#[derive(Debug)]
struct SessionRegistry {
    bus: Arc<EventBus>,
    hub: Arc<TerminalHub>,
    /// Exported to children as `HUTAN_SOCKET`.
    socket: PathBuf,
    /// Installed shell-integration scripts handed to every new terminal; `None` = no injection.
    shell_integration_dir: Option<PathBuf>,
    /// Where new terminals' base environment comes from (`[terminal] env_inherit`).
    env_inherit: EnvInherit,
    /// `SHELL` of sessions whose spec names none; `None` = the account's login shell.
    login_shell: Option<String>,
    /// Exported to children as `HUTAN_BIN`.
    hutan_bin: String,
    /// Weak so the actor stops once every [`RegistryHandle`] is gone.
    mailbox: mpsc::WeakSender<Command>,
    /// The Home workspace: where sessions without a workspace go.
    home: WorkspaceId,
    /// In `position` order.
    workspaces: Vec<WorkspaceEntry>,
    /// Closed projects, newest first.
    recent: Vec<stored_rows::RecentProject>,
    /// `position` for the next workspace row.
    next_position: i64,
    activity_interval: Duration,
    sessions: Vec<SessionEntry>,
    stop_all_waiters: Vec<Reply<()>>,
    chats: Arc<ChatManager>,
    storage: Storage,
    /// When sessions with unwritten title/cwd changes are written next.
    flush_at: Option<Instant>,
    /// When the earliest throttled workspace activity is applied.
    activity_flush_at: Option<Instant>,
    /// When the earliest held-back `agent.tree_updated` may go out.
    tree_flush_at: Option<Instant>,
    /// The git checkout of the directories sessions work in (`SessionInfo.placement`).
    placements: PlacementCache,
}

/// What [`spawn`] needs besides the shared services.
#[derive(Debug)]
pub struct RegistryConfig {
    /// Exported to children as `HUTAN_SOCKET`.
    pub socket: PathBuf,
    /// Installed shell-integration scripts handed to every new terminal; `None` = no injection.
    pub shell_integration_dir: Option<PathBuf>,
    /// `[terminal] env_inherit`: where new terminals' base environment comes from.
    pub env_inherit: EnvInherit,
    /// `SHELL` of sessions whose spec names none; `None` = the account's login shell.
    pub login_shell: Option<String>,
    /// The `hutan` CLI exported to sessions as `HUTAN_BIN`.
    pub hutan_bin: PathBuf,
    pub storage: Storage,
    pub restore: RestoreMode,
    /// The folder of the Home workspace (existing, canonical).
    pub home_folder: PathBuf,
    /// Shortest time between two `last_active_ms` updates of one workspace.
    pub activity_interval: Duration,
    /// How to start `codex app-server` for chat sessions.
    pub codex: CodexConfig,
    /// The ACP harnesses (`omp`, `claude`) chat sessions can run through.
    pub harnesses: Arc<Harnesses>,
    /// The model providers of direct chats.
    pub providers: Arc<ProviderService>,
    /// `gateway.max_context_chars`.
    pub max_context_chars: usize,
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
    let chats = ChatManager::new(
        bus.clone(),
        config.storage.clone(),
        ChatNotifier { tx: tx.downgrade() },
        config.codex,
        config.harnesses,
        config.providers,
        config.max_context_chars,
    );
    let (workspaces, home, next_position) =
        restored_workspaces(&stored, &config.storage, &config.home_folder);
    let mut registry = SessionRegistry {
        bus,
        hub,
        socket: config.socket,
        shell_integration_dir: config.shell_integration_dir,
        env_inherit: config.env_inherit,
        login_shell: config.login_shell,
        hutan_bin: config.hutan_bin.to_string_lossy().into_owned(),
        mailbox: tx.downgrade(),
        home,
        workspaces,
        recent: stored.recent_projects,
        next_position,
        activity_interval: config.activity_interval,
        sessions: Vec::new(),
        stop_all_waiters: Vec::new(),
        chats: chats.clone(),
        storage: config.storage,
        flush_at: None,
        activity_flush_at: None,
        tree_flush_at: None,
        placements: PlacementCache::default(),
    };
    registry.restore_sessions(stored.sessions, config.restore, stored.agent_trees);
    tokio::spawn(registry.run(rx));
    RegistryHandle { tx, chats }
}

/// The stored workspaces, Home among them (created and stored when missing, bound to
/// `home_folder`), and the next free position.
fn restored_workspaces(
    stored: &Stored,
    storage: &Storage,
    home_folder: &Path,
) -> (Vec<WorkspaceEntry>, WorkspaceId, i64) {
    let mut workspaces: Vec<WorkspaceEntry> = stored
        .workspaces
        .iter()
        .map(|w| {
            WorkspaceEntry::new(WorkspaceInfo {
                id: w.id,
                name: w.name.clone(),
                roots: w.roots.clone(),
                kind: kind_from_row(w.kind),
                pinned: w.pinned,
                position: w.position,
                created_by: created_by_from_row(w.created_by),
                last_active_ms: w.last_active_ms,
                live_sessions: 0,
                total_sessions: 0,
            })
        })
        .collect();
    let mut next_position = stored
        .workspaces
        .iter()
        .map(|w| w.position)
        .max()
        .map_or(0, |last| last + 1);
    let existing = workspaces
        .iter_mut()
        .find(|w| w.info.kind == WorkspaceKind::Home);
    let home = match existing {
        Some(home) => {
            // The configured folder is the truth; the user may have changed it.
            if home.info.roots != [home_folder] {
                home.info.roots = vec![home_folder.to_owned()];
                log_write(
                    storage
                        .workspaces()
                        .set_roots(home.info.id, home.info.roots.clone()),
                    "the Home folder",
                );
            }
            home.info.id
        }
        None => {
            let now = now_ms();
            // First in the list, ahead of whatever was stored.
            let position = stored
                .workspaces
                .iter()
                .map(|w| w.position)
                .min()
                .map_or(0, |first| first - 1);
            let info = WorkspaceInfo {
                id: WorkspaceId::new(),
                name: HOME_WORKSPACE_NAME.to_owned(),
                roots: vec![home_folder.to_owned()],
                kind: WorkspaceKind::Home,
                pinned: false,
                position,
                created_by: CreatedBy::User,
                last_active_ms: now,
                live_sessions: 0,
                total_sessions: 0,
            };
            log_write(
                storage.workspaces().insert(row_of(&info, now)),
                "the Home workspace",
            );
            let id = info.id;
            workspaces.insert(0, WorkspaceEntry::new(info));
            id
        }
    };
    next_position = next_position.max(1);
    (workspaces, home, next_position)
}

fn kind_from_row(kind: stored_rows::WorkspaceKind) -> WorkspaceKind {
    match kind {
        stored_rows::WorkspaceKind::Home => WorkspaceKind::Home,
        stored_rows::WorkspaceKind::Project => WorkspaceKind::Project,
    }
}

fn kind_to_row(kind: WorkspaceKind) -> stored_rows::WorkspaceKind {
    match kind {
        WorkspaceKind::Home => stored_rows::WorkspaceKind::Home,
        WorkspaceKind::Project => stored_rows::WorkspaceKind::Project,
    }
}

fn created_by_from_row(created_by: stored_rows::CreatedBy) -> CreatedBy {
    match created_by {
        stored_rows::CreatedBy::User => CreatedBy::User,
        stored_rows::CreatedBy::Cli => CreatedBy::Cli,
        stored_rows::CreatedBy::Agent => CreatedBy::Agent,
    }
}

fn created_by_to_row(created_by: CreatedBy) -> stored_rows::CreatedBy {
    match created_by {
        CreatedBy::User => stored_rows::CreatedBy::User,
        CreatedBy::Cli => stored_rows::CreatedBy::Cli,
        CreatedBy::Agent => stored_rows::CreatedBy::Agent,
    }
}

/// The row of a workspace that is being created.
fn row_of(info: &WorkspaceInfo, created_at_ms: u64) -> WorkspaceRow {
    WorkspaceRow {
        id: info.id,
        name: info.name.clone(),
        roots: info.roots.clone(),
        kind: kind_to_row(info.kind),
        pinned: info.pinned,
        created_by: created_by_to_row(info.created_by),
        created_at_ms,
        last_active_ms: info.last_active_ms,
        position: info.position,
    }
}

/// Canonicalises a folder on the blocking pool.
async fn resolve_folder(path: PathBuf) -> Result<Folder, ErrorBody> {
    tokio::task::spawn_blocking(move || projects::resolve_folder(&path))
        .await
        .map_err(|error| {
            error!(%error, "resolving a folder failed");
            ErrorBody::new(ErrorCode::Internal, "could not resolve the folder")
        })?
}

/// The canonical form of an existing directory, resolved on the blocking pool.
async fn canonical_dir(path: PathBuf) -> Result<PathBuf, ErrorBody> {
    if !path.is_absolute() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("`{}` is not an absolute path", path.display()),
        ));
    }
    let shown = path.display().to_string();
    tokio::task::spawn_blocking(move || std::fs::canonicalize(&path).ok().filter(|p| p.is_dir()))
        .await
        .map_err(|error| {
            error!(%error, "resolving a directory failed");
            ErrorBody::new(ErrorCode::Internal, "could not resolve the directory")
        })?
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::BadRequest,
                format!("`{shown}` is not an existing directory"),
            )
        })
}

/// Storage failures never take request handling down: the in-memory state stays authoritative
/// and the next write may succeed.
pub(crate) fn log_write(result: Result<(), StorageError>, what: &str) {
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
            let deadline = [self.flush_at, self.activity_flush_at, self.tree_flush_at]
                .into_iter()
                .flatten()
                .min();
            let command = match deadline {
                Some(deadline) => tokio::select! {
                    command = mailbox.recv() => command,
                    () = sleep_until(deadline) => {
                        self.on_timer();
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
                    let _ = reply.send(Ok(self.workspace_list()));
                }
                Command::RenameWorkspace(req, reply) => {
                    let _ = reply.send(self.rename_workspace(req));
                }
                Command::CloseWorkspace(id, reply) => {
                    let _ = reply.send(self.close_workspace(id));
                }
                Command::OpenWorkspace(folder, created_by, reply) => {
                    let _ = reply.send(self.open_workspace(&folder, created_by));
                }
                Command::AddRoot(id, folder, reply) => {
                    let _ = reply.send(self.add_root(id, &folder));
                }
                Command::RemoveRoot(id, path, reply) => {
                    let _ = reply.send(self.remove_root(id, &path));
                }
                Command::RecentProjects(limit, reply) => {
                    let _ = reply.send(Ok(self.recent_projects(limit)));
                }
                Command::MoveWorkspace(req, reply) => {
                    let _ = reply.send(self.move_workspace(req));
                }
                Command::PinWorkspace(req, reply) => {
                    let _ = reply.send(self.pin_workspace(req));
                }
                Command::Activity(terminal) => self.on_activity(terminal),
                Command::Snapshot(reply) => {
                    // The actor is the only publisher of the state it snapshots, so the bus's
                    // last seq is exactly the watermark of this state.
                    let _ = reply.send(Ok(SessionSnapshot {
                        workspaces: self.workspace_list().workspaces,
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
                Command::AgentReport(req, reply) => {
                    let _ = reply.send(self.report_agent(*req));
                }
                Command::AgentRelease(req, reply) => {
                    let _ = reply.send(self.release_agent(req));
                }
                Command::AgentTree(session, reply) => {
                    let _ = reply.send(self.agent_tree(session));
                }
                Command::CreateChat(req, reply) => self.create_chat(req, reply),
                Command::ChatStarted {
                    session,
                    outcome,
                    reply,
                } => self.on_chat_started(session, outcome, reply),
                Command::ChatState {
                    session,
                    turn,
                    pending,
                } => self.on_chat_state(session, turn, pending),
                Command::ChatExited { session, exit } => self.on_chat_exited(session, &exit),
                Command::ChatNode { session, update } => self.on_chat_node(session, *update),
                Command::InvalidatePlacements(reply) => {
                    self.invalidate_placements();
                    let _ = reply.send(Ok(()));
                }
                Command::ChatTarget(id, reply) => {
                    let _ = reply.send(self.chat_target(id));
                }
                Command::SetChatModel(req, reply) => self.set_chat_model(req, reply),
                Command::ChatModelSwitched {
                    session,
                    model,
                    outcome,
                    reply,
                } => self.on_chat_model_switched(session, model, outcome, reply),
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

    fn workspace_list(&self) -> WorkspaceList {
        WorkspaceList {
            workspaces: self.workspaces.iter().map(|w| w.info.clone()).collect(),
        }
    }

    /// `(live, total)` sessions of a workspace.
    fn session_counts_of(&self, id: WorkspaceId) -> (u32, u32) {
        let mut counts = (0, 0);
        for session in self.sessions.iter().filter(|s| s.info.workspace_id == id) {
            counts.1 += 1;
            counts.0 += u32::from(session.info.state.is_live());
        }
        counts
    }

    /// Fills in the session counts after the sessions were restored; nothing is announced
    /// (clients take a snapshot after a daemon start).
    fn recount_sessions(&mut self) {
        for index in 0..self.workspaces.len() {
            let (live, total) = self.session_counts_of(self.workspaces[index].info.id);
            let info = &mut self.workspaces[index].info;
            info.live_sessions = live;
            info.total_sessions = total;
        }
    }

    /// Applies `change`, refreshes the session counts, and announces the result when anything
    /// differs from what clients know. The event is published before the state changes, so a
    /// rejected event leaves everything as it was. Storage is the caller's business.
    fn update_workspace(
        &mut self,
        id: WorkspaceId,
        change: impl FnOnce(&mut WorkspaceInfo),
    ) -> Result<WorkspaceInfo, ErrorBody> {
        let index = self.index_of(id)?;
        let mut next = self.workspaces[index].info.clone();
        change(&mut next);
        (next.live_sessions, next.total_sessions) = self.session_counts_of(id);
        if next != self.workspaces[index].info {
            self.publish(&Event::WorkspaceUpdated {
                workspace: next.clone(),
            })?;
            self.workspaces[index].info = next.clone();
        }
        Ok(next)
    }

    /// Announces new session counts of a workspace (a session changed state).
    fn sync_workspace(&mut self, id: WorkspaceId) {
        if let Err(error) = self.update_workspace(id, |_| {}) {
            warn!(workspace = %id, message = %error.message, "could not announce a workspace change");
        }
    }

    fn create_workspace(&mut self, req: WorkspaceCreate) -> Result<WorkspaceInfo, ErrorBody> {
        let name = validated_name(req.name, "workspace")?;
        self.add_project(name, req.roots, req.created_by.unwrap_or_default())
    }

    /// Creates, announces, and stores a project workspace at the end of the list.
    fn add_project(
        &mut self,
        name: String,
        roots: Vec<PathBuf>,
        created_by: CreatedBy,
    ) -> Result<WorkspaceInfo, ErrorBody> {
        let now = now_ms();
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            name,
            roots,
            kind: WorkspaceKind::Project,
            pinned: false,
            position: self.next_position,
            created_by,
            last_active_ms: now,
            live_sessions: 0,
            total_sessions: 0,
        };
        self.publish(&Event::WorkspaceUpdated {
            workspace: workspace.clone(),
        })?;
        self.next_position += 1;
        log_write(
            self.storage.workspaces().insert(row_of(&workspace, now)),
            "a new workspace",
        );
        self.workspaces.push(WorkspaceEntry::new(workspace.clone()));
        self.remember_project(&workspace, now);
        Ok(workspace)
    }

    /// Notes a project in the recent list (by its primary root); Home is not a project.
    fn remember_project(&mut self, workspace: &WorkspaceInfo, opened_ms: u64) {
        if workspace.kind != WorkspaceKind::Project {
            return;
        }
        let Some(path) = workspace.roots.first() else {
            return;
        };
        let project = stored_rows::RecentProject {
            path: path.clone(),
            name: workspace.name.clone(),
            last_opened_ms: opened_ms,
        };
        match self.recent.iter_mut().find(|r| r.path == project.path) {
            Some(known) => {
                known.name.clone_from(&project.name);
                known.last_opened_ms = known.last_opened_ms.max(opened_ms);
            }
            None => self.recent.push(project.clone()),
        }
        self.recent.sort_by(|a, b| {
            b.last_opened_ms
                .cmp(&a.last_opened_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        self.recent.truncate(stored_rows::RECENT_PROJECTS_KEPT);
        log_write(
            self.storage.recent_projects().upsert(project),
            "a recent project",
        );
    }

    fn rename_workspace(&mut self, req: WorkspaceRename) -> Result<WorkspaceInfo, ErrorBody> {
        let name = validated_name(req.name, "workspace")?;
        let workspace = self.update_workspace(req.workspace_id, |w| w.name = name)?;
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
        if id == self.home {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the Home workspace cannot be closed",
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
        let closed = self.workspaces.remove(index).info;
        // The project stays in the recent list.
        self.remember_project(&closed, closed.last_active_ms);
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

    /// The workspace whose root contains `path` (canonical); the deepest root wins, the
    /// earlier workspace on a tie.
    fn workspace_containing(&self, path: &Path) -> Option<usize> {
        let mut best: Option<(usize, usize)> = None;
        for (index, workspace) in self.workspaces.iter().enumerate() {
            for root in &workspace.info.roots {
                if projects::root_contains(root, path) {
                    let depth = root.components().count();
                    if best.is_none_or(|(_, deepest)| depth > deepest) {
                        best = Some((index, depth));
                    }
                }
            }
        }
        best.map(|(index, _)| index)
    }

    fn open_workspace(
        &mut self,
        folder: &Folder,
        created_by: Option<CreatedBy>,
    ) -> Result<WorkspaceOpened, ErrorBody> {
        if let Some(index) = self.workspace_containing(&folder.path) {
            let workspace = self.workspaces[index].info.clone();
            self.remember_project(&workspace, now_ms());
            return Ok(WorkspaceOpened {
                workspace,
                created: false,
            });
        }
        // A project that was closed comes back under the name it had.
        let name = self
            .recent
            .iter()
            .find(|r| r.path == folder.root)
            .map_or_else(|| folder.name(), |r| r.name.clone());
        let workspace = self.add_project(
            name,
            vec![folder.root.clone()],
            created_by.unwrap_or_default(),
        )?;
        Ok(WorkspaceOpened {
            workspace,
            created: true,
        })
    }

    fn project_for_roots(&self, id: WorkspaceId) -> Result<usize, ErrorBody> {
        let index = self.index_of(id)?;
        if self.workspaces[index].info.kind == WorkspaceKind::Home {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the Home workspace is bound to its folder; set `home_folder` in daemon.toml",
            ));
        }
        Ok(index)
    }

    fn add_root(&mut self, id: WorkspaceId, folder: &Folder) -> Result<WorkspaceInfo, ErrorBody> {
        let index = self.project_for_roots(id)?;
        let current = &self.workspaces[index].info;
        if current
            .roots
            .iter()
            .any(|root| projects::same_place(root, &folder.root))
        {
            return Ok(current.clone());
        }
        let workspace = self.update_workspace(id, |w| w.roots.push(folder.root.clone()))?;
        log_write(
            self.storage
                .workspaces()
                .set_roots(id, workspace.roots.clone()),
            "workspace roots",
        );
        self.remember_project(&workspace, workspace.last_active_ms);
        Ok(workspace)
    }

    fn remove_root(&mut self, id: WorkspaceId, path: &Path) -> Result<WorkspaceInfo, ErrorBody> {
        let index = self.project_for_roots(id)?;
        let at = self.workspaces[index]
            .info
            .roots
            .iter()
            .position(|root| projects::same_place(root, path))
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("{} is not a root of workspace {id}", path.display()),
                )
            })?;
        let workspace = self.update_workspace(id, |w| {
            w.roots.remove(at);
        })?;
        log_write(
            self.storage
                .workspaces()
                .set_roots(id, workspace.roots.clone()),
            "workspace roots",
        );
        Ok(workspace)
    }

    /// Open projects and closed ones still remembered, newest first. An open project's time is
    /// its latest activity when that is newer than when it was last opened.
    fn recent_projects(&self, limit: Option<u32>) -> RecentProjects {
        let limit = limit
            .unwrap_or(RECENT_PROJECTS_DEFAULT_LIMIT)
            .min(RECENT_PROJECTS_MAX_LIMIT);
        let mut projects: Vec<RecentProject> = Vec::new();
        for workspace in self.workspaces.iter().map(|w| &w.info) {
            let Some(path) = workspace.roots.first() else {
                continue;
            };
            if workspace.kind != WorkspaceKind::Project {
                continue;
            }
            let opened = self
                .recent
                .iter()
                .find(|r| &r.path == path)
                .map_or(0, |r| r.last_opened_ms);
            projects.push(RecentProject {
                path: path.clone(),
                name: workspace.name.clone(),
                last_opened_ms: opened.max(workspace.last_active_ms),
                workspace_id: Some(workspace.id),
            });
        }
        for closed in &self.recent {
            let open = self
                .workspaces
                .iter()
                .any(|w| w.info.roots.contains(&closed.path));
            if !open {
                projects.push(RecentProject {
                    path: closed.path.clone(),
                    name: closed.name.clone(),
                    last_opened_ms: closed.last_opened_ms,
                    workspace_id: None,
                });
            }
        }
        projects.sort_by(|a, b| {
            b.last_opened_ms
                .cmp(&a.last_opened_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        projects.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        RecentProjects { projects }
    }

    /// Moves a workspace before another (or to the end) and renumbers the positions `0..n`;
    /// every workspace whose position changed is announced.
    fn move_workspace(&mut self, req: WorkspaceMove) -> Result<WorkspaceList, ErrorBody> {
        let from = self.index_of(req.workspace_id)?;
        if let Some(before) = req.before {
            self.index_of(before)?;
            if before == req.workspace_id {
                return Ok(self.workspace_list());
            }
        }
        let moved = self.workspaces.remove(from);
        let at = req
            .before
            .and_then(|before| self.workspaces.iter().position(|w| w.info.id == before))
            .unwrap_or(self.workspaces.len());
        self.workspaces.insert(at, moved);

        let mut writes = Vec::new();
        let mut changed = Vec::new();
        for (index, entry) in self.workspaces.iter_mut().enumerate() {
            let position = i64::try_from(index).unwrap_or(i64::MAX);
            if entry.info.position != position {
                entry.info.position = position;
                writes.push(Write::SetWorkspacePosition {
                    id: entry.info.id,
                    position,
                });
                changed.push(entry.info.clone());
            }
        }
        self.next_position = i64::try_from(self.workspaces.len()).unwrap_or(i64::MAX);
        if !writes.is_empty() {
            log_write(self.storage.batch(writes), "the workspace order");
            for workspace in changed {
                self.emit(&Event::WorkspaceUpdated { workspace });
            }
        }
        Ok(self.workspace_list())
    }

    fn pin_workspace(&mut self, req: WorkspacePin) -> Result<WorkspaceInfo, ErrorBody> {
        let workspace = self.update_workspace(req.workspace_id, |w| w.pinned = req.pinned)?;
        log_write(
            self.storage
                .workspaces()
                .set_pinned(workspace.id, workspace.pinned),
            "a workspace pin",
        );
        Ok(workspace)
    }

    // Workspace activity ----------------------------------------------------------------------

    fn activity_interval_ms(&self) -> u64 {
        u64::try_from(self.activity_interval.as_millis()).unwrap_or(u64::MAX)
    }

    /// Input or output in a terminal.
    fn on_activity(&mut self, terminal: TerminalId) {
        let Some(index) = self.terminal_index(terminal) else {
            return;
        };
        let session = &self.sessions[index];
        if session.info.state.is_live() {
            self.note_activity(session.info.workspace_id, now_ms());
        }
    }

    /// Applies the activity now when the workspace was last updated at least one interval ago;
    /// otherwise remembers it for [`flush_activity`](Self::flush_activity), so a busy workspace
    /// is announced once per interval with the newest time.
    fn note_activity(&mut self, workspace: WorkspaceId, at_ms: u64) {
        let interval = self.activity_interval_ms();
        let Some(entry) = self.workspaces.iter_mut().find(|w| w.info.id == workspace) else {
            return;
        };
        let due = entry.info.last_active_ms.saturating_add(interval);
        if due <= at_ms {
            self.apply_activity(workspace, at_ms);
            return;
        }
        entry.activity_pending = true;
        let at = Instant::now() + Duration::from_millis(due - at_ms);
        self.activity_flush_at = Some(self.activity_flush_at.map_or(at, |first| first.min(at)));
    }

    fn apply_activity(&mut self, workspace: WorkspaceId, at_ms: u64) {
        if let Some(entry) = self.workspaces.iter_mut().find(|w| w.info.id == workspace) {
            entry.activity_pending = false;
        }
        match self.update_workspace(workspace, |w| {
            w.last_active_ms = w.last_active_ms.max(at_ms);
        }) {
            Ok(info) => log_write(
                self.storage
                    .workspaces()
                    .set_last_active(workspace, info.last_active_ms),
                "workspace activity",
            ),
            Err(error) => {
                warn!(%workspace, message = %error.message, "could not announce workspace activity");
            }
        }
    }

    fn on_timer(&mut self) {
        let now = Instant::now();
        if self.flush_at.is_some_and(|at| at <= now) {
            self.flush_dirty();
        }
        if self.activity_flush_at.is_some_and(|at| at <= now) {
            self.flush_activity();
        }
        if self.tree_flush_at.is_some_and(|at| at <= now) {
            self.flush_tree_events();
        }
    }

    /// Announces the remembered activity of every workspace whose interval has passed and
    /// schedules the rest.
    fn flush_activity(&mut self) {
        self.activity_flush_at = None;
        let now = now_ms();
        let interval = self.activity_interval_ms();
        let mut ready = Vec::new();
        let mut wait: Option<u64> = None;
        for entry in &self.workspaces {
            if !entry.activity_pending {
                continue;
            }
            let due = entry.info.last_active_ms.saturating_add(interval);
            if due <= now {
                ready.push(entry.info.id);
            } else {
                wait = Some(wait.map_or(due - now, |w| w.min(due - now)));
            }
        }
        // Stamped with the time of the announcement: the activity happened within the last
        // interval, and the next announcement is a full interval away.
        for id in ready {
            self.apply_activity(id, now);
        }
        if let Some(wait) = wait {
            self.activity_flush_at = Some(Instant::now() + Duration::from_millis(wait));
        }
    }

    fn index_of(&self, id: WorkspaceId) -> Result<usize, ErrorBody> {
        self.workspaces
            .iter()
            .position(|w| w.info.id == id)
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
        let workspace_id = req.workspace_id.unwrap_or(self.home);
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
            agent: None,
            created_at_ms: now_ms(),
            restored: false,
            chat: None,
            placement: self.placement_of(spec.cwd.as_deref()),
        };
        self.publish(&Event::SessionUpdated {
            session: info.clone(),
        })?;
        let session_id = info.id;
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
            agent_message: None,
            agent_session_id: None,
            tree: TreeState::new(session_id),
            chat: None,
        });
        let index = self.sessions.len() - 1;
        // A new session is activity; the workspace announcement that follows the session's
        // first update carries the new time together with the new counts.
        self.touch_workspace(workspace_id);
        self.start_terminal(index);
        Ok(self.sessions[index].info.clone())
    }

    /// Records activity now without announcing it.
    fn touch_workspace(&mut self, id: WorkspaceId) {
        let now = now_ms();
        let Some(entry) = self.workspaces.iter_mut().find(|w| w.info.id == id) else {
            return;
        };
        entry.activity_pending = false;
        entry.info.last_active_ms = entry.info.last_active_ms.max(now);
        let last_active_ms = entry.info.last_active_ms;
        log_write(
            self.storage
                .workspaces()
                .set_last_active(id, last_active_ms),
            "workspace activity",
        );
    }

    /// Spawns the process for a `Starting` session and moves it to `Running` or `Failed`.
    fn start_terminal(&mut self, index: usize) {
        let restore_argv = self.sessions[index].restore_argv.take();
        let session = &self.sessions[index];
        let mut env: BTreeMap<String, String> = session.spec.env.clone();
        if let Some(shell) = &self.login_shell {
            env.entry("SHELL".to_owned())
                .or_insert_with(|| shell.clone());
        }
        // Daemon-managed variables win over whatever the client asked for.
        env.insert(
            "HUTAN_SOCKET".to_owned(),
            self.socket.to_string_lossy().into_owned(),
        );
        env.insert("HUTAN_SESSION_ID".to_owned(), session.info.id.to_string());
        env.insert("HUTAN_ENV".to_owned(), "1".to_owned());
        env.insert("HUTAN_BIN".to_owned(), self.hutan_bin.clone());
        env.insert(
            "HUTAN_WORKSPACE_ID".to_owned(),
            session.info.workspace_id.to_string(),
        );
        let mut config = TerminalConfig {
            argv: restore_argv.unwrap_or_else(|| session.spec.command.clone()),
            cwd: session.info.cwd.clone(),
            env,
            env_inherit: self.env_inherit,
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
        session.request_stop(&self.chats);
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
        if session.chat.is_some() {
            return Ok(self.restart_chat(index));
        }
        session.advance(SessionEvent::Restart);
        if let Some(old) = session.terminal.take() {
            // Attached clients are dropped; they re-attach to the new terminal id.
            self.hub.remove(old.id());
        }
        session.stopping = false;
        session.keep_live = None;
        session.info.restored = false;
        // A new process is a new reporting epoch: nothing the old one said applies.
        session.forget_agent(now_ms());
        session.tree.reset_seqs();
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

    // Agent reports ---------------------------------------------------------------------------

    fn report_agent(&mut self, req: AgentReportRequest) -> Result<AgentApplied, ErrorBody> {
        let index = self.session_index(req.session_id)?;
        agent_field("source", &req.source)?;
        agent_field("agent", &req.agent)?;
        if let Some(id) = &req.agent_session_id {
            agent_field("agent_session_id", id)?;
        }
        if let Some(node) = &req.node {
            check_node_report(node)?;
        }
        let resume = match &req.resume_argv {
            Some(argv) => {
                agent::validate_resume_argv(argv)
                    .map_err(|message| ErrorBody::new(ErrorCode::BadRequest, message))?;
                let json = agent::resume_ref_json(
                    &req.source,
                    &req.agent,
                    req.agent_session_id.as_deref(),
                    argv.clone(),
                )
                .map_err(|error| {
                    error!(%error, "could not encode a resume reference");
                    ErrorBody::new(ErrorCode::Internal, "could not encode the resume reference")
                })?;
                Some(json)
            }
            None => None,
        };

        let session = &mut self.sessions[index];
        let node_id = req.node.as_ref().map(|node| node.id.clone());
        // A hook can fire just as the process ends; there is nothing left to describe.
        if !session.info.state.is_live()
            || !session
                .tree
                .accept_seq(&req.source, node_id.as_deref(), req.seq)
        {
            return Ok(AgentApplied { applied: false });
        }
        let message = req
            .message
            .map(|message| clipped(&message, MAX_AGENT_MESSAGE_CHARS))
            .filter(|message| !message.is_empty());
        let same_agent = session
            .tree
            .tree()
            .root()
            .is_some_and(|root| root.agent == req.agent);
        let now = now_ms();
        session.tree.apply(
            node_update(
                &req.source,
                &req.agent,
                req.state,
                req.node,
                message.as_deref(),
            ),
            now,
        );
        // The message, conversation id and resume command belong to the main agent only.
        let about_root = node_id
            .as_deref()
            .is_none_or(|id| session.tree.tree().root_id() == Some(id));
        let mut resume_changed = false;
        if about_root {
            // The conversation id sticks to the agent until it says otherwise.
            session.agent_session_id = req
                .agent_session_id
                .or_else(|| session.agent_session_id.take().filter(|_| same_agent));
            session.agent_message = message;
            resume_changed = resume.is_some() && session.resume_ref_json != resume;
            if resume_changed {
                session.resume_ref_json = resume;
            }
        }
        self.tree_changed(index);
        if resume_changed {
            self.persist_session(index);
        }
        Ok(AgentApplied { applied: true })
    }

    fn release_agent(&mut self, req: AgentReleaseRequest) -> Result<AgentApplied, ErrorBody> {
        let index = self.session_index(req.session_id)?;
        agent_field("source", &req.source)?;
        if let Some(node) = &req.node {
            agent_field("node", node)?;
        }
        let session = &mut self.sessions[index];
        if !session
            .tree
            .accept_seq(&req.source, req.node.as_deref(), req.seq)
        {
            return Ok(AgentApplied { applied: false });
        }
        let about_root = req
            .node
            .as_deref()
            .is_none_or(|id| session.tree.tree().root_id() == Some(id));
        let outcome = req.outcome.unwrap_or(NodeOutcome::Completed);
        let node_released =
            session
                .tree
                .release(&req.source, req.node.as_deref(), outcome, now_ms());
        let mut resume_released = false;
        if about_root {
            resume_released = session
                .resume_ref_json
                .as_deref()
                .and_then(restore::resume_source)
                .is_some_and(|source| source == req.source);
            if resume_released {
                session.resume_ref_json = None;
            }
            if node_released {
                session.agent_message = None;
                session.agent_session_id = None;
            }
        }
        self.tree_changed(index);
        if resume_released {
            self.persist_session(index);
        }
        Ok(AgentApplied {
            applied: node_released || resume_released,
        })
    }

    fn stop_all(&mut self, forget: bool, reply: Reply<()>) {
        for session in self.sessions.iter_mut().filter(|s| s.info.state.is_live()) {
            // A session the user is already stopping stays stopped; the others die with the
            // daemon but remain in the restore set unless the user asked to forget them.
            if !session.stopping && !forget {
                session.keep_live = Some(session.info.state.clone());
            }
            session.request_stop(&self.chats);
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
            TerminalMeta::PromptMark(PromptMark::A) => {
                // The shell is prompting again: whatever agent was running in the foreground is
                // gone, even if it never got to say so.
                let resumable = self.sessions[index].forget_agent(now_ms());
                self.tree_changed(index);
                if resumable {
                    self.persist_session(index);
                }
            }
            TerminalMeta::Block(block) => self.emit(&Event::TerminalBlock {
                terminal_id: terminal,
                block,
            }),
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
        // The agent died with the process. A daemon shutdown keeps the resume reference and
        // the tree as it was: the next daemon starts the agent again and marks the tree
        // restored.
        let keep = session.keep_live.is_some();
        let resume = session.resume_ref_json.take().filter(|_| keep);
        if keep {
            session.agent_message = None;
            session.agent_session_id = None;
        } else {
            session.forget_agent(now_ms());
        }
        session.resume_ref_json = resume;
        session.stopping = false;
        self.sync_agent_summary(index);
        // The waiters and the event below must not show it still sharing a checkout.
        self.refresh_placement(index);
        let session = &mut self.sessions[index];
        let info = session.info.clone();
        let waiters = std::mem::take(&mut session.stop_waiters);
        self.emit_session(index);
        self.persist_session(index);
        self.tree_changed(index);
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

    // Agent trees -----------------------------------------------------------------------------

    /// The trees of one session (unknown id: `not_found`) or of every session that has one.
    /// Finished subtrees past their time are dropped first.
    fn agent_tree(&mut self, session: Option<SessionId>) -> Result<AgentTree, ErrorBody> {
        let indices: Vec<usize> = match session {
            Some(id) => vec![self.session_index(id)?],
            None => (0..self.sessions.len()).collect(),
        };
        let now = now_ms();
        let mut observations = Vec::new();
        for index in indices {
            self.sessions[index].tree.prune(now);
            if self.sessions[index].tree.event_due().is_some() {
                self.tree_changed(index);
            }
            if let Some(observation) = self.sessions[index].tree.tree().observation() {
                observations.push(observation);
            }
        }
        Ok(AgentTree { observations })
    }

    // Chat sessions ---------------------------------------------------------------------------

    fn chat_target(&self, id: SessionId) -> Result<bool, ErrorBody> {
        let session = &self.sessions[self.session_index(id)?];
        if session.chat.is_none() {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("session {id} is not a chat session"),
            ));
        }
        Ok(session.info.state.is_live())
    }

    fn create_chat(&mut self, req: ChatCreate, reply: Reply<SessionInfo>) {
        match self.begin_chat(req) {
            Ok(index) => self.spawn_chat_start(index, Some(reply)),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// Adds the session (state `starting`) and stores it; the agent is started by the caller.
    fn begin_chat(&mut self, req: ChatCreate) -> Result<usize, ErrorBody> {
        let harness = is_harness_agent(&req.agent);
        if req.agent != AGENT_CODEX && !harness && !is_gateway_agent(&req.agent) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "unsupported agent `{}` (supported: {AGENT_CODEX}, omp, claude, {AGENT_DIRECT}, {AGENT_ASSISTANT})",
                    req.agent
                ),
            ));
        }
        if req.mode.is_some() && !harness {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "`mode` is a session mode of a harness chat (omp, claude)",
            ));
        }
        if harness && (req.approval_policy.is_some() || req.sandbox.is_some()) {
            // Not faked: the harness decides what asks and what is sandboxed.
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "the approval policy and sandbox of a {} chat belong to the harness's own settings; \
                     Hutan answers the requests it makes",
                    req.agent
                ),
            ));
        }
        let direct = if is_gateway_agent(&req.agent) {
            Some(self.direct_target(&req)?)
        } else {
            None
        };
        let workspace_id = match req.workspace_id {
            Some(id) => {
                self.index_of(id)?;
                id
            }
            None => self
                .workspace_containing(&req.cwd)
                .map_or(self.home, |index| self.workspaces[index].info.id),
        };
        let spec = match direct {
            Some((provider_id, model, system_prompt)) => ChatSpec {
                agent: req.agent,
                model: Some(model),
                approval_policy: Default::default(),
                sandbox: Default::default(),
                thread_id: None,
                provider_id: Some(provider_id),
                system_prompt,
                mode: None,
            },
            None => ChatSpec {
                agent: req.agent,
                model: req.model.filter(|m| !m.trim().is_empty()),
                approval_policy: req.approval_policy.unwrap_or_default(),
                sandbox: req.sandbox.unwrap_or_default(),
                thread_id: None,
                provider_id: None,
                system_prompt: None,
                mode: req.mode.filter(|m| !m.trim().is_empty()),
            },
        };
        let folder = req
            .cwd
            .file_name()
            .map_or_else(|| "chat".to_owned(), |n| n.to_string_lossy().into_owned());
        let default_title = match (&spec.provider_id, &spec.model) {
            (Some(provider), Some(model)) => format!("{provider} · {model}"),
            _ => format!("{} · {folder}", spec.agent),
        };
        let chosen = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        let info = SessionInfo {
            id: SessionId::new(),
            workspace_id,
            kind: SessionKind::Harness,
            title: chosen.clone().unwrap_or_else(|| default_title.clone()),
            state: SessionState::Starting,
            command: vec![spec.agent.clone()],
            cwd: Some(req.cwd.clone()),
            terminal_id: None,
            agent: None,
            created_at_ms: now_ms(),
            restored: false,
            chat: Some(chat_info(&spec)),
            placement: self.placement_of(Some(&req.cwd)),
        };
        self.publish(&Event::SessionUpdated {
            session: info.clone(),
        })?;
        let session_id = info.id;
        self.sessions.push(SessionEntry {
            info,
            spec: TerminalSpec {
                command: vec![spec.agent.clone()],
                cwd: Some(req.cwd),
                ..TerminalSpec::default()
            },
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
            agent_message: None,
            agent_session_id: None,
            tree: TreeState::new(session_id),
            chat: Some(spec),
        });
        let index = self.sessions.len() - 1;
        let workspace = self.sessions[index].info.workspace_id;
        self.touch_workspace(workspace);
        // Stored before the agent runs: the journal's rows refer to this one.
        self.persist_session(index);
        self.sync_workspace(workspace);
        Ok(index)
    }

    /// What a direct chat talks to: its provider, the model (the request's, else the provider's
    /// default) and the system prompt.
    fn direct_target(
        &self,
        req: &ChatCreate,
    ) -> Result<(String, String, Option<String>), ErrorBody> {
        let bad = |message: String| ErrorBody::new(ErrorCode::BadRequest, message);
        let Some(provider_id) = req.provider_id.as_deref().filter(|p| !p.is_empty()) else {
            return Err(bad(
                "a direct chat needs a provider (`provider_id`, see `hutan provider ls`)".into(),
            ));
        };
        let provider = self.chats.providers().require(provider_id)?;
        let model = req
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .or(provider.default_model)
            .ok_or_else(|| {
                bad(format!(
                    "no model: name one, or give provider `{provider_id}` a default model"
                ))
            })?;
        let system_prompt = req
            .system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned);
        if system_prompt
            .as_ref()
            .is_some_and(|p| p.len() > MAX_SYSTEM_PROMPT_BYTES)
        {
            return Err(bad(format!(
                "the system prompt is longer than {MAX_SYSTEM_PROMPT_BYTES} bytes"
            )));
        }
        Ok((provider.id, model, system_prompt))
    }

    /// `chat.set_model`: the session's spec and info change now; the running chat is told, and
    /// the reply waits until its journal has the notice.
    fn set_chat_model(&mut self, req: ChatSetModel, reply: Reply<SessionInfo>) {
        let harness = self
            .session_index(req.session_id)
            .ok()
            .and_then(|index| self.sessions[index].chat.as_ref())
            .is_some_and(|spec| is_harness_agent(&spec.agent));
        if harness {
            self.switch_harness_model(req, reply);
            return;
        }
        match self.switch_chat_model(req) {
            Ok((index, switch)) => {
                let info = self.sessions[index].info.clone();
                let chats = self.chats.clone();
                let id = info.id;
                tokio::spawn(async move {
                    let result = match switch {
                        Some((provider_id, model)) => {
                            chats.set_model(id, provider_id, model).await.map(|()| info)
                        }
                        None => Ok(info),
                    };
                    let _ = reply.send(result);
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// `chat.set_model` of a harness chat: the agent has to accept the model, so the session's
    /// record changes only when it did (`on_chat_model_switched`).
    fn switch_harness_model(&mut self, req: ChatSetModel, reply: Reply<SessionInfo>) {
        let checked = (|| {
            let index = self.session_index(req.session_id)?;
            let session = &self.sessions[index];
            if !session.info.state.is_live() || !self.chats.is_running(req.session_id) {
                return Err(ErrorBody::new(
                    ErrorCode::InvalidState,
                    format!(
                        "chat session {} is not running (`hutan restart` it first)",
                        req.session_id
                    ),
                ));
            }
            let model = req.model.trim();
            if model.is_empty() {
                return Err(ErrorBody::new(ErrorCode::BadRequest, "the model is empty"));
            }
            let unchanged = session
                .chat
                .as_ref()
                .is_some_and(|spec| spec.model.as_deref() == Some(model));
            Ok((index, model.to_owned(), unchanged))
        })();
        let (index, model, unchanged) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        if unchanged {
            let _ = reply.send(Ok(self.sessions[index].info.clone()));
            return;
        }
        let id = req.session_id;
        let chats = self.chats.clone();
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let outcome = chats.set_model(id, String::new(), model.clone()).await;
            let command = Command::ChatModelSwitched {
                session: id,
                model,
                outcome,
                reply,
            };
            if let Some(tx) = mailbox.upgrade() {
                // A closed mailbox means the daemon is going down; the reply is dropped.
                let _ = tx.send(command).await;
            }
        });
    }

    fn on_chat_model_switched(
        &mut self,
        id: SessionId,
        model: String,
        outcome: Result<(), ErrorBody>,
        reply: Reply<SessionInfo>,
    ) {
        let result = match (outcome, self.session_index(id)) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(index)) => {
                let session = &mut self.sessions[index];
                if let Some(spec) = &mut session.chat {
                    spec.model = Some(model.clone());
                }
                if let Some(chat) = &mut session.info.chat {
                    chat.model = model;
                }
                self.emit_session(index);
                self.persist_session(index);
                Ok(self.sessions[index].info.clone())
            }
        };
        let _ = reply.send(result);
    }

    /// Validates and applies the switch; `None` means nothing changed.
    #[allow(clippy::type_complexity)]
    fn switch_chat_model(
        &mut self,
        req: ChatSetModel,
    ) -> Result<(usize, Option<(String, String)>), ErrorBody> {
        let index = self.session_index(req.session_id)?;
        let session = &self.sessions[index];
        let Some(spec) = &session.chat else {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("session {} is not a chat session", req.session_id),
            ));
        };
        if !is_gateway_agent(&spec.agent) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "only a direct or harness chat has a switchable model",
            ));
        }
        if !session.info.state.is_live() || !self.chats.is_running(req.session_id) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!(
                    "chat session {} is not running (`hutan restart` it first)",
                    req.session_id
                ),
            ));
        }
        let model = req.model.trim();
        if model.is_empty() {
            return Err(ErrorBody::new(ErrorCode::BadRequest, "the model is empty"));
        }
        let current_provider = spec.provider_id.clone().unwrap_or_default();
        let provider_id = match req.provider_id.as_deref().filter(|p| !p.is_empty()) {
            Some(provider) => self.chats.providers().require(provider)?.id,
            None => current_provider.clone(),
        };
        if provider_id == current_provider && spec.model.as_deref() == Some(model) {
            return Ok((index, None));
        }
        let model = model.to_owned();
        let session = &mut self.sessions[index];
        if let Some(spec) = &mut session.chat {
            spec.provider_id = Some(provider_id.clone());
            spec.model = Some(model.clone());
        }
        if let Some(chat) = &mut session.info.chat {
            chat.provider_id = Some(provider_id.clone());
            chat.model.clone_from(&model);
        }
        self.emit_session(index);
        self.persist_session(index);
        Ok((index, Some((provider_id, model))))
    }

    /// Starts the agent of a `Starting` chat session on a task; the outcome comes back as
    /// `ChatStarted`.
    fn spawn_chat_start(&mut self, index: usize, reply: Option<Reply<SessionInfo>>) {
        let session = &self.sessions[index];
        let Some(spec) = session.chat.clone() else {
            return;
        };
        let id = session.info.id;
        let cwd = session.info.cwd.clone().unwrap_or_default();
        let chats = self.chats.clone();
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let outcome = chats.start(id, cwd, &spec).await;
            let command = Command::ChatStarted {
                session: id,
                outcome,
                reply,
            };
            match mailbox.upgrade() {
                Some(tx) => {
                    // A closed mailbox means the daemon is going down.
                    let _ = tx.send(command).await;
                }
                None => {
                    // Nobody left to adopt the agent.
                    chats.stop(id);
                }
            }
        });
    }

    fn on_chat_started(
        &mut self,
        id: SessionId,
        outcome: Result<Started, ErrorBody>,
        reply: Option<Reply<SessionInfo>>,
    ) {
        let respond = |reply: Option<Reply<SessionInfo>>,
                       result: Result<SessionInfo, ErrorBody>| {
            if let Some(reply) = reply {
                let _ = reply.send(result);
            }
        };
        let Ok(index) = self.session_index(id) else {
            // The workspace was closed meanwhile; there is nothing to attach the agent to.
            self.chats.stop(id);
            respond(
                reply,
                Err(ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("session {id} not found"),
                )),
            );
            return;
        };
        let session = &mut self.sessions[index];
        match outcome {
            Ok(started) => {
                if let Some(spec) = &mut session.chat {
                    spec.thread_id = Some(started.thread_id.clone());
                    spec.model = Some(started.model.clone());
                }
                if let Some(chat) = &mut session.info.chat {
                    chat.model = started.model;
                    if !is_gateway_agent(&chat.agent) {
                        chat.agent_session_id = Some(started.thread_id);
                    }
                }
                session.advance(SessionEvent::Ready);
                if session.stopping {
                    // A stop arrived while the agent was starting; it goes down right away
                    // and the exit finishes the session.
                    self.chats.stop(id);
                    respond(
                        reply,
                        Err(ErrorBody::new(
                            ErrorCode::InvalidState,
                            "the session was stopped while it started",
                        )),
                    );
                } else {
                    self.sync_chat_state(index);
                    respond(reply, Ok(self.sessions[index].info.clone()));
                }
                self.emit_session(index);
                self.persist_session(index);
            }
            Err(error) => {
                warn!(session = %id, message = %error.message, "chat agent failed to start");
                let stopping = session.stopping;
                session.advance(if stopping {
                    SessionEvent::StopCompleted
                } else {
                    SessionEvent::Failed {
                        reason: error.message.clone(),
                    }
                });
                session.stopping = false;
                let waiters = std::mem::take(&mut session.stop_waiters);
                let info = session.info.clone();
                self.emit_session(index);
                self.persist_session(index);
                respond(reply, Err(error));
                for waiter in waiters {
                    let _ = waiter.send(Ok(info.clone()));
                }
                self.settle_stop_all();
            }
        }
    }

    /// Records what the pump reports and moves the session's state to match.
    fn on_chat_state(&mut self, id: SessionId, turn: Option<bool>, pending: u32) {
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let before = self.sessions[index].info.clone();
        if let Some(chat) = &mut self.sessions[index].info.chat {
            if let Some(active) = turn {
                chat.turn_active = active;
            }
            chat.pending_approvals = pending;
        }
        self.sync_chat_state(index);
        if self.sessions[index].info != before {
            self.emit_session(index);
            self.persist_session(index);
        }
    }

    /// An update of the session's agent tree from its adapter (the main agent's thread and the
    /// subagents' threads).
    fn on_chat_node(&mut self, id: SessionId, update: NodeUpdate) {
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let session = &mut self.sessions[index];
        // The agent's last words can arrive after the session was stopped.
        if !session.info.state.is_live() {
            return;
        }
        session.tree.apply(update, now_ms());
        self.tree_changed(index);
    }

    /// Moves a live chat session to the state its turn and approval counts call for:
    /// `waiting_approval` while anything is pending, else `running` during a turn, else `idle`.
    /// A session still starting keeps its state until it is ready.
    fn sync_chat_state(&mut self, index: usize) {
        let session = &mut self.sessions[index];
        let Some(chat) = &session.info.chat else {
            return;
        };
        let target = if chat.pending_approvals > 0 {
            SessionState::WaitingApproval
        } else if chat.turn_active {
            SessionState::Running
        } else {
            SessionState::Idle
        };
        // At most three steps separate any two of the states.
        for _ in 0..3 {
            let event = match (&session.info.state, &target) {
                (SessionState::Idle, SessionState::Running | SessionState::WaitingApproval) => {
                    SessionEvent::TurnStarted
                }
                (SessionState::Running, SessionState::WaitingApproval) => {
                    SessionEvent::ApprovalRequested
                }
                (SessionState::WaitingApproval, SessionState::Running | SessionState::Idle) => {
                    SessionEvent::ApprovalResolved
                }
                (SessionState::Running, SessionState::Idle) => SessionEvent::TurnFinished,
                _ => return,
            };
            session.advance(event);
        }
    }

    fn on_chat_exited(&mut self, id: SessionId, exit: &ExitInfo) {
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let session = &mut self.sessions[index];
        if !session.info.state.is_live() {
            return;
        }
        session.advance(if session.stopping {
            SessionEvent::StopCompleted
        } else if exit.code == Some(0) && exit.signal.is_none() {
            SessionEvent::ProcessExited {
                code: exit.code,
                signal: exit.signal,
            }
        } else {
            SessionEvent::Failed {
                reason: exit_reason(exit),
            }
        });
        if let Some(chat) = &mut session.info.chat {
            chat.turn_active = false;
            chat.pending_approvals = 0;
        }
        session.stopping = false;
        // A daemon shutdown keeps the tree as it was: the next daemon marks it restored.
        if session.keep_live.is_none() {
            session.tree.session_ended(now_ms());
        }
        self.sync_agent_summary(index);
        let session = &mut self.sessions[index];
        let info = session.info.clone();
        let waiters = std::mem::take(&mut session.stop_waiters);
        self.emit_session(index);
        self.persist_session(index);
        self.tree_changed(index);
        self.emit(&Event::SessionExited {
            session_id: id,
            code: exit.code,
            signal: exit.signal,
        });
        for waiter in waiters {
            let _ = waiter.send(Ok(info.clone()));
        }
        self.settle_stop_all();
    }

    /// A stopped or failed chat session comes back: the agent resumes the stored conversation.
    fn restart_chat(&mut self, index: usize) -> SessionInfo {
        let session = &mut self.sessions[index];
        session.advance(SessionEvent::Restart);
        session.stopping = false;
        session.keep_live = None;
        session.info.restored = false;
        if let Some(chat) = &mut session.info.chat {
            chat.turn_active = false;
            chat.pending_approvals = 0;
        }
        self.emit_session(index);
        self.persist_session(index);
        self.spawn_chat_start(index, None);
        self.sessions[index].info.clone()
    }

    /// Rebuilds a stored chat session. A live one is started again (the agent resumes its
    /// conversation, the journal keeps the history) unless `mode` says to list it as stopped.
    fn restore_chat(&mut self, row: SessionRow, mode: RestoreMode) -> ChatRestore {
        let was_live = row.state.is_live();
        let (spec, unusable) = match serde_json::from_str::<ChatSpec>(&row.spec_json) {
            Ok(spec) => (spec, None),
            Err(error) => {
                warn!(session = %row.id, %error, "stored chat spec is unreadable");
                (
                    ChatSpec {
                        agent: AGENT_CODEX.to_owned(),
                        model: None,
                        approval_policy: Default::default(),
                        sandbox: Default::default(),
                        thread_id: None,
                        provider_id: None,
                        system_prompt: None,
                        mode: None,
                    },
                    Some(format!("stored spec unreadable: {error}")),
                )
            }
        };
        let mut state = row.state.clone();
        let mut ended_at_ms = row.ended_at_ms;
        let mut action = ChatRestoreKind::Keep;
        if was_live {
            ended_at_ms = Some(now_ms());
            match (unusable, mode) {
                (Some(reason), _) => {
                    state = SessionState::Failed { reason };
                    action = ChatRestoreKind::Rewrite;
                }
                (None, RestoreMode::Stopped) => {
                    state = SessionState::Stopped;
                    action = ChatRestoreKind::Rewrite;
                }
                (None, RestoreMode::Respawn) => {
                    state = SessionState::Starting;
                    ended_at_ms = None;
                    action = ChatRestoreKind::Start;
                }
            }
        }
        let default_title = format!("{} · chat", spec.agent);
        self.sessions.push(SessionEntry {
            info: SessionInfo {
                id: row.id,
                workspace_id: row.workspace_id,
                kind: SessionKind::Harness,
                title: row.title,
                state,
                command: vec![spec.agent.clone()],
                cwd: row.cwd.clone(),
                terminal_id: None,
                agent: None,
                created_at_ms: row.created_at_ms,
                // The journal has the whole conversation: nothing is gone.
                restored: false,
                chat: Some(chat_info(&spec)),
                placement: None,
            },
            spec: TerminalSpec {
                command: vec![spec.agent.clone()],
                cwd: row.cwd,
                ..TerminalSpec::default()
            },
            default_title,
            title_locked: row.title_locked,
            osc_title: None,
            terminal: None,
            stopping: false,
            stop_waiters: Vec::new(),
            ended_at_ms,
            resume_ref_json: None,
            restore_argv: None,
            keep_live: None,
            dirty: false,
            agent_message: None,
            agent_session_id: None,
            tree: TreeState::new(row.id),
            chat: Some(spec),
        });
        let index = self.sessions.len() - 1;
        match action {
            ChatRestoreKind::Start => ChatRestore::Start(index),
            ChatRestoreKind::Rewrite => ChatRestore::Rewrite(index),
            ChatRestoreKind::Keep => ChatRestore::Keep,
        }
    }

    // Storage ---------------------------------------------------------------------------------

    /// Rebuilds the sessions from their rows. Live ones are spawned again (`Respawn`) or listed
    /// as stopped; ended ones stay as they were, marked `restored` because their screen is gone.
    fn restore_sessions(
        &mut self,
        rows: Vec<SessionRow>,
        mode: RestoreMode,
        trees: HashMap<SessionId, ObservationTree>,
    ) {
        let mut respawn = Vec::new();
        let mut rewrite = Vec::new();
        let mut chat_starts = Vec::new();
        for row in rows {
            if !self
                .workspaces
                .iter()
                .any(|w| w.info.id == row.workspace_id)
            {
                warn!(session = %row.id, "stored session has no workspace; skipping it");
                continue;
            }
            if row.kind == SessionKind::Harness {
                match self.restore_chat(row, mode) {
                    ChatRestore::Start(index) => chat_starts.push(index),
                    ChatRestore::Rewrite(index) => rewrite.push(index),
                    ChatRestore::Keep => {}
                }
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
            let state_is_live = state.is_live();
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
                    agent: None,
                    restored,
                    chat: None,
                    placement: None,
                },
                spec,
                default_title,
                title_locked: row.title_locked,
                osc_title: None,
                terminal: None,
                stopping: false,
                stop_waiters: Vec::new(),
                ended_at_ms,
                // Only a session that comes back to life resumes; the others forgot their agent.
                resume_ref_json: row.resume_ref_json.filter(|_| state_is_live),
                restore_argv,
                keep_live: None,
                dirty: false,
                agent_message: None,
                agent_session_id: None,
                tree: TreeState::new(row.id),
                chat: None,
            });
            let index = self.sessions.len() - 1;
            match action {
                Some(true) => respawn.push(index),
                Some(false) => rewrite.push(index),
                None => {}
            }
        }
        // Before the respawns announce anything: the counts of a restored workspace are right
        // from the start.
        self.recount_sessions();
        self.restore_trees(trees);
        self.place_restored_sessions();
        // `start_terminal` stores the session itself once it is running.
        for index in respawn {
            self.start_terminal(index);
        }
        for index in chat_starts {
            self.spawn_chat_start(index, None);
        }
        for index in rewrite {
            self.persist_session(index);
        }
    }

    /// Gives every session the tree stored for it, brought up to date: a session that comes
    /// back may have its main agent running again (unknown until it says so), one that does
    /// not has lost every agent. Finished subtrees past their time are dropped.
    fn restore_trees(&mut self, mut trees: HashMap<SessionId, ObservationTree>) {
        let now = now_ms();
        for session in &mut self.sessions {
            if let Some(tree) = trees.remove(&session.info.id) {
                session.tree = TreeState::from_tree(tree);
            }
            if session.info.state.is_live() {
                session.tree.restored(now);
            } else {
                session.tree.ended_silently(now);
            }
            session.tree.prune_silently(now);
            if session.tree.is_dirty() {
                self.flush_at
                    .get_or_insert_with(|| Instant::now() + DIRTY_WRITE_INTERVAL);
            }
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
        let spec_json = match &session.chat {
            Some(chat) => serde_json::to_string(chat),
            None => serde_json::to_string(&spec),
        };
        SessionRow {
            id: session.info.id,
            workspace_id: session.info.workspace_id,
            kind: session.info.kind,
            title: session.info.title.clone(),
            title_locked: session.title_locked,
            spec_json: spec_json.unwrap_or_else(|error| {
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
            if self.sessions[index].tree.take_dirty() {
                self.persist_tree(index);
            }
        }
    }

    // Agent trees -----------------------------------------------------------------------------

    /// Writes the session's tree now (or removes its row once the tree is empty).
    fn persist_tree(&self, index: usize) {
        let session = &self.sessions[index];
        let id = session.info.id;
        let result = match session.tree.stored() {
            Some(json) => self.storage.agent_trees().upsert(id, json, now_ms()),
            None => self.storage.agent_trees().delete(id),
        };
        log_write(result, "an agent tree");
    }

    /// Recomputes the session's `agent` summary from its tree. Returns whether clients would
    /// see a difference (the timestamp aside).
    fn sync_agent_summary(&mut self, index: usize) -> bool {
        let session = &mut self.sessions[index];
        let next = tree::summary(
            session.info.state.is_live(),
            session.tree.tree(),
            session.agent_message.as_deref(),
            session.agent_session_id.as_deref(),
        );
        let changed = !tree::same_summary(session.info.agent.as_ref(), next.as_ref());
        session.info.agent = next;
        changed
    }

    /// The agents of a session changed (its tree took a report, a release, an exit): the
    /// summary older clients read is refreshed and announced when it differs, the tree's own
    /// change is announced as far as the rate limit allows, and the stored copy is queued.
    fn tree_changed(&mut self, index: usize) {
        if self.sync_agent_summary(index) {
            self.emit_session(index);
        }
        self.publish_tree_event(index, Instant::now());
        if self.sessions[index].tree.is_dirty() {
            self.flush_at
                .get_or_insert_with(|| Instant::now() + DIRTY_WRITE_INTERVAL);
        }
        self.schedule_tree_events();
    }

    /// Announces what changed in the session's tree, unless the last announcement was too
    /// recent (then the timer does it).
    fn publish_tree_event(&mut self, index: usize, now: Instant) {
        if let Some(event) = self.sessions[index].tree.take_event(now) {
            self.emit(&event);
        }
    }

    fn schedule_tree_events(&mut self) {
        self.tree_flush_at = self
            .sessions
            .iter()
            .filter_map(|s| s.tree.event_due())
            .min();
    }

    /// The timer for held-back tree announcements went off.
    fn flush_tree_events(&mut self) {
        let now = Instant::now();
        for index in 0..self.sessions.len() {
            self.publish_tree_event(index, now);
        }
        self.schedule_tree_events();
    }

    // Events ----------------------------------------------------------------------------------

    /// Announces the session and, when the change moved its workspace's counts, the workspace.
    fn emit_session(&mut self, index: usize) {
        self.refresh_placement(index);
        self.emit(&Event::SessionUpdated {
            session: self.sessions[index].info.clone(),
        });
        self.sync_workspace(self.sessions[index].info.workspace_id);
    }

    // Placement -------------------------------------------------------------------------------

    /// The placement of a session working in `dir`, before anyone shares it.
    fn placement_of(&mut self, dir: Option<&Path>) -> Option<Placement> {
        dir.and_then(|dir| self.placements.placement(dir))
    }

    /// Derives the placement of session `index` from its directory again (its checkout may
    /// have changed: it moved, a branch was switched) and who shares it, and announces the
    /// other sessions whose placement changed. The session itself is the caller's to announce.
    /// Returns whether its own placement changed.
    fn refresh_placement(&mut self, index: usize) -> bool {
        let before = self.sessions[index].info.placement.clone();
        let cwd = self.sessions[index].info.cwd.clone();
        let mut next = self.placement_of(cwd.as_deref());
        if let (Some(next), Some(before)) = (&mut next, &before) {
            // Kept until the sharing is worked out below; one change, one announcement.
            next.shared_with.clone_from(&before.shared_with);
        }
        self.sessions[index].info.placement = next;
        for other in self.sync_shared() {
            if other != index {
                self.emit(&Event::SessionUpdated {
                    session: self.sessions[other].info.clone(),
                });
            }
        }
        self.sessions[index].info.placement != before
    }

    /// Works out which sessions share a checkout and stores it; returns the sessions whose
    /// `shared_with` changed.
    fn sync_shared(&mut self) -> Vec<usize> {
        let occupants: Vec<Occupant<'_>> = self
            .sessions
            .iter()
            .map(|session| Occupant {
                id: session.info.id,
                live: session.info.state.is_live(),
                agent: session.info.agent.is_some(),
                root: session.info.placement.as_ref().map(|p| p.root.as_path()),
            })
            .collect();
        let shared = shared_with(&occupants);
        drop(occupants);
        let mut changed = Vec::new();
        for (index, shared) in shared.into_iter().enumerate() {
            if let Some(placement) = &mut self.sessions[index].info.placement
                && placement.shared_with != shared
            {
                placement.shared_with = shared;
                changed.push(index);
            }
        }
        changed
    }

    /// `worktree.*` created or removed checkouts: what was learned of directories is stale.
    fn invalidate_placements(&mut self) {
        self.placements.clear();
        for index in 0..self.sessions.len() {
            if self.refresh_placement(index) {
                self.emit_session(index);
            }
        }
    }

    /// Derives the placement of every restored session; nobody listens yet, so nothing is
    /// announced.
    fn place_restored_sessions(&mut self) {
        for index in 0..self.sessions.len() {
            let cwd = self.sessions[index].info.cwd.clone();
            self.sessions[index].info.placement = self.placement_of(cwd.as_deref());
        }
        self.sync_shared();
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
    // Output is activity of the session's workspace, reported at most once per
    // `ACTIVITY_NOTIFY_INTERVAL`; damage that arrives meanwhile is picked up afterwards.
    let mut damage = terminal.damage();
    let mut quiet = std::pin::pin!(sleep_until(Instant::now()));
    let mut throttled = false;
    let mut watching_output = true;
    let status = loop {
        tokio::select! {
            biased;
            () = &mut quiet, if throttled => throttled = false,
            changed = damage.changed(), if watching_output && !throttled => {
                if changed.is_err() {
                    watching_output = false;
                } else {
                    if !send(Command::Activity(id)).await {
                        return;
                    }
                    throttled = true;
                    quiet.as_mut().reset(Instant::now() + ACTIVITY_NOTIFY_INTERVAL);
                }
            }
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

/// `source`, `agent`, and `agent_session_id` are short, non-empty identifiers.
fn agent_field(name: &str, value: &str) -> Result<(), ErrorBody> {
    if value.is_empty() || value.len() > MAX_AGENT_FIELD_BYTES {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("`{name}` must be 1 to {MAX_AGENT_FIELD_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// The first `max` characters of `text`.
fn clipped(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// What the daemon checks of the node of a report: ids are short identifiers, `usage` says what
/// its numbers mean. Free text is clipped, not refused.
fn check_node_report(node: &NodeReport) -> Result<(), ErrorBody> {
    agent_field("node.id", &node.id)?;
    if let Some(parent) = &node.parent {
        agent_field("node.parent", parent)?;
    }
    if let Some(usage) = &node.usage
        && (usage.kind.is_empty() || usage.kind.len() > MAX_USAGE_KIND_BYTES)
    {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("`node.usage.kind` must be 1 to {MAX_USAGE_KIND_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// The reducer's input for a report. A report with a state replaces what the node is doing
/// (`activity`, else the report's `message`); one without only changes what it carries.
fn node_update(
    source: &str,
    agent: &str,
    state: Option<AgentState>,
    node: Option<NodeReport>,
    message: Option<&str>,
) -> NodeUpdate {
    let mut update = NodeUpdate {
        source: source.to_owned(),
        agent: agent.to_owned(),
        state: state.map(tree::node_state),
        ..NodeUpdate::default()
    };
    let mut text = message.map(str::to_owned);
    if let Some(node) = node {
        update.id = Some(node.id);
        update.parent = node.parent;
        update.kind = node.kind;
        update.name = node.name.map(|name| clipped(&name, MAX_NODE_TEXT_CHARS));
        update.agent_type = node
            .agent_type
            .map(|agent_type| clipped(&agent_type, MAX_NODE_TEXT_CHARS));
        update.outcome = node.outcome;
        update.model = node.model.map(|model| clipped(&model, MAX_NODE_TEXT_CHARS));
        update.usage = node.usage;
        update.transcript_path = node
            .transcript_path
            .filter(|path| !path.is_empty() && path.len() <= MAX_TRANSCRIPT_PATH_BYTES);
        text = node.activity.or(text);
    }
    update.activity = match text {
        Some(text) => Patch::Set(text),
        None if state.is_some() => Patch::Clear,
        None => Patch::Keep,
    };
    update
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

enum ChatRestoreKind {
    Keep,
    Rewrite,
    Start,
}

/// What [`SessionRegistry::restore_chat`] left to do for the session it added.
enum ChatRestore {
    /// Nothing: the session is shown as it was.
    Keep,
    /// Its state changed; write it.
    Rewrite(usize),
    /// Start its agent again.
    Start(usize),
}

fn chat_info(spec: &ChatSpec) -> ChatInfo {
    ChatInfo {
        agent: spec.agent.clone(),
        model: spec.model.clone().unwrap_or_default(),
        approval_policy: spec.approval_policy,
        sandbox: spec.sandbox,
        turn_active: false,
        pending_approvals: 0,
        provider_id: spec.provider_id.clone(),
        agent_session_id: (!is_gateway_agent(&spec.agent))
            .then(|| spec.thread_id.clone())
            .flatten(),
    }
}

/// Why a chat agent that ended on its own is a failure.
fn exit_reason(exit: &ExitInfo) -> String {
    let how = match (exit.code, exit.signal) {
        (_, Some(signal)) => format!("killed by signal {signal}"),
        (Some(code), None) => format!("exited with code {code}"),
        (None, None) => "exited".to_owned(),
    };
    let tail = exit.stderr_tail.trim();
    if tail.is_empty() {
        format!("the agent {how}")
    } else {
        format!("the agent {how}: {tail}")
    }
}

/// argv[0]'s basename, or the login shell's name when the spec runs the default shell.
fn default_title(spec: &TerminalSpec) -> String {
    let program = match spec.command.first() {
        Some(program) => program.clone(),
        None => hutan_terminal::default_shell(&spec.env),
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
