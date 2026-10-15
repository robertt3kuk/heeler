//! The Ollama adapter over Ollama's **native** API: `POST {base}/api/chat` (newline-delimited
//! JSON) and `GET {base}/api/tags`.
//!
//! Why native and not the OpenAI-compatible `/v1` endpoint Ollama also offers: the native API
//! reports `prompt_eval_count` / `eval_count` on every reply and a `done_reason`, answers an
//! unknown model with a plain 404 and message (`model 'x' not found`), takes `num_predict` and
//! `temperature` as documented options, and exists on every Ollama version, while the
//! compatibility layer has changed between releases. (A user who prefers it can add Ollama as
//! an `openai-compatible` provider with base URL `http://127.0.0.1:11434/v1`.)
//!
//! Ollama needs no key. When one is stored (Ollama's hosted API) it is sent as a bearer token.

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use staffy_core::ModelInfo;
use staffy_core::agent::TokenCounts;
use tokio_util::sync::CancellationToken;

use crate::error::GatewayError;
use crate::framing::LineBuffer;
use crate::http::{Decoder, Endpoint, stream_response};
use crate::provider::Provider;
use crate::redact::Redactor;
use crate::types::{ChatRequest, GatewayEvent, GatewayStream, ProviderKind, Role};

#[derive(Debug)]
pub(crate) struct Ollama {
    endpoint: Endpoint,
}

impl Ollama {
    pub(crate) fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }
}

fn auth(
    endpoint: &Endpoint,
    request: reqwest::RequestBuilder,
) -> Result<reqwest::RequestBuilder, GatewayError> {
    endpoint.with_key(request, "authorization", "Bearer ")
}

fn body(request: &ChatRequest) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            json!({ "role": role, "content": m.content })
        })
        .collect();
    let mut options = serde_json::Map::new();
    if let Some(temperature) = request.temperature {
        options.insert("temperature".into(), json!(temperature));
    }
    if let Some(max) = request.max_tokens {
        options.insert("num_predict".into(), json!(max));
    }
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
    });
    if !options.is_empty() {
        body["options"] = Value::Object(options);
    }
    body
}

impl Provider for Ollama {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Ollama
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, GatewayError>> {
        async move {
            let value = self
                .endpoint
                .get_json("api/tags", |r| auth(&self.endpoint, r))
                .await?;
            let mut models: Vec<ModelInfo> = value
                .get("models")
                .and_then(Value::as_array)
                .ok_or_else(|| GatewayError::Protocol("`/api/tags` has no `models` array".into()))?
                .iter()
                .filter_map(|m| {
                    let id = m
                        .get("name")
                        .or_else(|| m.get("model"))?
                        .as_str()?
                        .to_owned();
                    let details = ["family", "parameter_size", "quantization_level"]
                        .iter()
                        .filter_map(|k| m.pointer(&format!("/details/{k}"))?.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    Some(ModelInfo {
                        display_name: id.clone(),
                        description: details,
                        id,
                        is_default: false,
                        hidden: false,
                        default_reasoning_effort: None,
                        reasoning_efforts: Vec::new(),
                    })
                })
                .collect();
            models.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(models)
        }
        .boxed()
    }

    fn stream_chat(&self, request: ChatRequest, cancel: CancellationToken) -> GatewayStream {
        let body = body(&request).to_string();
        let endpoint = self.endpoint.clone();
        let open = async move {
            endpoint
                .send(|| {
                    let request = endpoint
                        .post("api/chat")
                        .header("content-type", "application/json")
                        .body(body.clone());
                    auth(&endpoint, request)
                })
                .await
        }
        .boxed();
        stream_response(
            open,
            OllamaDecoder::new(self.endpoint.redactor().clone()),
            self.endpoint.redactor().clone(),
            cancel,
        )
    }
}

struct OllamaDecoder {
    lines: LineBuffer,
    redactor: Redactor,
    completed: bool,
}

impl OllamaDecoder {
    fn new(redactor: Redactor) -> Self {
        Self {
            lines: LineBuffer::default(),
            redactor,
            completed: false,
        }
    }

    fn line(&mut self, line: &str, out: &mut Vec<GatewayEvent>) -> Result<(), GatewayError> {
        if line.trim().is_empty() {
            return Ok(());
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|e| GatewayError::Protocol(format!("a stream line is not JSON: {e}")))?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Err(GatewayError::Stream {
                message: self.redactor.apply(error).into_owned(),
            });
        }
        if let Some(text) = value.pointer("/message/content").and_then(Value::as_str)
            && !text.is_empty()
        {
            out.push(GatewayEvent::Delta(text.to_owned()));
        }
        if value.get("done").and_then(Value::as_bool) == Some(true) {
            self.completed = true;
            let usage = TokenCounts {
                input_tokens: value.get("prompt_eval_count").and_then(Value::as_u64),
                output_tokens: value.get("eval_count").and_then(Value::as_u64),
                ..TokenCounts::default()
            };
            if usage != TokenCounts::default() {
                out.push(GatewayEvent::Usage(usage));
            }
            out.push(GatewayEvent::Completed {
                finish_reason: value
                    .get("done_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
        Ok(())
    }
}

impl Decoder for OllamaDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<GatewayEvent>, GatewayError> {
        let mut out = Vec::new();
        for line in self.lines.push(bytes)? {
            self.line(&line, &mut out)?;
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<GatewayEvent>, GatewayError> {
        let mut out = Vec::new();
        if let Some(line) = self.lines.finish() {
            self.line(&line, &mut out)?;
        }
        if self.completed {
            Ok(out)
        } else {
            Err(GatewayError::Stream {
                message: "the connection closed before the reply was complete".into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_become_deltas_then_usage_then_the_end() {
        let mut decoder = OllamaDecoder::new(Redactor::default());
        let mut out = decoder
            .feed(b"{\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}\n{\"message\":{\"content\":\"lo\"},\"done\":false}\n")
            .unwrap();
        out.extend(
            decoder
                .feed(b"{\"message\":{\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":26,\"eval_count\":3}\n")
                .unwrap(),
        );
        assert_eq!(
            out,
            [
                GatewayEvent::Delta("Hel".into()),
                GatewayEvent::Delta("lo".into()),
                GatewayEvent::Usage(TokenCounts {
                    input_tokens: Some(26),
                    output_tokens: Some(3),
                    ..TokenCounts::default()
                }),
                GatewayEvent::Completed {
                    finish_reason: Some("stop".into())
                },
            ]
        );
        assert!(decoder.finish().unwrap().is_empty());
    }
}
