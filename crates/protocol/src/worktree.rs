//! Worktrees and session placement (protocol 0.14, capability `worktree`): DTOs of
//! `worktree.*`, the `worktree.changed` event and [`Placement`] on `SessionInfo`
//! (`docs/protocol.md` § Worktrees and placement, `docs/orchestration.md`).
//!
//! git is the source of truth: the daemon lists worktrees with `git worktree list` and only
//! remembers which ones it created itself.

use std::path::PathBuf;

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// `worktree.create`: a checkout of `branch` next to the repository. Answered with
/// [`WorktreeCreated`].
///
/// `branch` is created from `base` when it does not exist, else it is checked out. Errors:
/// `bad_request` (not a valid branch name, no git repository, `path` not absolute), `conflict`
/// (the path exists, or the branch is checked out in another worktree; the message names
/// where), `not_found` (workspace), `invalid_state` with git's message when git refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreate {
    pub workspace_id: WorkspaceId,
    /// A directory inside the repository; absent: the first root of the workspace that is a git
    /// repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    pub branch: String,
    /// The ref a new branch starts from; absent: `HEAD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Absolute and not existing; absent: `<parent of repo>/<repo name>-wt/<branch with / as ->`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// Result of `worktree.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreated {
    pub worktree: WorktreeInfo,
}

/// `worktree.list`: the worktrees of the repositories of one workspace, or of every workspace.
/// Answered with [`WorktreeList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorktreeListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// Result of `worktree.list`: per repository its main worktree first, then the others in
/// git's order; repositories in workspace order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeList {
    pub worktrees: Vec<WorktreeInfo>,
}

/// One worktree as git reports it, plus what Hutan knows about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// Canonical directory.
    pub path: PathBuf,
    /// The repository's main worktree (equal to `path` for the main worktree itself).
    pub repo_root: PathBuf,
    /// Absent for a detached HEAD (or a bare repository).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit checked out (full hash).
    pub head: String,
    /// The repository's main worktree, which cannot be removed.
    pub main: bool,
    /// Hutan made it (`worktree.create`).
    pub created_by_hutan: bool,
    /// Uncommitted changes (tracked, or untracked and not ignored).
    pub dirty: bool,
    /// Live sessions whose checkout is this worktree.
    pub live_sessions: Vec<SessionId>,
    /// `git worktree lock` was used.
    pub locked: bool,
    /// The directory is gone; `git worktree prune` would drop the entry.
    pub prunable: bool,
}

/// `worktree.remove`: removes the worktree directory, optionally its branch. Answered with
/// [`WorktreeRemoved`].
///
/// Errors: `invalid_state` (with the reason) for the main worktree, a worktree with live
/// sessions inside it, or uncommitted changes without `force`; `not_found` for a path git does
/// not list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemove {
    pub path: PathBuf,
    /// Remove even with uncommitted changes (never with live sessions).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    /// Also delete the branch with `git branch -d` (never `-D`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delete_branch: bool,
}

/// Result of `worktree.remove`: `{}` unless a branch was asked to go and stayed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorktreeRemoved {
    /// `delete_branch` was set and git kept the branch (typically: it is not merged); git's
    /// reason. The worktree is removed anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_kept: Option<String>,
}

/// Where a session works: the git checkout containing its cwd (`SessionInfo.placement`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub kind: PlacementKind,
    /// The checkout's top level (canonical).
    pub root: PathBuf,
    /// Absent for a detached HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Other live sessions with an agent in this checkout, while this session has one too:
    /// two agents writing to one checkout.
    #[serde(default)]
    pub shared_with: Vec<SessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementKind {
    /// The repository's main checkout.
    Main,
    /// A linked worktree.
    Worktree,
}
