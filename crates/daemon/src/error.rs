use std::io;
use std::path::PathBuf;

use crate::paths::PathsError;

/// Longest `sun_path` that fits `sockaddr_un` on macOS (104 bytes including the NUL).
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("another heelerd is already running (lock {} is held)", lock.display())]
    AlreadyRunning { lock: PathBuf },
    #[error(
        "socket path {} is {len} bytes; the limit is {MAX_SOCKET_PATH_BYTES}",
        path.display()
    )]
    SocketPathTooLong { path: PathBuf, len: usize },
    #[error(transparent)]
    Paths(#[from] PathsError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("daemon task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

impl DaemonError {
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let context = context.into();
        move |source| Self::Io { context, source }
    }
}
