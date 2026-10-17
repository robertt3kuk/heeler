//! Hutan domain types: identifiers, session lifecycle, the agent event model, the agent
//! observation tree, and tasks with dependencies. No I/O lives here.

pub mod agent;
pub mod ids;
pub mod observation;
pub mod question;
pub mod session;
pub mod task;

pub use agent::{
    AgentEvent, AgentEventDraft, AgentEventKind, ApprovalPolicy, DecidedBy, Delivery, ModelInfo,
    SandboxMode,
};
pub use ids::{
    ApprovalId, Epoch, ItemId, ParseIdError, QuestionId, Seq, SessionId, TaskId, TerminalId,
    TurnId, WorkspaceId,
};
pub use observation::{
    AgentNode, AgentObservation, Changes, Liveness, NodeKind, NodeOutcome, NodeState, NodeUpdate,
    ObservationTree, Patch, Usage,
};
pub use question::{QuestionOutcome, QuestionResponse, QuestionSpec};
pub use session::{InvalidTransition, SessionEvent, SessionKind, SessionState};
pub use task::{
    InvalidTaskTransition, Scheduler, Task, TaskEvent, TaskPlacement, TaskPlacementKind, TaskRun,
    TaskState,
};
