//! Live sessions: the connection bridge, attention, shared terminal attachments, and the
//! workspace ordering and filtering the sidebar uses.

pub mod attention;
mod store;
mod terminal;
pub mod workspaces;

pub use attention::{AttentionTracker, Presentation, Tone, Unread, ended_text, present};
pub use store::{REPLAY_WINDOW, SessionsStore};
pub use terminal::TerminalSession;
