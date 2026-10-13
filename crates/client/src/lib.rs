//! Async client for the heelerd protocol (ADR 0002).
//!
//! [`Client::connect`] dials the daemon (optionally spawning it first), performs the handshake,
//! and starts a reader task that matches responses to calls and fans events and terminal
//! frames out to subscribers. Reconnection is not part of this crate yet; [`Client::last_seq`]
//! and [`Client::connect_resuming`] are the hooks for it.

mod client;
mod error;
mod socket;

pub use client::{Client, ConnectOptions, Resumed};
pub use error::ClientError;
pub use socket::default_socket_path;
