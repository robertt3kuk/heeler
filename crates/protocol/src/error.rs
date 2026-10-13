use crate::frame::MAX_PAYLOAD;

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("frame payload of {len} bytes exceeds the {MAX_PAYLOAD}-byte limit")]
    FrameTooLarge { len: usize },
    #[error("unknown frame kind {0}")]
    UnknownFrameKind(u8),
    #[error("binary frame payload truncated: need {needed} bytes, have {have}")]
    Truncated { needed: usize, have: usize },
    #[error("malformed control message: {0}")]
    MalformedControl(&'static str),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("binary encoding: {0}")]
    Postcard(#[from] postcard::Error),
}
