//! Heeler terminal: PTY ownership, the VT engine and render-state extraction (ADR 0004).
//!
//! The daemon is the only VT parser. [`Terminal`] owns a child process in a PTY and keeps the
//! canonical screen state; clients get [`heeler_protocol::render`] DTOs produced per client
//! through a [`RenderTracker`]. [`Emulator`] is the same engine without a PTY (corpus replay,
//! recorder, benchmarks). Engine types never appear in this crate's public API.

#[cfg(test)]
mod behavior;
#[cfg(test)]
mod block_tests;
mod blocks;
mod emulator;
#[cfg(test)]
mod equivalence;
mod error;
mod queries;
mod render;
mod scrollback;
pub mod shell_integration;
mod sniffer;
mod sys;
mod terminal;

pub use blocks::{BlockText, BlockTextError};
pub use emulator::{DefaultColors, Emulator, LineScope, LogicalLine, LogicalLines};
pub use error::TerminalError;
pub use render::RenderTracker;
pub use sniffer::{PromptMark, TerminalMeta};
pub use terminal::{ExitStatus, Terminal, TerminalConfig, default_shell};
