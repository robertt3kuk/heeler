//! The gateway's vocabulary: what is asked of a provider and what comes back.

use std::fmt;

use heeler_core::agent::TokenCounts;

use crate::error::GatewayError;

/// The API family a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    /// api.openai.com.
    OpenAi,
    /// api.anthropic.com (Messages API).
    Anthropic,
    /// A local (or Ollama cloud) Ollama server, over its native API.
    Ollama,
    /// Anything that speaks `/chat/completions` and `/models`: OpenRouter, Groq, LM Studio,
    /// vLLM, llama.cpp server, …
    OpenAiCompatible,
}

impl ProviderKind {
    pub const ALL: [Self; 4] = [
        Self::OpenAi,
        Self::Anthropic,
        Self::Ollama,
        Self::OpenAiCompatible,
    ];

    /// The name used in the protocol, the database and the CLI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::OpenAiCompatible => "openai-compatible",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }

    /// Where the provider lives unless told otherwise. OpenAI-style base URLs include the API
    /// version (`…/v1`), Anthropic's and Ollama's do not (their paths carry it).
    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::Anthropic => Some("https://api.anthropic.com"),
            Self::Ollama => Some("http://127.0.0.1:11434"),
            Self::OpenAiCompatible => None,
        }
    }

    /// Whether requests fail without an API key. Local servers usually need none; for them a
    /// stored key is sent as a bearer token when there is one.
    pub const fn requires_credential(self) -> bool {
        matches!(self, Self::OpenAi | Self::Anthropic)
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

/// One chat completion to stream.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: None,
            max_tokens: None,
        }
    }
}

/// What a streamed completion produces, in order: any number of `Delta`s, at most one `Usage`,
/// then `Completed`. A failure is an `Err` item instead of `Completed`; cancelling ends the
/// stream with `Err(GatewayError::Cancelled)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEvent {
    /// The next piece of the assistant's text.
    Delta(String),
    /// What the request cost; a number the provider did not report stays `None`.
    Usage(TokenCounts),
    /// The reply is complete. `finish_reason` is the provider's own word (`stop`, `length`,
    /// `end_turn`, `max_tokens`, …).
    Completed { finish_reason: Option<String> },
}

/// The stream `Provider::stream_chat` returns.
pub type GatewayStream = futures::stream::BoxStream<'static, Result<GatewayEvent, GatewayError>>;

/// Whether a provider's finish reason means the reply was cut off by the token limit.
pub fn is_length_cut(finish_reason: &str) -> bool {
    matches!(finish_reason, "length" | "max_tokens" | "max_output_tokens")
}
