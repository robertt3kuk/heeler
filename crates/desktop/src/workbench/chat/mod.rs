//! The agent chat view (M4.4): a chat tab over one agent session. `model` follows the daemon
//! (journal, then live events) and owns the composer; `view` draws it; `cards` draws the
//! transcript's cards; `markdown_view` draws message text; `chip` is what "Send to agent"
//! attaches; `input` is the composer's text editing. For assistant chats (M16-B) `context`
//! describes the structured attachments of a message and the `@` mentions that offer them, and
//! `attach` is the Attach menu and form that turn a choice into one.

mod attach;
mod cards;
mod chip;
mod context;
mod input;
mod markdown_view;
mod model;
mod view;
mod widgets;

pub(crate) use cards::{counts_of, diff_row};
pub use chip::{Chip, cut_to};
pub use context::{
    ContextChip, DEFAULT_HITS, DEFAULT_LINES, DEFAULT_TURNS, Mention, MentionKind, find_mention,
    is_gateway_chat, is_harness_chat, kind_label, parse_file_arg, split_context,
};
pub use input::TextInput;
pub use markdown_view::Look;
pub use model::{ChatModel, LoadStatus};
pub use view::{ChatView, KEY_CONTEXT};
pub use widgets::{ButtonKind, button};
