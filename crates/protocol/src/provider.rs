//! Model providers and direct chat (protocol 0.11, capability `chat.direct`): DTOs of
//! `provider.*` and `chat.set_model`. The secret of a provider is write-only: it travels in
//! `provider.set_credential` and in no response, event or log; everything else says only whether
//! one is stored.

use std::fmt;

use hutan_core::{ModelInfo, SessionId};
use serde::{Deserialize, Serialize};

/// The `agent` of a `chat.create` that talks to a model provider directly.
pub const AGENT_DIRECT: &str = "direct";

/// A configured model provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    /// A short unique slug (`openai`, `ollama-local`) chosen by the daemon from the name; what
    /// every other request calls the provider.
    pub id: String,
    /// `openai`, `anthropic`, `ollama` or `openai-compatible`.
    pub kind: String,
    pub name: String,
    /// Where the provider is reached; the kind's default when none was given.
    pub base_url: String,
    /// The model a chat uses when it names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// An API key is stored in the Keychain. Never the key itself.
    pub has_credential: bool,
    pub created_at_ms: u64,
}

/// Result of `provider.list`: oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderList {
    pub providers: Vec<ProviderInfo>,
}

/// Result of `provider.add`, `.update`, `.set_credential` and `.clear_credential`: the provider
/// as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderResult {
    pub provider: ProviderInfo,
}

/// `provider.add`: answered with [`ProviderResult`]; `bad_request` for an unknown `kind`, a
/// `base_url` that is not an http(s) URL without credentials in it, or no `base_url` for
/// `openai-compatible`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAdd {
    /// `openai`, `anthropic`, `ollama` or `openai-compatible`.
    pub kind: String,
    pub name: String,
    /// Absent takes the kind's default (`https://api.openai.com/v1`,
    /// `https://api.anthropic.com`, `http://127.0.0.1:11434`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

/// `provider.update`: changes the fields that are present. An empty `default_model` clears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUpdate {
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

/// `provider.remove`, `provider.clear_credential` and `provider.models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRef {
    pub provider_id: String,
}

/// `provider.set_credential`: stores (or replaces) the provider's API key in the Keychain.
/// Answered with [`ProviderResult`], which says `has_credential: true` and nothing else about
/// the key. `Debug` does not show the secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSetCredential {
    pub provider_id: String,
    pub secret: String,
}

impl fmt::Debug for ProviderSetCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderSetCredential")
            .field("provider_id", &self.provider_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Result of `provider.models`: what the provider offers right now (sorted by id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModels {
    pub provider_id: String,
    pub models: Vec<ModelInfo>,
}

/// A local model server `provider.discover` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredProvider {
    pub kind: String,
    /// `Ollama`, `LM Studio`.
    pub name: String,
    pub base_url: String,
    /// The ids of the models it serves.
    pub models: Vec<String>,
    /// The configured provider that already points here, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// Result of `provider.discover`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDiscovery {
    pub found: Vec<DiscoveredProvider>,
}

/// `chat.set_model`: the next turn of a direct chat uses `model` (of `provider_id`, absent
/// keeps the session's provider). The history stays; the journal records the switch. Answered
/// with `SessionResult`; `invalid_state` for a session that is not running, `bad_request` for
/// a session that is not a direct or harness chat or an unknown provider.
///
/// Protocol 0.17 (capability `harness.acp`): also switches the model of a running harness chat
/// (`omp`, `claude`) when its agent offers model switching (`provider_id` is ignored there; the
/// model is one of `chat.models`); `bad_request` when the agent offers none, or does not know
/// the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSetModel {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub model: String,
}
