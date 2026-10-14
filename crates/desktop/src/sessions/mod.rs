//! Live sessions: the connection bridge, attention, and shared terminal attachments.

pub mod attention;
mod store;
mod terminal;

pub use attention::{AttentionTracker, Presentation, Tone, Unread, ended_text, present};
pub use store::{REPLAY_WINDOW, SessionsStore};
pub use terminal::TerminalSession;
