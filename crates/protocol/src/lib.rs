//! Hutan IPC protocol (ADR 0002, ADR 0004, `docs/protocol.md`).
//!
//! - [`chat`] also carries provenance and mid-stream partials (protocol 0.15, capability
//!   `chat.provenance`); [`history`] hits carry their origin (path, terminal and block).
//! - [`assistant`]: assistant chat DTOs and context attachments (protocol 0.12).
//! - [`notify`]: notification intents, `client.focus` (protocol 0.13).
//! - [`blocks`]: terminal block DTOs (protocol 0.5).
//! - [`chat`]: agent chat and approval DTOs (protocol 0.9).
//! - [`provider`]: model provider and direct chat DTOs (protocol 0.11).
//! - [`history`]: session history search DTOs (protocol 0.8).
//! - [`frame`]: length-prefixed frames and the tokio codec.
//! - [`control`]: JSON control envelope (requests, responses, events).
//! - [`messages`]: typed request params, results, events, and DTOs.
//! - [`worktree`]: worktrees and session placement (protocol 0.14).
//! - [`tree`]: agent observation tree DTOs (protocol 0.10).
//! - [`render`]: terminal render snapshot/frame DTOs carried in binary frames.

pub mod assistant;
pub mod blocks;
pub mod chat;
pub mod control;
pub mod error;
pub mod frame;
pub mod history;
pub mod messages;
pub mod notify;
pub mod provider;
pub mod render;
pub mod tree;
pub mod worktree;

pub use control::{ControlMessage, ErrorBody, ErrorCode};
pub use error::ProtocolError;
pub use frame::{Frame, FrameCodec, FrameKind, MAX_PAYLOAD};
pub use messages::{Event, Request};

use serde::{Deserialize, Serialize};

/// Version spoken by this build. Minor = additive, major = breaking (ADR 0002).
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion {
    major: 0,
    minor: 15,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    /// Peers interoperate iff majors match; capabilities gate minor additions.
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}
