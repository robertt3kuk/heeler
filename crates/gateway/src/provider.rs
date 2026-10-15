//! The [`Provider`] trait and how a configured provider becomes one.

use std::fmt::Debug;
use std::sync::Arc;

use futures::future::BoxFuture;
use staffy_core::ModelInfo;
use tokio_util::sync::CancellationToken;

use crate::anthropic::Anthropic;
use crate::error::GatewayError;
use crate::http::{Endpoint, Http, validate_base_url};
use crate::ollama::Ollama;
use crate::openai::OpenAi;
use crate::secret::Secret;
use crate::types::{ChatRequest, GatewayStream, ProviderKind};

/// One model provider, reachable and (if it needs one) authenticated.
pub trait Provider: Send + Sync + Debug {
    fn kind(&self) -> ProviderKind;

    /// The models the provider offers (`/models`, `/v1/models`, `/api/tags`), sorted by id.
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, GatewayError>>;

    /// Streams one chat completion. The request is sent when the stream is first polled;
    /// cancelling `cancel` ends the stream with [`GatewayError::Cancelled`] and closes the
    /// connection, whether that happens while connecting or in the middle of the reply.
    fn stream_chat(&self, request: ChatRequest, cancel: CancellationToken) -> GatewayStream;
}

/// What it takes to talk to a provider.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Without one the kind's default is used (`openai-compatible` has none: a base URL is
    /// required).
    pub base_url: Option<String>,
    pub api_key: Option<Secret>,
}

/// Builds the adapter of `config.kind` over `http`.
pub fn connect(config: &ProviderConfig, http: &Http) -> Result<Arc<dyn Provider>, GatewayError> {
    let base = match (&config.base_url, config.kind.default_base_url()) {
        (Some(url), _) => validate_base_url(url)?,
        (None, Some(default)) => default.to_owned(),
        (None, None) => {
            return Err(GatewayError::InvalidUrl(format!(
                "a `{}` provider needs a base URL",
                config.kind
            )));
        }
    };
    let endpoint = Endpoint::new(http.clone(), &base, config.api_key.clone());
    Ok(match config.kind {
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => {
            Arc::new(OpenAi::new(config.kind, endpoint))
        }
        ProviderKind::Anthropic => Arc::new(Anthropic::new(endpoint)),
        ProviderKind::Ollama => Arc::new(Ollama::new(endpoint)),
    })
}
