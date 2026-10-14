//! Command-line grammar. clap reports usage errors itself with exit code 2.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
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
    /// Report a coding agent's state to the session it runs in (for agent hooks; never fails)
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Install, remove, or inspect the agent integrations (Codex, Claude Code, OMP)
    #[command(subcommand)]
    Integration(IntegrationCommand),
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start heelerd detached and wait for its socket; does nothing if it already runs
    Start,
    /// Show whether heelerd runs, and its pid, uptime, and counts
    Status,
    /// Ask heelerd to shut down; sessions that were alive come back at the next start
    Stop {
        /// Stop live sessions first; otherwise shutdown is refused while any run. They are
        /// respawned by the next heelerd unless --forget is given
        #[arg(long)]
        stop_sessions: bool,
        /// Record the stopped sessions as stopped so the next heelerd does not respawn them
        #[arg(long, requires = "stop_sessions")]
        forget: bool,
    },
    /// Stop heelerd and start a new one
    Restart {
        /// Stop live sessions first; otherwise the restart is refused while any run. The new
        /// heelerd respawns them
        #[arg(long)]
        stop_sessions: bool,
    },
}

const WS_TARGET_HELP: &str =
    "Workspace id, unique id prefix (at least 4 characters), or exact name";

#[derive(Debug, Subcommand)]
pub enum WsCommand {
    /// List workspaces (empty ones are hidden unless pinned or Home); pinned ones come first
    Ls(WsLsArgs),
    /// Open a folder as a project: finds the workspace containing it, or creates one (named
    /// after the folder, or its git repository root)
    Open {
        /// The folder; relative paths are resolved against the current directory
        path: PathBuf,
    },
    /// Create a workspace
    New {
        name: String,
        /// Repository root; repeatable
        #[arg(long = "root", value_name = "PATH")]
        roots: Vec<PathBuf>,
    },
    /// Rename a workspace
    Rename { id: WorkspaceId, name: String },
    /// Close a workspace (it stays in `ws recent`); Home and workspaces with live sessions
    /// cannot be closed
    Close { id: WorkspaceId },
    /// Change the manual order: put a workspace before another one, or last
    Move(WsMoveArgs),
    /// Keep a workspace at the top of listings
    Pin {
        #[arg(help = WS_TARGET_HELP)]
        target: String,
    },
    /// Undo `pin`
    Unpin {
        #[arg(help = WS_TARGET_HELP)]
        target: String,
    },
    /// Open projects and closed ones remembered, newest first
    Recent {
        /// At most this many (the daemon caps it at 50)
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..))]
        limit: u32,
    },
    /// Print `export HEELER_WORKSPACE=<id>`: new sessions of `heeler new` go to that workspace
    /// (use as `eval "$(heeler ws use NAME)"`)
    Use {
        #[arg(help = WS_TARGET_HELP)]
        target: String,
    },
}

#[derive(Debug, Args)]
pub struct WsLsArgs {
    /// Order of the listing (pinned workspaces always come first)
    #[arg(long, value_enum, default_value_t = WsSort::Manual)]
    pub sort: WsSort,
    /// Include empty workspaces that are neither pinned nor Home
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum WsSort {
    /// The daemon's manual order (`ws move`)
    Manual,
    /// Latest session activity first
    Recent,
    /// Workspaces with sessions waiting for you (approvals, blocked agents) first
    Attention,
    /// Alphabetical
    Name,
}

#[derive(Debug, Args)]
pub struct WsMoveArgs {
    #[arg(help = WS_TARGET_HELP)]
    pub target: String,
    /// Place it right before this workspace
    #[arg(
        long,
        value_name = "ID|NAME",
        conflicts_with = "last",
        required_unless_present = "last"
    )]
    pub before: Option<String>,
    /// Place it after all others
    #[arg(long)]
    pub last: bool,
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
    /// Workspace to create the session in: id, unique id prefix, or exact name (default:
    /// $HEELER_WORKSPACE, else Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
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
    /// Only sessions of this workspace: id, unique id prefix, or exact name
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
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

/// The `agent` subcommands run inside Heeler sessions on behalf of agent hooks: outside a
/// session (no `HEELER_SESSION_ID`), without a daemon, on any daemon error, and after 200 ms
/// they do nothing and exit 0, so a hook can never break the agent that runs it.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Report the agent's state (and optionally how to resume it)
    Report(AgentReportArgs),
    /// Say that the agent is gone: forgets its report and resume command
    Release(AgentReleaseArgs),
    /// Translate one Codex or Claude Code hook event (JSON on stdin) into a report; used by the
    /// installed hook scripts
    Hook(AgentHookArgs),
}

#[derive(Debug, Args)]
pub struct AgentReportArgs {
    /// Who reports (one integration, e.g. `claude-hooks`); sequence numbers are per source
    #[arg(long)]
    pub source: String,
    /// The agent's name (`omp`, `claude`, `codex`)
    #[arg(long)]
    pub agent: String,
    #[arg(long, value_enum)]
    pub state: AgentStateArg,
    /// Must increase per session and source; default: the current time in nanoseconds
    #[arg(long)]
    pub seq: Option<u64>,
    /// Short text shown with the state (a permission prompt, a count of subagents)
    #[arg(long)]
    pub message: Option<String>,
    /// The agent's own conversation id
    #[arg(long, value_name = "ID")]
    pub agent_session_id: Option<String>,
    /// Command that resumes the agent after a daemon restart, after `--` (a plain command name
    /// on PATH, at most 64 words, no apostrophes)
    #[arg(last = true, value_name = "ARGV")]
    pub resume_argv: Vec<String>,
}

#[derive(Debug, Args)]
pub struct AgentReleaseArgs {
    #[arg(long)]
    pub source: String,
    /// Must increase per session and source; default: the current time in nanoseconds
    #[arg(long)]
    pub seq: Option<u64>,
}

#[derive(Debug, Args)]
pub struct AgentHookArgs {
    /// Whose hook events arrive on stdin
    #[arg(value_enum)]
    pub agent: HookAgentArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AgentStateArg {
    Idle,
    Working,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookAgentArg {
    Codex,
    Claude,
}

#[derive(Debug, Subcommand)]
pub enum IntegrationCommand {
    /// Add Heeler's hooks to the agent's configuration (idempotent; other entries are kept and
    /// the file is backed up first)
    Install(IntegrationArgs),
    /// Remove only what `install` added
    Uninstall(IntegrationArgs),
    /// Show whether the integration is installed
    Status(IntegrationArgs),
}

#[derive(Debug, Args)]
pub struct IntegrationArgs {
    #[arg(value_enum)]
    pub target: IntegrationTarget,
    /// The agent's configuration directory (default: $CODEX_HOME or ~/.codex; $CLAUDE_CONFIG_DIR
    /// or ~/.claude; $PI_CODING_AGENT_DIR or ~/.omp/agent)
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IntegrationTarget {
    Codex,
    Claude,
    Omp,
}

fn parse_env(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
        _ => Err(format!("expected K=V, got `{text}`")),
    }
}
