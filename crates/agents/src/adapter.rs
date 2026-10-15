//! The seam between the daemon and a concrete agent (ADR 0005): spawn an agent, get a stream of
//! [`AgentEventDraft`]s, and call it through [`AgentAdapter`].

use std::path::PathBuf;

use futures::future::BoxFuture;
use hutan_core::{AgentEventDraft, ApprovalId, ApprovalPolicy, NodeUpdate, SandboxMode, TurnId};

use crate::error::AgentError;

/// What opens (or reopens) one conversation with an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSpec {
    pub cwd: PathBuf,
    /// A model from the agent's catalog; `None` takes the catalog's default.
    pub model: Option<String>,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxMode,
    /// Continue this provider conversation instead of starting a new one.
    pub resume_thread_id: Option<String>,
}

/// The conversation that is open after [`StartSpec`] was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub model: String,
    pub thread_id: String,
    pub resumed: bool,
}

/// How an agent process ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// The last lines the process wrote to stderr.
    pub stderr_tail: String,
}

/// What an adapter's event stream carries.
#[derive(Debug, Clone, PartialEq)]
pub enum AdapterMessage {
    Event(Box<AgentEventDraft>),
    /// Something about an agent of the session's observation tree: the main agent's thread, or a
    /// subagent's (`docs/integrations.md`).
    Node(Box<NodeUpdate>),
    /// The process ended; nothing follows.
    Exited(ExitInfo),
}

/// A turn the agent accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRef {
    pub turn_id: TurnId,
    pub provider_turn_id: String,
}

/// One running agent conversation. Methods never block the caller's event loop on the agent's
/// event stream, but they wait for the agent's reply: call them from a spawned task.
pub trait AgentAdapter: Send + Sync + std::fmt::Debug {
    /// The agent's short name (`codex`).
    fn agent(&self) -> &'static str;

    /// Sends the user's message; the turn's events follow on the stream.
    fn send_message(&self, text: String) -> BoxFuture<'_, Result<TurnRef, AgentError>>;

    /// Asks the agent to stop the turn in progress.
    fn interrupt(&self) -> BoxFuture<'_, Result<(), AgentError>>;

    /// Answers a pending permission request with one of the options it offered.
    fn respond(
        &self,
        approval: ApprovalId,
        option_id: String,
    ) -> BoxFuture<'_, Result<(), AgentError>>;

    /// Ends the process (politely first); the stream ends with [`AdapterMessage::Exited`].
    fn shutdown(&self) -> BoxFuture<'_, ()>;
}
