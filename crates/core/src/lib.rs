//! Heeler domain types: identifiers and session lifecycle. No I/O lives here.

pub mod ids;
pub mod session;

pub use ids::{Epoch, ParseIdError, Seq, SessionId, TerminalId, WorkspaceId};
pub use session::{InvalidTransition, SessionEvent, SessionKind, SessionState};
