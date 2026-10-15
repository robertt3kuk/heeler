//! Notification intents (protocol 0.13, capability `notify.intents`): DTOs of `notify.*`,
//! `client.focus` and the `notify.intent` / `ui.open` events (`docs/protocol.md` § Notification
//! intents).
//!
//! The daemon decides *that* something deserves the user's attention and says so with an intent;
//! whoever can actually show a notification (a Notifier, the desktop, `staffy notify watch`)
//! renders it and sends the user's choice back with `notify.act`.

use serde::{Deserialize, Serialize};
use staffy_core::{SessionId, WorkspaceId};

/// Longest [`NotificationIntent::body`], in characters.
pub const BODY_MAX_CHARS: usize = 240;
/// Longest [`NotificationIntent::title`], in characters.
pub const TITLE_MAX_CHARS: usize = 120;
/// The one action of every intent that is not an approval; `notify.act` with it sends `ui.open`.
pub const ACTION_OPEN: &str = "open";

/// Why the user is being told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    /// An agent asks for permission (`approval.required`). Actions are the approval's options.
    ApprovalRequired,
    /// A node of a session's agent tree became blocked.
    AgentBlocked,
    /// The main agent of a session finished a turn that took at least `notify.min_turn_secs`.
    AgentFinished,
    /// A session's process exited with a non-zero code.
    SessionExitedWithError,
    /// A chat turn finished while no client had the session focused.
    ChatTurnFinished,
}

impl IntentKind {
    pub const ALL: [Self; 5] = [
        Self::ApprovalRequired,
        Self::AgentBlocked,
        Self::AgentFinished,
        Self::SessionExitedWithError,
        Self::ChatTurnFinished,
    ];

    /// The wire (and `daemon.toml`) name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "approval_required",
            Self::AgentBlocked => "agent_blocked",
            Self::AgentFinished => "agent_finished",
            Self::SessionExitedWithError => "session_exited_with_error",
            Self::ChatTurnFinished => "chat_turn_finished",
        }
    }
}

/// One thing the user can do about an intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentAction {
    /// For an approval the `id` of one of its options (what `approval.respond` takes),
    /// otherwise [`ACTION_OPEN`].
    pub id: String,
    pub label: String,
}

/// A request for the user's attention. `title` and `body` are redacted (secrets masked) and
/// bounded; `body` has at most [`BODY_MAX_CHARS`] characters.
///
/// An intent stays pending until it is acknowledged (`notify.ack`, or `notify.act`), its
/// approval is resolved, or it expires; `expires_at_ms` is absent for an approval, which
/// lives exactly as long as the approval does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationIntent {
    /// Opaque, unique within the daemon's lifetime.
    pub id: String,
    pub kind: IntentKind,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub title: String,
    pub body: String,
    pub actions: Vec<IntentAction>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

/// Result of `notify.list`: the pending intents, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyList {
    pub intents: Vec<NotificationIntent>,
}

/// `notify.ack`: the user has seen the intent; it is no longer pending. Answered with `Empty`;
/// `not_found` for an unknown, already acknowledged or expired intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyAck {
    pub id: String,
}

/// `notify.act`: the user chose `action_id` of the intent. An approval's action is routed to
/// `approval.respond` with it as `option_id`; [`ACTION_OPEN`] makes the daemon publish a
/// `ui.open` event for the clients. The intent is acknowledged by it. Answered with
/// [`NotifyActed`].
///
/// Errors: `not_found` (unknown, acknowledged or expired intent), `option_not_offered` (the
/// intent does not offer `action_id`), and whatever `approval.respond` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyAct {
    pub id: String,
    pub action_id: String,
}

/// Where a chosen action went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActRoute {
    /// Answered the approval.
    Approval,
    /// Published `ui.open`.
    UiOpen,
}

/// Result of `notify.act`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyActed {
    pub route: ActRoute,
    /// For [`ActRoute::UiOpen`]: how many subscribed desktop clients the event reached; `0`
    /// means nobody can show the session, and the caller may start the desktop.
    pub desktop_clients: u32,
}

/// `client.focus`: the session this connection's user is looking at, or none. Per connection:
/// replaced by the next report and dropped when the connection closes. A chat turn that
/// finishes in a session some client has focused raises no `chat_turn_finished` intent.
/// Answered with `Empty`. Unknown ids are accepted; they match nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClientFocus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}
