//! `hutan mcp`: Hutan's control tools as an MCP server on stdio (ADR 0016, `docs/control.md`).
//!
//! The agent's host starts it; it speaks MCP on stdin/stdout and the Hutan protocol to the
//! daemon. When it runs inside a Hutan session it carries `HUTAN_SESSION_ID` as the client's
//! origin, so the daemon's `[control]` policy applies to what the agent asks.

pub mod guide;
mod server;
pub mod table;

use std::path::PathBuf;

use crate::error::CliError;

/// Runs until the host closes stdin. Logs only to stderr: stdout is the protocol.
pub async fn run(socket: Option<PathBuf>) -> Result<(), CliError> {
    server::serve(
        tokio::io::stdin(),
        tokio::io::stdout(),
        server::Target { socket },
    )
    .await?;
    Ok(())
}
