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
    /// The process is gone (or its pipes are); `detail` is what it said on stderr last.
    #[error("the agent process is not running{}", suffix(.detail))]
    Exited { detail: String },
    #[error("agent request `{method}` failed ({code}): {message}")]
    Rpc {
        method: String,
        code: i64,
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
    #[error("no turn is in progress")]
    NoTurn,
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
