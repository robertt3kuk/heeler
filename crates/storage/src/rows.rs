//! Typed rows. Free-form payloads (`spec`, `resume_ref`) are opaque JSON strings so this crate
//! stays independent of the protocol and of agent integrations.

use std::path::PathBuf;

use hutan_core::{ApprovalId, Epoch, SessionId, SessionKind, SessionState, WorkspaceId};
use serde::{Deserialize, Serialize};

/// What a workspace is for. The database holds at most one `Home`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Home,
    Project,
}

/// Who created a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatedBy {
    User,
    Cli,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub id: WorkspaceId,
    pub name: String,
    pub roots: Vec<PathBuf>,
    pub kind: WorkspaceKind,
    pub pinned: bool,
    pub created_by: CreatedBy,
    pub created_at_ms: u64,
    /// Latest session activity.
    pub last_active_ms: u64,
    /// Sort key for listing; unique ordering is the caller's business.
    pub position: i64,
}

/// A project remembered after its workspace was closed (`recent_projects`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentProject {
    /// The project's primary root.
    pub path: PathBuf,
    pub name: String,
    pub last_opened_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub kind: SessionKind,
    pub title: String,
    /// The user named the session; terminal titles no longer rename it.
    pub title_locked: bool,
    /// What a restart spawns again (JSON written by the daemon).
    pub spec_json: String,
    pub cwd: Option<PathBuf>,
    /// The state at the last write. A live state here after the daemon stopped means the
    /// session was alive then.
    pub state: SessionState,
    pub created_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    /// Native resume reference of an agent session (JSON), used instead of `spec_json` on restore.
    pub resume_ref_json: Option<String>,
}

/// One daemon run recorded in `app_meta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochRecord {
    pub epoch: Epoch,
    pub started_at_ms: u64,
}

/// One journaled event of a chat session. The event itself is JSON (the core `AgentEvent`) so
/// this crate stays independent of the event model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatItemRow {
    pub session_id: SessionId,
    pub seq: u64,
    pub ts_ms: u64,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    /// The event's `type`.
    pub kind: String,
    pub event_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatTurnStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTurnRow {
    pub session_id: SessionId,
    pub turn_id: String,
    pub provider_turn_id: Option<String>,
    pub status: ChatTurnStatus,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Selected,
    Expired,
}

/// A permission request and, once it is over, how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRow {
    pub id: ApprovalId,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub kind: String,
    pub title: String,
    /// The options the provider offered (JSON array).
    pub options_json: String,
    pub requested_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
    pub outcome: Option<ApprovalOutcome>,
    pub option_id: Option<String>,
    /// Which client answered (`cli`, `desktop`, ...); absent for expiries.
    pub decided_by: Option<String>,
}

/// A model provider's configuration. The credential is not here: it is in the Keychain, and
/// `keychain_ref` only names where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    /// A short slug, unique; also the Keychain account.
    pub id: String,
    /// `openai`, `anthropic`, `ollama` or `openai-compatible`.
    pub kind: String,
    pub name: String,
    pub base_url: String,
    pub default_model: Option<String>,
    pub keychain_ref: String,
    pub created_at_ms: u64,
}
