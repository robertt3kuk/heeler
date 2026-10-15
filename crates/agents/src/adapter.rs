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

/// Read-only context attached to a message (`docs/assistant.md`): a label for the user and
/// the resolved, redacted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlock {
    pub label: String,
    pub text: String,
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
    /// The agent's short name (`codex`, `omp`, `claude`).
    fn agent(&self) -> &str;

    /// Sends the user's message; the turn's events follow on the stream.
    fn send_message(&self, text: String) -> BoxFuture<'_, Result<TurnRef, AgentError>>;

    /// [`send_message`](Self::send_message) with attached context. Agents that cannot take
    /// context refuse a message that has some.
    fn send_message_with_context(
        &self,
        text: String,
        context: Vec<ContextBlock>,
    ) -> BoxFuture<'_, Result<TurnRef, AgentError>> {
        Box::pin(async move {
            if context.is_empty() {
                self.send_message(text).await
            } else {
                Err(AgentError::Unsupported(format!(
                    "{} does not take attached context",
                    self.agent()
                )))
            }
        })
    }

    /// Switches the model of the running conversation; the next turn uses it. Agents without
    /// model switching refuse.
    fn set_model(&self, model: String) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let _ = model;
            Err(AgentError::Unsupported(format!(
                "{} does not offer model switching",
                self.agent()
            )))
        })
    }

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

    /// The agent's own process id while it runs (the process whose descendants are the
    /// session's processes); `None` for agents that are not a process of their own, and after
    /// the process was reaped.
    fn pid(&self) -> Option<u32> {
        None
    }

    /// Asks the agent to stop the turn in progress of one of its threads — a subagent's; the
    /// thread is named by the node id the adapter reported for it. Fails with
    /// [`AgentError::NoTurn`] when that thread has no turn in progress and with
    /// [`AgentError::Unsupported`] when the agent cannot stop a single subagent.
    fn interrupt_thread(&self, _thread_id: &str) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async {
            Err(AgentError::Unsupported(
                "this agent cannot stop a single subagent".to_owned(),
            ))
        })
    }
}
