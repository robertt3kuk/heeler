use std::io;
use std::path::PathBuf;

use heeler_protocol::{ErrorBody, ProtocolError};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// The daemon answered with an error response.
    #[error("daemon error {:?}: {}", .0.code, .0.message)]
    Remote(ErrorBody),
    /// The connection closed before (or while) the call completed.
    #[error("disconnected from the daemon")]
    Disconnected,
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("heelerd did not start accepting connections on {} in time", .socket.display())]
    AutostartTimeout { socket: PathBuf },
    #[error("cannot determine the heelerd socket path; set HEELER_SOCKET or HEELER_HOME")]
    NoSocketPath,
}
