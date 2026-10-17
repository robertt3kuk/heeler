//! Tasks with dependencies (protocol 0.16, capability `task`): DTOs of `task.*` and the
//! `task.updated` / `task.removed` events (`docs/protocol.md` § Tasks, `docs/orchestration.md`
//! § Tasks). The model itself — [`Task`], its states, placement and run — lives in
//! `hutan-core`.
//!
//! Errors, in short: `not_found` (an unknown task, dependency or workspace), `bad_request`
//! (invalid fields; a dependency cycle, whose message names it; a dependency in another
//! workspace), `invalid_state` (the task is not in a state the request needs), `conflict` (a
//! task that others depend on is deleted without `cascade`).
//!
//! Protocol 0.25, capability `task.review`: the delivery facet of [`Task`] (`delivery`,
//! `delivery_history`, `after_policy`), [`TaskReview`] and `worktree.commit` (see
//! `worktree.rs`). Delivery moves are announced with `task.updated`.

use hutan_core::{
    AfterPolicy, ReviewDecision, SessionId, Task, TaskId, TaskPlacement, TaskRun, TaskState,
    WorkspaceId,
};
use serde::{Deserialize, Serialize};

use crate::messages::SessionInfo;

/// `task.create`: a new task in `todo` (shown `ready` when it has no open dependency). Answered
/// with [`TaskResult`].
///
/// `title` is 1 to 200 characters after trimming, `prompt` at most 64 KiB. `depends_on` names
/// tasks of the same workspace. A `chat` run needs a `prompt` (its first message), and a
/// `terminal` run a non-empty `argv`. Without `placement` the task runs in the main checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCreate {
    pub workspace_id: WorkspaceId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<TaskPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<TaskRun>,
    /// Start when the task is ready and `[tasks] max_parallel` has a free slot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_start: bool,
    /// What the task waits for in each dependency (protocol 0.25); absent: `done`.
    #[serde(default, skip_serializing_if = "AfterPolicy::is_done")]
    pub after_policy: AfterPolicy,
}

/// `task.list`: the tasks of one workspace, or of all, in dependency order (a task after the
/// tasks it depends on; otherwise oldest first), optionally only those in `state` (the state
/// as shown: `ready` and `blocked` are matched too). Answered with [`TaskList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskState>,
}

/// Result of `task.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskList {
    pub tasks: Vec<Task>,
}

/// `task.update`: changes the fields that are present. Answered with [`TaskResult`].
///
/// - `prompt` replaces the prompt; an empty one removes it. `run` replaces the run.
/// - `depends_on` replaces the dependency list; a cycle is refused.
/// - Fields other than `state` need the task not to be running (`invalid_state`).
/// - `state` is one of `cancelled` (stops the live session of a running task), `done` (the user
///   says it is done; from every state but `cancelled`) and `todo` (an ended task is run
///   again; its earlier sessions stay listed). Anything else is `bad_request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUpdate {
    pub id: TaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<TaskId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<TaskPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<TaskRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_start: Option<bool>,
    /// Protocol 0.25: what the task waits for in each dependency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_policy: Option<AfterPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskState>,
}

/// `task.delete`: removes a task (its sessions and worktree stay). A task that others depend on
/// is refused with `conflict`, naming them, unless `cascade` deletes them too; a running task is
/// refused with `invalid_state`. Answered with [`TaskDeleted`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDelete {
    pub id: TaskId,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cascade: bool,
}

/// Result of `task.delete`: every task that went (`task.removed` was published for each).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDeleted {
    pub removed: Vec<TaskId>,
}

/// `task.review` (protocol 0.25, capability `task.review`): a person's verdict on a `done`
/// task. Answered with [`TaskReviewed`].
///
/// `accept` moves the delivery to `accepted` (dependants with `after_policy = accepted` may
/// start). `request_changes` moves it to `changes_requested`; `note` says what to change (an
/// accept may carry one too; it is only recorded). When a `request_changes` note is given and
/// the task ran as a chat whose session is still live, the note is sent to that session as a
/// user message and the task goes back to `running` (`TaskReviewed.session` names the
/// session); a terminal agent cannot be written to, so the note stays on the task for its
/// owner to read. Errors: `not_found`; `invalid_state` for a task that is not `done`, or
/// whose work was already merged or discarded, and for a note that cannot be sent (a turn is
/// in progress); `bad_request` for a note over 16 KiB. Agent-originated calls need the user's
/// approval like every mutation (ADR 0016).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReview {
    pub id: TaskId,
    pub decision: ReviewDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Result of `task.review`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReviewed {
    pub task: Task,
    /// The chat session the change request was sent to (the task runs again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
}

/// `task.start`: runs a `ready` task, now. Answered with [`TaskStarted`]. Refused with
/// `invalid_state` unless the task is `ready` (the message says what is missing), with the
/// reason of whatever fails while it is set up (git, the agent) otherwise; the task is then
/// `todo` as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub id: TaskId,
}

/// Result of `task.create` and `task.update`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub task: Task,
}

/// Result of `task.start`: the task, now `running`, and the session that works on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStarted {
    pub task: Task,
    pub session: SessionInfo,
    /// Things worth knowing, e.g. that another live agent already works in the checkout of a
    /// `main` task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
