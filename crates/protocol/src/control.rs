//! JSON control envelope carried in [`FrameKind::Control`](crate::FrameKind) frames.
//!
//! The envelope is decoded loosely first so that a request with an unknown method can still be
//! answered with its `id`; typed decoding happens in [`Request::decode`](crate::Request::decode)
//! and [`Event::decode`](crate::Event::decode).

use hutan_core::{Epoch, Seq};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ProtocolError;
use crate::frame::{Frame, FrameKind};
use crate::messages::{Event, Request};

#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    Response {
        id: u64,
        outcome: Result<Value, ErrorBody>,
    },
    Event {
        epoch: Epoch,
        seq: Seq,
        event: String,
        data: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    UnsupportedVersion,
    Unauthorized,
    NotFound,
    Conflict,
    InvalidState,
    OptionNotOffered,
    SlowConsumer,
    /// Protocol 0.12: the request is understood but a setting or a boundary refuses it (an
    /// assistant attachment the daemon's `[assistant.context]` does not allow, a file outside
    /// the workspace roots); the message names what gates it.
    Forbidden,
    ProviderError,
    Internal,
    /// Code from a newer peer; treat like `internal`.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Union of every envelope field; classification happens in [`ControlMessage::from_wire`].
#[derive(Default, Serialize, Deserialize)]
struct Wire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epoch: Option<Epoch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seq: Option<Seq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl ControlMessage {
    pub fn request(id: u64, request: &Request) -> Result<Self, ProtocolError> {
        let (method, params) = request.encode()?;
        Ok(Self::Request { id, method, params })
    }

    pub fn ok<T: Serialize>(id: u64, result: &T) -> Result<Self, ProtocolError> {
        Ok(Self::Response {
            id,
            outcome: Ok(serde_json::to_value(result)?),
        })
    }

    pub fn err(id: u64, error: ErrorBody) -> Self {
        Self::Response {
            id,
            outcome: Err(error),
        }
    }

    pub fn event(epoch: Epoch, seq: Seq, event: &Event) -> Result<Self, ProtocolError> {
        let (event, data) = event.encode()?;
        Ok(Self::Event {
            epoch,
            seq,
            event,
            data,
        })
    }

    pub fn to_json(&self) -> Result<Vec<u8>, ProtocolError> {
        let wire = match self {
            Self::Request { id, method, params } => Wire {
                id: Some(*id),
                method: Some(method.clone()),
                params: Some(params.clone()),
                ..Wire::default()
            },
            Self::Response { id, outcome } => match outcome {
                Ok(result) => Wire {
                    id: Some(*id),
                    result: Some(result.clone()),
                    ..Wire::default()
                },
                Err(error) => Wire {
                    id: Some(*id),
                    error: Some(error.clone()),
                    ..Wire::default()
                },
            },
            Self::Event {
                epoch,
                seq,
                event,
                data,
            } => Wire {
                event: Some(event.clone()),
                epoch: Some(*epoch),
                seq: Some(*seq),
                data: Some(data.clone()),
                ..Wire::default()
            },
        };
        Ok(serde_json::to_vec(&wire)?)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, ProtocolError> {
        Self::from_wire(serde_json::from_slice(bytes)?)
    }

    fn from_wire(wire: Wire) -> Result<Self, ProtocolError> {
        if let Some(method) = wire.method {
            let id = wire
                .id
                .ok_or(ProtocolError::MalformedControl("request without id"))?;
            let params = wire
                .params
                .filter(|p| !p.is_null())
                .unwrap_or_else(|| Value::Object(Default::default()));
            return Ok(Self::Request { id, method, params });
        }
        if let Some(event) = wire.event {
            let (Some(epoch), Some(seq)) = (wire.epoch, wire.seq) else {
                return Err(ProtocolError::MalformedControl("event without epoch/seq"));
            };
            return Ok(Self::Event {
                epoch,
                seq,
                event,
                data: wire.data.unwrap_or(Value::Null),
            });
        }
        let id = wire.id.ok_or(ProtocolError::MalformedControl(
            "message is neither request, response, nor event",
        ))?;
        let outcome = match wire.error {
            Some(error) => Err(error),
            None => Ok(wire.result.unwrap_or(Value::Null)),
        };
        Ok(Self::Response { id, outcome })
    }

    pub fn to_frame(&self) -> Result<Frame, ProtocolError> {
        Ok(Frame::control(self.to_json()?))
    }

    pub fn from_frame(frame: &Frame) -> Result<Self, ProtocolError> {
        if frame.kind != FrameKind::Control {
            return Err(ProtocolError::MalformedControl("not a control frame"));
        }
        Self::from_json(&frame.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_envelopes() {
        let req = ControlMessage::from_json(br#"{"id":3,"method":"daemon.status"}"#).unwrap();
        assert_eq!(
            req,
            ControlMessage::Request {
                id: 3,
                method: "daemon.status".into(),
                params: json!({})
            }
        );

        let ok = ControlMessage::from_json(br#"{"id":3,"result":{"a":1}}"#).unwrap();
        assert_eq!(
            ok,
            ControlMessage::Response {
                id: 3,
                outcome: Ok(json!({"a":1}))
            }
        );

        let err = ControlMessage::from_json(
            br#"{"id":4,"error":{"code":"brand_new_code","message":"x"}}"#,
        )
        .unwrap();
        let ControlMessage::Response {
            outcome: Err(body), ..
        } = err
        else {
            panic!("expected error response");
        };
        assert_eq!(body.code, ErrorCode::Unknown);
    }

    #[test]
    fn rejects_incomplete_envelopes() {
        for bad in [
            &br#"{"method":"x"}"#[..],
            br#"{"event":"x","seq":1}"#,
            br#"{"result":{}}"#,
        ] {
            assert!(ControlMessage::from_json(bad).is_err());
        }
    }
}
