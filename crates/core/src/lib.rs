//! Hutan domain types: identifiers, session lifecycle, the agent event model, and the agent
//! observation tree. No I/O lives here.

pub mod agent;
pub mod ids;
pub mod observation;
pub mod session;

pub use agent::{
    AgentEvent, AgentEventDraft, AgentEventKind, ApprovalPolicy, DecidedBy, Delivery, ModelInfo,
    SandboxMode,
};
pub use ids::{
    ApprovalId, Epoch, ItemId, ParseIdError, Seq, SessionId, TerminalId, TurnId, WorkspaceId,
};
pub use observation::{
    AgentNode, AgentObservation, Changes, Liveness, NodeKind, NodeOutcome, NodeState, NodeUpdate,
    ObservationTree, Patch, Usage,
};
pub use session::{InvalidTransition, SessionEvent, SessionKind, SessionState};
