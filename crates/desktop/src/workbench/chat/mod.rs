//! The agent chat view (M4.4): a chat tab over one agent session. `model` follows the daemon
//! (journal, then live events) and owns the composer; `view` draws it; `cards` draws the
//! transcript's cards; `markdown_view` draws message text; `chip` is what "Send to agent"
//! attaches; `input` is the composer's text editing.

mod cards;
mod chip;
mod input;
mod markdown_view;
mod model;
mod view;
mod widgets;

pub use chip::{Chip, cut_to};
pub use input::TextInput;
pub use markdown_view::Look;
pub use model::{ChatModel, LoadStatus};
pub use view::{ChatView, KEY_CONTEXT};
pub use widgets::{ButtonKind, button};
