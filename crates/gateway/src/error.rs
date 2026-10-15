//! What can go wrong between Staffy and a model provider. Messages never carry credentials: they
//! name the provider's own words (`error.message` of its reply) and what Staffy tried.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// The provider needs an API key and none is stored.
    #[error("the provider needs an API key and none is stored")]
    MissingCredential,
    /// 401 / 403: the key is missing, wrong, expired or not allowed to use the model.
    #[error("the provider refused the credentials (HTTP {status}): {message}")]
    Unauthorized { status: u16, message: String },
    /// 404: the model (or the endpoint, with a wrong base URL) does not exist.
    #[error("not found: {message}; check the model name and the provider's base URL")]
    NotFound { message: String },
    /// 429.
    #[error("rate limited{}: {message}", retry_hint(*.retry_after))]
    RateLimited {
        retry_after: Option<Duration>,
        message: String,
    },
    /// 5xx (Anthropic's 529 "overloaded" included).
    #[error("the provider failed (HTTP {status}): {message}")]
    Server { status: u16, message: String },
    /// Any other status the provider rejected the request with (400, 413, 422, …).
    #[error("the provider rejected the request (HTTP {status}): {message}")]
    Rejected { status: u16, message: String },
    /// The provider could not be reached; `attempts` connection attempts were made (a connect
    /// error is retried, nothing else is).
    #[error("could not connect to the provider after {attempts} attempt(s): {message}")]
    Connect { attempts: u32, message: String },
    /// Nothing arrived within the allowed time.
    #[error("the provider did not answer in time")]
    Timeout,
    /// The provider's stream broke off or said it had failed after streaming began.
    #[error("the stream failed: {message}")]
    Stream { message: String },
    /// The provider sent something that is not what its API documents.
    #[error("unexpected reply from the provider: {0}")]
    Protocol(String),
    /// The caller cancelled the request.
    #[error("cancelled")]
    Cancelled,
    /// The base URL is not usable.
    #[error("invalid base URL: {0}")]
    InvalidUrl(String),
    /// The HTTP client could not be built.
    #[error("cannot set up the HTTP client: {0}")]
    Client(String),
}

fn retry_hint(retry_after: Option<Duration>) -> String {
    retry_after.map_or_else(String::new, |d| {
        format!(" (retry after {}s)", d.as_secs().max(1))
    })
}

impl GatewayError {
    /// Whether the same request may succeed later without a change (for a caller that wants to
    /// offer "try again").
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::Server { .. }
                | Self::Connect { .. }
                | Self::Timeout
                | Self::Stream { .. }
        )
    }

    /// The `Retry-After` of a 429, when the provider sent one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}
