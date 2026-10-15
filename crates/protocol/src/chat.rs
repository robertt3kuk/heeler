//! Agent chat sessions and approvals (protocol 0.9, capability `chat.codex`; direct chats since
//! 0.11, capability `chat.direct`): DTOs of
//! `chat.*`, `approval.*` and their events. The event itself is the core
//! [`AgentEvent`](staffy_core::AgentEvent).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use staffy_core::agent::{PermissionKind, PermissionOption};
use staffy_core::{
    AgentEvent, ApprovalId, ApprovalPolicy, ModelInfo, SandboxMode, SessionId, TurnId, WorkspaceId,
};

use crate::assistant::Attachment;

/// Most events one `chat.history` returns, whatever `limit` asks for.
pub const HISTORY_MAX_LIMIT: u32 = 1000;
/// `limit` when a request leaves it out.
pub const HISTORY_DEFAULT_LIMIT: u32 = 200;
/// Longest accepted `chat.send` text, in bytes.
pub const SEND_MAX_BYTES: usize = 256 * 1024;

fn default_limit() -> u32 {
    HISTORY_DEFAULT_LIMIT
}

/// `chat.create`: opens an agent conversation as a new session; answered with `SessionResult`
/// once the agent is ready (state `idle`) or with an error when it could not start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCreate {
    /// Defaults to the workspace containing `cwd`, else Home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// An existing directory, absolute: the agent works there.
    pub cwd: PathBuf,
    /// `codex`, or `direct` (protocol 0.11): the model gateway, no agent in between.
    pub agent: String,
    /// A model from `chat.models` (`provider.models` for `direct`); absent takes the agent
    /// catalog's default (the provider's `default_model` for `direct`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Absent means `on-request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    /// Absent means `workspace-write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Protocol 0.11 (capability `chat.direct`), `agent: "direct"` only: the provider to talk
    /// to (`provider.list`); required there, ignored for other agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Protocol 0.11, `agent: "direct"` only: sent to the model ahead of the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// `chat.send`: the user's message; the turn's events follow as `chat.event`.
///
/// Protocol 0.12 (capability `assistant`): `attachments` is read-only context the daemon
/// resolves, redacts, bounds and journals with the message (`docs/assistant.md`); only direct
/// and assistant chats take any (`bad_request` for a Codex chat). `forbidden` when the daemon's
/// `[assistant.context]` does not allow one of them, `not_found` when it names something that
/// does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSend {
    pub session_id: SessionId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// Result of `chat.send`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSent {
    pub turn_id: TurnId,
}

/// `chat.history`: the journal of a session. Events come oldest first; to page back pass the
/// first `seq` of the previous page as `before_seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryRequest {
    pub session_id: SessionId,
    /// Only events with a smaller `seq`; absent starts at the newest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
    /// At most [`HISTORY_MAX_LIMIT`]; default [`HISTORY_DEFAULT_LIMIT`].
    #[serde(default = "default_limit")]
    pub limit: u32,
}

/// Result of `chat.history`. Events are marked `replay`. Streaming deltas are not journaled,
/// so `seq` has gaps; the `*_end` events carry the complete text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatHistory {
    pub events: Vec<AgentEvent>,
    /// Older events exist before the first one returned.
    pub has_more: bool,
    /// The newest `seq` the session has used (journaled or not): live events with a greater
    /// `seq` are not in `events`.
    pub last_seq: u64,
}

/// `chat.models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatModelsRequest {
    pub agent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatModels {
    pub agent: String,
    pub models: Vec<ModelInfo>,
}

/// `approval.list`: permission requests waiting for an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApprovalListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// A permission request waiting for an answer. Pending approvals live in the daemon: they
/// survive client disconnects, and expire when their turn ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalInfo {
    pub id: ApprovalId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub kind: PermissionKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What the agent offered; only these can be chosen.
    pub options: Vec<PermissionOption>,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalList {
    pub approvals: Vec<ApprovalInfo>,
}

/// `approval.respond`: `option_id` must be the `id` of one of the approval's options
/// (otherwise `option_not_offered`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRespond {
    pub approval_id: ApprovalId,
    pub option_id: String,
}

/// What a chat session is, as part of its `SessionInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatInfo {
    pub agent: String,
    pub model: String,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxMode,
    /// A turn is in progress.
    #[serde(default)]
    pub turn_active: bool,
    /// Permission requests waiting for an answer.
    #[serde(default)]
    pub pending_approvals: u32,
    /// Protocol 0.11: the provider of a direct chat (`agent: "direct"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}
