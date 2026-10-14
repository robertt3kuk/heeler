//! Heeler domain types: identifiers, session lifecycle, and the agent event model. No I/O lives
//! here.

pub mod agent;
pub mod ids;
pub mod session;

pub use agent::{
    AgentEvent, AgentEventDraft, AgentEventKind, ApprovalPolicy, Delivery, ModelInfo, SandboxMode,
};
pub use ids::{
    ApprovalId, Epoch, ItemId, ParseIdError, Seq, SessionId, TerminalId, TurnId, WorkspaceId,
};
pub use session::{InvalidTransition, SessionEvent, SessionKind, SessionState};
