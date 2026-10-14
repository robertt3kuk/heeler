use std::io;
use std::path::PathBuf;

/// Failures of PTY ownership and process control.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("invalid terminal size {cols}x{rows}: both dimensions must be at least 1")]
    InvalidSize { cols: u16, rows: u16 },
    #[error("failed to open a PTY: {0}")]
    OpenPty(String),
    #[error("failed to spawn {program:?}: {message}")]
    Spawn { program: String, message: String },
    #[error("failed to start terminal thread {name}: {source}")]
    Thread {
        name: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("terminal is closed (the child exited or the PTY is gone)")]
    Closed,
    #[error("PTY resize failed: {0}")]
    Resize(String),
    #[error("failed to signal the child process: {0}")]
    Signal(#[source] io::Error),
    #[error("failed to install shell integration at {path}: {source}")]
    ShellIntegration {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
