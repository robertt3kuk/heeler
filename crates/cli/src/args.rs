//! Command-line grammar. clap reports usage errors itself with exit code 2.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use heeler_core::WorkspaceId;

/// Exit codes: 0 ok, 1 remote or other error, 2 usage (including an ambiguous session target),
/// 3 heelerd unreachable or lost. `attach` killed by SIGTERM/SIGHUP exits 143/129.
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
    /// Start a terminal session
    New(NewArgs),
    /// List sessions
    Ls(LsArgs),
    /// Attach this terminal to a session; detach with ctrl+\ then d
    Attach(TargetArgs),
    /// Type text into a session without attaching
    Send(SendArgs),
    /// Stop a session (SIGHUP, then SIGKILL after 3 s)
    Stop(TargetArgs),
    /// Start an ended session again (new terminal, same id)
    Restart(TargetArgs),
    /// Change a session's title
    Rename(RenameArgs),
    /// Print a session's output (scrollback and screen) as plain text, oldest first
    History(HistoryArgs),
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
    /// Stop heelerd and start a new one
    Restart {
        /// Stop live sessions first; otherwise the restart is refused while any run
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

const TARGET_HELP: &str =
    "Session id, unique id prefix (at least 4 characters), terminal id (`t:7`), or exact title";

#[derive(Debug, Args)]
pub struct TargetArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Workspace to create the session in (default: the default workspace)
    #[arg(long = "ws", value_name = "ID")]
    pub workspace: Option<WorkspaceId>,
    /// Working directory (default: the current directory)
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
    /// Session title (default: the command name)
    #[arg(long)]
    pub title: Option<String>,
    /// Extra environment variable for the session; repeatable
    #[arg(long = "env", value_name = "K=V", value_parser = parse_env)]
    pub env: Vec<(String, String)>,
    /// Attach this terminal to the new session
    #[arg(long)]
    pub attach: bool,
    /// Command to run after `--`; the login shell if omitted
    #[arg(last = true, value_name = "ARGV")]
    pub argv: Vec<String>,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    /// Only sessions of this workspace
    #[arg(long = "ws", value_name = "ID")]
    pub workspace: Option<WorkspaceId>,
}

#[derive(Debug, Args)]
pub struct SendArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// Text to type, sent as given
    pub text: String,
    /// Press enter (a carriage return) after the text
    #[arg(long)]
    pub enter: bool,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    pub title: String,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// Print at most this many lines, counted from the newest
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..))]
    pub lines: u32,
}

fn parse_env(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
        _ => Err(format!("expected K=V, got `{text}`")),
    }
}
