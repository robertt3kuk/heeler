//! Command-line grammar. clap reports usage errors itself with exit code 2.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use heeler_core::WorkspaceId;

/// Exit codes: 0 ok, 1 remote or other error, 2 usage, 3 heelerd unreachable.
#[derive(Debug, Parser)]
#[command(name = "heeler", version, about = "Command-line client for heelerd")]
pub struct Cli {
    /// heelerd socket; default: $HEELER_SOCKET, else $HEELER_HOME/heelerd.sock, else the
    /// standard location under ~/Library/Application Support/Heeler
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Machine-readable JSON output
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the heelerd daemon
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Manage workspaces
    #[command(subcommand)]
    Ws(WsCommand),
    /// Send one raw protocol request and print the result (or the error body, exit 1)
    Api(ApiArgs),
    /// Print the daemon's current state, then (with --follow) new events, as JSON lines
    Events(EventsArgs),
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start heelerd detached and wait for its socket; does nothing if it already runs
    Start,
    /// Show whether heelerd runs, and its pid, uptime, and counts
    Status,
    /// Ask heelerd to shut down
    Stop {
        /// Stop live sessions first; otherwise shutdown is refused while any run
        #[arg(long)]
        stop_sessions: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum WsCommand {
    /// List workspaces
    Ls,
    /// Create a workspace
    New {
        name: String,
        /// Repository root; repeatable
        #[arg(long = "root", value_name = "PATH")]
        roots: Vec<PathBuf>,
    },
    /// Rename a workspace
    Rename { id: WorkspaceId, name: String },
    /// Close a workspace
    Close { id: WorkspaceId },
}

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// Protocol method, for example `daemon.status`
    pub method: String,
    /// Request params as a JSON value (default `{}`)
    pub params: Option<String>,
}

#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Keep running and print events as they happen
    #[arg(long, short)]
    pub follow: bool,
}
