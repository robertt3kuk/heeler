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
//! - [`chat`]: chat sessions: agent adapters, direct model chats, the approval engine, the journal.
//! - [`direct`]: direct model chat: a session whose "agent" is the model gateway.
//! - [`providers`]: model providers: SQLite config, Keychain credentials, adapters.
//! - [`agent`]: validation of agent reports and the `HEELER_BIN` lookup.
//! - [`tree`]: the per-session agent observation tree: event coalescing, rollup, storage.
//! - [`assistant`]: assistant sessions (direct chats with a scope) and their context providers.
//! - [`context`]: redaction, budget, headers and assembly of attached context.
//! - [`context_file`]: file attachments, read below the workspace roots without following symlinks.

pub mod agent;
pub mod assistant;
pub mod bus;
pub mod chat;
pub mod config;
pub mod conn;
pub mod context;
pub mod context_file;
pub mod daemon;
pub mod direct;
pub mod error;
pub mod history;
pub mod paths;
pub mod projects;
pub mod providers;
pub mod registry;
pub mod restore;
pub mod router;
pub mod terminals;
pub mod tree;

pub use bus::RingLimits;
pub use config::{AssistantSettings, DaemonSettings, HistorySettings, RestoreMode};
pub use daemon::{DaemonConfig, RunningDaemon, start};
pub use error::DaemonError;
pub use paths::Paths;
pub use providers::GatewayConfig;
