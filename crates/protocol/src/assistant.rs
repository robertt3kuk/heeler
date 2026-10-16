//! Assistant chat (protocol 0.12, capability `assistant`): DTOs of `assistant.*` and of the
//! context attachments a `chat.send` may carry (`docs/assistant.md`).
//!
//! An assistant is a direct chat (`chat.create` agent `direct`, protocol 0.11) whose session has
//! `chat.agent = "assistant"`. Its scope follows from its workspace: the Home workspace has the
//! `global` assistant, every other workspace has `workspace` ones.

use hutan_core::{AgentEvent, TerminalId, WorkspaceId};
use serde::{Deserialize, Serialize};

use crate::messages::SessionInfo;

/// The `chat.agent` of an assistant session.
pub const AGENT_ASSISTANT: &str = "assistant";

/// Most attachments one `chat.send` may carry.
pub const ATTACHMENTS_MAX: usize = 16;
/// Most lines a `shell_recent` attachment asks for.
pub const SHELL_RECENT_MAX_LINES: u32 = 500;
/// Most turns an `agent_transcript` attachment asks for.
pub const TRANSCRIPT_MAX_TURNS: u32 = 20;
/// Most hits a `search` attachment asks for.
pub const SEARCH_MAX_LIMIT: u32 = 10;
/// The key of a user `message_end` event's `payload` that holds the [`AttachmentMeta`] list.
pub const ATTACHMENTS_META_KEY: &str = "attachments_meta";

/// Where an assistant lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantScope {
    /// The Home workspace's assistant: files only under the Home folder, and only when the
    /// daemon's `[assistant.global_context]` allows files.
    Global,
    /// An assistant of one project workspace.
    Workspace,
}

impl AssistantScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }
}

/// What runs an assistant (protocol 0.20, capability `assistant.backend`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantBackend {
    /// OMP over ACP, with its tools limited to reading (`docs/assistant.md` § Backends).
    Omp,
    /// Claude Code over its ACP adapter, in its read-only `plan` mode.
    Claude,
    /// Codex (`codex app-server`) with a read-only sandbox that never asks.
    Codex,
    /// A direct model through a configured provider (`hutan provider ls`), no tools.
    #[default]
    Provider,
}

impl AssistantBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Omp => "omp",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Provider => "provider",
        }
    }

    /// The harness (`chat.agent`) that runs this backend; `None` for a direct provider.
    pub fn harness(self) -> Option<&'static str> {
        match self {
            Self::Omp => Some("omp"),
            Self::Claude => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Provider => None,
        }
    }
}

/// `assistant.configure` (protocol 0.20, capability `assistant.backend`): which backend the
/// assistants of a scope are created with from now on. Persisted by the daemon; a workspace's
/// choice wins over the global one, which wins over `[assistant]` in `daemon.toml`. Existing
/// sessions are untouched: `assistant.open` returns the oldest assistant of the scope that runs
/// on the configured backend (and, for `provider`, the configured provider), creating one when
/// there is none. Answered with [`AssistantConfig`].
///
/// Errors: `bad_request` for `workspace` without `workspace_id`, for the Home workspace, for
/// `provider` without a `provider` and for a `provider` the daemon does not know, for a
/// `provider` given with a harness backend; `not_found` for a workspace that does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantConfigure {
    pub scope: AssistantScope,
    /// Required for `workspace`, ignored for `global`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub backend: AssistantBackend,
    /// A model of the backend (`chat.models` for a harness, `provider.models` for a provider);
    /// absent takes the backend's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The provider of the `provider` backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// The backend choice `assistant.configure` stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantConfig {
    pub scope: AssistantScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub backend: AssistantBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// `assistant.open`: the default assistant session of a scope, created on first use. A stopped
/// default is started again. Answered with `SessionResult`.
///
/// The default is the oldest assistant of the scope that runs on the configured backend
/// (protocol 0.20: `assistant.configure`, else `[assistant] backend` of `daemon.toml`, else the
/// provider backend when `[assistant] provider` is set); a new one is created with that backend
/// and model. Before 0.20 the only backend was the provider.
///
/// Errors: `bad_request` for `workspace` without `workspace_id` and for the Home workspace (it
/// has the `global` scope); `not_found` for a workspace that does not exist; `invalid_state`
/// when no backend is configured (the message names the setting); `provider_error` when a
/// harness backend cannot start (its program is missing, or it is not logged in).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantOpen {
    pub scope: AssistantScope,
    /// Required for `workspace`, ignored for `global`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// Create one more assistant in the scope instead of returning the default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
}

/// `assistant.list`: the assistant sessions, oldest first, optionally of one scope and/or one
/// workspace. Answered with [`AssistantList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AssistantListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<AssistantScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// One assistant session of `assistant.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantEntry {
    pub scope: AssistantScope,
    pub workspace_id: WorkspaceId,
    /// The session `assistant.open` returns for its scope and workspace: the oldest one on the
    /// configured backend.
    pub default: bool,
    /// Protocol 0.20: what runs the session; an older daemon has provider assistants only.
    #[serde(default)]
    pub backend: AssistantBackend,
    pub session: SessionInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantList {
    pub assistants: Vec<AssistantEntry>,
}

/// The kind of an [`Attachment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    ShellBlock,
    ShellRecent,
    AgentTranscript,
    File,
    Search,
}

impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShellBlock => "shell_block",
            Self::ShellRecent => "shell_recent",
            Self::AgentTranscript => "agent_transcript",
            Self::File => "file",
            Self::Search => "search",
        }
    }
}

/// Lines `start..=end` (1-based, inclusive) of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

/// Read-only context the user attaches to a message. The daemon reads it when the message is
/// sent, redacts it, cuts it to its share of the budget and journals it with the message;
/// nothing is read that was not attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attachment {
    /// The output of one finished or running block of a terminal (`terminal.block_text`).
    ShellBlock {
        terminal_id: TerminalId,
        block_id: u64,
    },
    /// The last `lines` (1 to [`SHELL_RECENT_MAX_LINES`]) lines of a terminal's screen and
    /// scrollback.
    ShellRecent { terminal_id: TerminalId, lines: u32 },
    /// The last `last_turns` (1 to [`TRANSCRIPT_MAX_TURNS`]) turns of a conversation:
    /// a Hutan chat session id (read from its journal), or the id of a Claude Code, Codex or
    /// OMP session (read from the history index).
    AgentTranscript { session_id: String, last_turns: u32 },
    /// A file under the roots of the session's workspace (the Home folder for the global
    /// assistant), optionally only some lines. A relative path is relative to the first root.
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<LineRange>,
    },
    /// The best hits of a history search (`limit` 1 to [`SEARCH_MAX_LIMIT`]).
    Search { query: String, limit: u32 },
}

impl Attachment {
    pub fn kind(&self) -> AttachmentKind {
        match self {
            Self::ShellBlock { .. } => AttachmentKind::ShellBlock,
            Self::ShellRecent { .. } => AttachmentKind::ShellRecent,
            Self::AgentTranscript { .. } => AttachmentKind::AgentTranscript,
            Self::File { .. } => AttachmentKind::File,
            Self::Search { .. } => AttachmentKind::Search,
        }
    }
}

/// What a user message of a chat event says about its attachments (never their text, which is
/// part of the journaled message): kind, a short label, characters kept, and whether the budget
/// cut them. Carried in the `payload` of the user's `message_end` event under
/// [`ATTACHMENTS_META_KEY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub kind: AttachmentKind,
    pub label: String,
    pub chars: u64,
    pub truncated: bool,
}

/// The attachments a chat event's `payload` describes; empty for any other event.
pub fn attachments_meta(event: &AgentEvent) -> Vec<AttachmentMeta> {
    event
        .payload
        .as_ref()
        .and_then(|payload| payload.get(ATTACHMENTS_META_KEY))
        .and_then(|meta| serde_json::from_value(meta.clone()).ok())
        .unwrap_or_default()
}
