//! Agent adapters (ADR 0005). One [`AgentAdapter`] trait, one concrete adapter today:
//!
//! - [`codex`]: `codex app-server` over stdio JSON-RPC, mapped to the unified
//!   [`staffy_core::AgentEvent`] model.
//! - [`adapter`]: the trait and the types around it.
//! - [`jsonrpc`]: newline-delimited JSON-RPC over a child process.
//! - [`fixture`]: record / scrub / replay tooling for Codex transcripts (the fake app server the
//!   tests run against).
//!
//! Nothing here knows about sessions, storage or the wire protocol: the daemon owns those.

pub mod adapter;
pub mod codex;
pub mod error;
pub mod fixture;
pub mod jsonrpc;

pub use adapter::{AdapterMessage, AgentAdapter, ExitInfo, StartSpec, Started, TurnRef};
pub use codex::{CodexAppServerAdapter, CodexConfig, CodexSession};
pub use error::AgentError;
