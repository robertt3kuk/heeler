//! Finding local model servers: on request (`provider.discover`), never on a timer, Heeler asks
//! the two well-known loopback ports whether a server is there and which models it has.

use std::time::Duration;

use futures::future::join_all;
use heeler_core::ModelInfo;

use crate::error::GatewayError;
use crate::http::{Http, HttpConfig};
use crate::provider::{ProviderConfig, connect};
use crate::types::ProviderKind;

/// A place to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTarget {
    pub kind: ProviderKind,
    /// What the user would call it.
    pub name: String,
    pub base_url: String,
}

/// The servers Heeler knows by port: Ollama on 11434, LM Studio on 1234.
pub fn default_targets() -> Vec<ProbeTarget> {
    vec![
        ProbeTarget {
            kind: ProviderKind::Ollama,
            name: "Ollama".to_owned(),
            base_url: "http://127.0.0.1:11434".to_owned(),
        },
        ProbeTarget {
            kind: ProviderKind::OpenAiCompatible,
            name: "LM Studio".to_owned(),
            base_url: "http://127.0.0.1:1234/v1".to_owned(),
        },
    ]
}

/// A server that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub target: ProbeTarget,
    pub models: Vec<ModelInfo>,
}

/// Asks every target for its model list, in parallel, with a short timeout and no retries.
/// Unreachable targets (and anything that does not answer like the server it should be) are
/// left out.
pub async fn discover(targets: &[ProbeTarget]) -> Vec<Discovered> {
    let http = match Http::new(HttpConfig {
        connect_timeout: Duration::from_millis(750),
        read_timeout: Duration::from_secs(2),
        request_timeout: Duration::from_secs(3),
        connect_retries: 0,
        no_proxy: true,
        ..HttpConfig::default()
    }) {
        Ok(http) => http,
        Err(error) => {
            tracing::warn!(%error, "local discovery unavailable");
            return Vec::new();
        }
    };
    let probes = targets.iter().map(|target| {
        let http = http.clone();
        async move {
            let config = ProviderConfig {
                kind: target.kind,
                base_url: Some(target.base_url.clone()),
                api_key: None,
            };
            let models = match probe(&config, &http).await {
                Ok(models) => models,
                Err(error) => {
                    tracing::debug!(url = %target.base_url, %error, "nothing to discover");
                    return None;
                }
            };
            Some(Discovered {
                target: target.clone(),
                models,
            })
        }
    });
    join_all(probes).await.into_iter().flatten().collect()
}

async fn probe(config: &ProviderConfig, http: &Http) -> Result<Vec<ModelInfo>, GatewayError> {
    connect(config, http)?.list_models().await
}
