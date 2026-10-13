//! Wire frames: `u32 length (BE, payload only) | u8 kind | payload` (ADR 0002).

use bytes::{Buf, BufMut, Bytes, BytesMut};
use heeler_core::TerminalId;
use tokio_util::codec::{Decoder, Encoder};

use crate::error::ProtocolError;
use crate::render::TerminalRender;

/// Largest accepted payload. Larger frames are a protocol violation.
pub const MAX_PAYLOAD: usize = 4 * 1024 * 1024;

const HEADER_LEN: usize = 5;
const TERMINAL_ID_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    /// UTF-8 JSON control envelope.
    Control = 0,
    /// Daemon → client terminal render (snapshot or frame), postcard-encoded.
    TerminalRender = 1,
    /// Client → daemon terminal input bytes.
    TerminalInput = 2,
}

impl TryFrom<u8> for FrameKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Control),
            1 => Ok(Self::TerminalRender),
            2 => Ok(Self::TerminalInput),
            other => Err(ProtocolError::UnknownFrameKind(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: Bytes,
}

impl Frame {
    pub fn control(json: impl Into<Bytes>) -> Self {
        Self {
            kind: FrameKind::Control,
            payload: json.into(),
        }
    }

    pub fn terminal_render(
        terminal: TerminalId,
        render: &TerminalRender,
    ) -> Result<Self, ProtocolError> {
        let mut buf = BytesMut::with_capacity(256);
        buf.put_u64(terminal.0);
        let encoded = postcard::to_extend(render, buf)?;
        Ok(Self {
            kind: FrameKind::TerminalRender,
            payload: encoded.freeze(),
        })
    }

    pub fn terminal_input(terminal: TerminalId, input: &[u8]) -> Self {
        let mut buf = BytesMut::with_capacity(TERMINAL_ID_LEN + input.len());
        buf.put_u64(terminal.0);
        buf.put_slice(input);
        Self {
            kind: FrameKind::TerminalInput,
            payload: buf.freeze(),
        }
    }

    /// Splits a binary terminal frame into its terminal id and body (zero-copy).
    pub fn split_terminal(&self) -> Result<(TerminalId, Bytes), ProtocolError> {
        if self.payload.len() < TERMINAL_ID_LEN {
            return Err(ProtocolError::Truncated {
                needed: TERMINAL_ID_LEN,
                have: self.payload.len(),
            });
        }
        let mut body = self.payload.clone();
        let id = body.get_u64();
        Ok((TerminalId(id), body))
    }

    pub fn decode_terminal_render(&self) -> Result<(TerminalId, TerminalRender), ProtocolError> {
        let (id, body) = self.split_terminal()?;
        Ok((id, postcard::from_bytes(&body)?))
    }
}

/// Tokio codec for [`Frame`]s. Rejects oversized frames before buffering them.
#[derive(Debug, Default, Clone, Copy)]
pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = ProtocolError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, ProtocolError> {
        if src.len() < HEADER_LEN {
            return Ok(None);
        }
        let len = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;
        if len > MAX_PAYLOAD {
            return Err(ProtocolError::FrameTooLarge { len });
        }
        let kind = FrameKind::try_from(src[4])?;
        if src.len() < HEADER_LEN + len {
            src.reserve(HEADER_LEN + len - src.len());
            return Ok(None);
        }
        src.advance(HEADER_LEN);
        let payload = src.split_to(len).freeze();
        Ok(Some(Frame { kind, payload }))
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = ProtocolError;

    fn encode(&mut self, frame: Frame, dst: &mut BytesMut) -> Result<(), ProtocolError> {
        let len = frame.payload.len();
        if len > MAX_PAYLOAD {
            return Err(ProtocolError::FrameTooLarge { len });
        }
        dst.reserve(HEADER_LEN + len);
        // MAX_PAYLOAD < u32::MAX, checked above.
        dst.put_u32(len as u32);
        dst.put_u8(frame.kind as u8);
        dst.put_slice(&frame.payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(frame: Frame) -> BytesMut {
        let mut buf = BytesMut::new();
        FrameCodec.encode(frame, &mut buf).unwrap();
        buf
    }

    #[test]
    fn decodes_frames_split_across_reads() {
        let wire = encode(Frame::control(&b"{\"id\":1}"[..]));
        let mut codec = FrameCodec;
        let mut buf = BytesMut::new();
        for (i, byte) in wire.iter().enumerate() {
            buf.put_u8(*byte);
            let out = codec.decode(&mut buf).unwrap();
            if i + 1 < wire.len() {
                assert!(out.is_none(), "frame emitted early at byte {i}");
            } else {
                assert_eq!(out.unwrap().payload, &b"{\"id\":1}"[..]);
            }
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn decodes_back_to_back_frames() {
        let mut buf = encode(Frame::control(&b"a"[..]));
        buf.extend_from_slice(&encode(Frame::terminal_input(TerminalId(7), b"ls\r")));
        let first = FrameCodec.decode(&mut buf).unwrap().unwrap();
        let second = FrameCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(first.kind, FrameKind::Control);
        let (id, body) = second.split_terminal().unwrap();
        assert_eq!((id, &body[..]), (TerminalId(7), &b"ls\r"[..]));
        assert!(FrameCodec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn rejects_oversized_header_without_buffering_payload() {
        let mut buf = BytesMut::new();
        buf.put_u32((MAX_PAYLOAD + 1) as u32);
        buf.put_u8(0);
        assert!(matches!(
            FrameCodec.decode(&mut buf),
            Err(ProtocolError::FrameTooLarge { .. })
        ));
        let too_big = Frame::control(vec![0u8; MAX_PAYLOAD + 1]);
        assert!(FrameCodec.encode(too_big, &mut BytesMut::new()).is_err());
    }

    #[test]
    fn rejects_unknown_kind_and_truncated_terminal_frames() {
        let mut buf = BytesMut::new();
        buf.put_u32(0);
        buf.put_u8(9);
        assert!(matches!(
            FrameCodec.decode(&mut buf),
            Err(ProtocolError::UnknownFrameKind(9))
        ));
        let short = Frame {
            kind: FrameKind::TerminalInput,
            payload: Bytes::from_static(b"abc"),
        };
        assert!(matches!(
            short.split_terminal(),
            Err(ProtocolError::Truncated { .. })
        ));
    }
}
