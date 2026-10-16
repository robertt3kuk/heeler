//! Live sessions: the connection bridge, attention, shared terminal attachments, and the
//! workspace ordering and filtering the sidebar uses.

pub mod agents;
pub mod attention;
mod placement;
mod store;
mod terminal;
pub mod workspaces;

pub use agents::{AgentAttentionChanged, AgentBadge, BlockedNode};
pub use attention::{
    AttentionEntry, AttentionKind, AttentionTracker, Presentation, Tone, Unread, classify,
    ended_text, present,
};
pub use placement::{PlacementBadge, placement_badge};
pub use store::{
    AccountsChanged, REPLAY_WINDOW, SessionsStore, UI_OPEN_MAX_AGE_MS, UiOpenRequested,
};
pub use terminal::TerminalSession;
