//! `heelerd` as a library so tests (and later tooling) can run the daemon in-process.
//!
//! - [`paths`]: `HEELER_HOME` / `HEELER_SOCKET` layout.
//! - [`config`]: `daemon.toml` (`restore_sessions`, `home_folder`).
//! - [`daemon`]: lock, socket, accept loop, shutdown ([`start`]).
//! - [`conn`]: per-client handshake, request loop, bounded outbound queue.
//! - [`bus`]: epoch + seq event ring and fan-out.
//! - [`router`]: typed request dispatch.
//! - [`registry`]: the `SessionRegistry` actor (workspaces and terminal sessions, written
//!   through to `heeler-storage` and restored from it at start).
//! - [`restore`]: loading durable state and the rules for what is stored.
//! - [`projects`]: the Home folder, git-root detection, which workspace root contains a path.
//! - [`terminals`]: live terminals, ack-paced attachments, resize, history, and input.
//! - [`history`]: the opt-in session history index (`heeler-index`) and the `history.*` requests.
//! - [`chat`]: agent chat sessions: adapters, the approval engine, the journal.
//! - [`agent`]: validation of agent reports and the `HEELER_BIN` lookup.
//! - [`tree`]: the per-session agent observation tree: event coalescing, rollup, storage.

pub mod agent;
pub mod bus;
pub mod chat;
pub mod config;
pub mod conn;
pub mod daemon;
pub mod error;
pub mod history;
pub mod paths;
pub mod projects;
pub mod registry;
pub mod restore;
pub mod router;
pub mod terminals;
pub mod tree;

pub use bus::RingLimits;
pub use config::{DaemonSettings, HistorySettings, RestoreMode};
pub use daemon::{DaemonConfig, RunningDaemon, start};
pub use error::DaemonError;
pub use paths::Paths;
