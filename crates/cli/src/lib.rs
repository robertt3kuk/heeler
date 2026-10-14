//! Building blocks of the `heeler` CLI that the integration slice reuses for `attach`:
//! [`render::HostRenderer`] draws the daemon's render stream into the host terminal,
//! [`detach::DetachDetector`] filters the detach key out of host input, and [`host`] wraps raw
//! mode and size. [`chat_render::ChatPrinter`] prints agent chat events as plain text.

pub mod chat_render;
pub mod detach;
pub mod host;
pub mod render;

pub use detach::DetachDetector;
pub use render::HostRenderer;
