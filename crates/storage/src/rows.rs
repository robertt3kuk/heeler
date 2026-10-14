//! Typed rows. Free-form payloads (`spec`, `resume_ref`) are opaque JSON strings so this crate
//! stays independent of the protocol and of agent integrations.

use std::path::PathBuf;

use heeler_core::{Epoch, SessionId, SessionKind, SessionState, WorkspaceId};
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
