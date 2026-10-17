use std::io;

/// Everything an agent adapter can fail with.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("could not start `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error(
        "Codex {found} is not supported (supported: {supported}); install a Codex release in that range"
    )]
    UnsupportedVersion { found: String, supported: String },
    #[error("cannot tell which Codex version this is (server said `{0}`)")]
    UnknownVersion(String),
    #[error(
        "the agent speaks ACP version {found}, Hutan speaks version {supported}; update the agent (or Hutan)"
    )]
    UnsupportedAcp { found: u64, supported: u64 },
    /// The agent needs a login Hutan cannot do for it (ADR 0015): the harness's own flow does.
    #[error("the agent is not logged in{}", suffix(.detail))]
    AuthRequired { detail: String },
    /// The agent lacks a capability the request needs (it did not advertise it).
    #[error("{0}")]
    Unsupported(String),
    /// The process is gone (or its pipes are); `detail` is what it said on stderr last.
    #[error("the agent process is not running{}", suffix(.detail))]
    Exited { detail: String },
    #[error("agent request `{method}` failed ({code}): {message}")]
    Rpc {
        method: String,
        code: i64,
        /// The agent's message, followed by its `error.data` details when it sent any.
        message: String,
    },
    #[error("agent request `{method}` timed out")]
    Timeout { method: String },
    #[error("the agent does not offer model `{model}`; available: {available}")]
    UnknownModel { model: String, available: String },
    #[error("option `{option}` was not offered for this request")]
    OptionNotOffered { option: String },
    #[error("no pending request with that id (already answered or expired)")]
    UnknownApproval,
    #[error("no pending question with that id (already answered or expired)")]
    UnknownQuestion,
    /// The answer cannot be put into the shape the agent asked for.
    #[error("{0}")]
    BadAnswer(String),
    #[error("no turn is in progress")]
    NoTurn,
    #[error("a turn is already in progress")]
    TurnInProgress,
    #[error("unexpected message from the agent: {0}")]
    Protocol(String),
}

fn suffix(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}
