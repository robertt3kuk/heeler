//! `heelerd` as a library so tests (and later tooling) can run the daemon in-process.
//!
//! - [`paths`]: `HEELER_HOME` / `HEELER_SOCKET` layout.
//! - [`config`]: `daemon.toml` (`restore_sessions`).
//! - [`daemon`]: lock, socket, accept loop, shutdown ([`start`]).
//! - [`conn`]: per-client handshake, request loop, bounded outbound queue.
//! - [`bus`]: epoch + seq event ring and fan-out.
//! - [`router`]: typed request dispatch.
//! - [`registry`]: the `SessionRegistry` actor (workspaces and terminal sessions, written
//!   through to `heeler-storage` and restored from it at start).
//! - [`restore`]: loading durable state and the rules for what is stored.
//! - [`terminals`]: live terminals, ack-paced attachments, resize, history, and input.

pub mod bus;
pub mod config;
pub mod conn;
pub mod daemon;
pub mod error;
pub mod paths;
pub mod registry;
pub mod restore;
pub mod router;
pub mod terminals;

pub use bus::RingLimits;
pub use config::{DaemonSettings, RestoreMode};
pub use daemon::{DaemonConfig, RunningDaemon, start};
pub use error::DaemonError;
pub use paths::Paths;
