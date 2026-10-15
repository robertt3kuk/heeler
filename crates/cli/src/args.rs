//! Command-line grammar. clap reports usage errors itself with exit code 2.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use hutan_core::{ApprovalPolicy, SandboxMode, WorkspaceId};

/// Exit codes: 0 ok, 1 remote or other error, 2 usage (including an ambiguous session target),
/// 3 hutand unreachable or lost. `attach` killed by SIGTERM/SIGHUP exits 143/129.
#[derive(Debug, Parser)]
#[command(name = "hutan", version, about = "Command-line client for hutand")]
pub struct Cli {
    /// hutand socket; default: $HUTAN_SOCKET, else $HUTAN_HOME/hutand.sock, else the
    /// standard location under ~/Library/Application Support/Hutan
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
    /// Manage the hutand daemon
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
    /// Show the agents and subagents the daemon observes, as a tree per session
    Agents(AgentsArgs),
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
    /// Search past sessions: Hutan terminal blocks and Claude Code, Codex and OMP transcripts
    /// (needs `hutan index enable`)
    Search(SearchArgs),
    /// Manage the session history index (opt-in, local)
    #[command(subcommand)]
    Index(IndexCommand),
    /// Chat with a coding agent (Codex) or directly with a model, through the daemon
    #[command(subcommand)]
    Chat(ChatCommand),
    /// Model providers for direct chat: OpenAI, Anthropic, Ollama, OpenAI-compatible servers
    #[command(subcommand)]
    Provider(ProviderCommand),
    /// Ask the assistant a question, with context attached (shell output, files, agent
    /// transcripts, search results); streams the answer
    Ask(AskArgs),
    /// Assistant sessions: direct chats with a scope (global, or one workspace)
    #[command(subcommand)]
    Assistant(AssistantCommand),
    /// List the permission requests of agent chats that wait for an answer
    Approvals,
    /// Answer a permission request with one of the options it offers (see `hutan approvals`)
    Approve(ApproveArgs),
    /// Notification intents the daemon raises when something needs you: list, acknowledge, act
    /// on, or watch them
    #[command(subcommand)]
    Notify(NotifyCommand),
    /// Git worktrees: isolated checkouts for parallel agents (`hutan wt new|ls|rm`)
    #[command(subcommand)]
    Wt(WtCommand),
}

const WT_WS_HELP: &str = "Workspace: id, unique id prefix, or exact name (default: \
     $HUTAN_WORKSPACE, else the workspace containing the current directory)";

#[derive(Debug, Subcommand)]
pub enum WtCommand {
    /// Create a worktree for a branch (a new branch from --base, or the existing one); prints
    /// its path
    New(WtNewArgs),
    /// List the worktrees of a workspace's repositories: branch, path, state, live sessions
    Ls(WtLsArgs),
    /// Remove a worktree; refuses the main one, one with live sessions, and one with uncommitted
    /// changes unless --force
    Rm(WtRmArgs),
}

#[derive(Debug, Args)]
pub struct WtNewArgs {
    /// The branch to check out; created from --base when it does not exist
    pub branch: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// Where a new branch starts (default: HEAD of the repository)
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Directory of the worktree, which must not exist (default: `<repo>-wt/<branch>` next to
    /// the repository, `/` in the branch written as `-`)
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// A directory in the repository (default: the current directory when the workspace is
    /// found by it, else the workspace's first git root)
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct WtLsArgs {
    /// Only this workspace's repositories (default: $HUTAN_WORKSPACE, else the workspace
    /// containing the current directory, else all workspaces)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct WtRmArgs {
    /// The worktree's path, or its branch (see `hutan wt ls`)
    pub target: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// Remove even with uncommitted changes
    #[arg(long)]
    pub force: bool,
    /// Also delete the branch, as `git branch -d` does: an unmerged branch stays and is
    /// reported
    #[arg(long)]
    pub delete_branch: bool,
}

#[derive(Debug, Subcommand)]
pub enum NotifyCommand {
    /// List the pending intents, oldest first
    Ls,
    /// Mark an intent as seen
    Ack(NotifyIdArgs),
    /// Choose an action of an intent: an approval's option answers the approval, `open` asks the
    /// desktop to show the session
    Act(NotifyActArgs),
    /// Print the pending intents, then new ones as they are raised (until interrupted)
    Watch,
}

#[derive(Debug, Args)]
pub struct NotifyIdArgs {
    /// Intent id or unique id prefix (at least 4 characters; see `hutan notify ls`)
    pub intent: String,
}

#[derive(Debug, Args)]
pub struct NotifyActArgs {
    /// Intent id or unique id prefix (at least 4 characters)
    pub intent: String,
    /// One of the intent's actions: an approval option (`accept`, `cancel`, ...) or `open`
    pub action: String,
}

#[derive(Debug, Subcommand)]
pub enum ChatCommand {
    /// Start a chat session with the agent in a directory; prints its id
    New(ChatNewArgs),
    /// Send a message to a chat session
    Send(ChatSendArgs),
    /// Print a chat session's history from the daemon's journal
    Log(TargetArgs),
    /// Follow a chat session live: its recent history, then new events as they happen
    Attach(TargetArgs),
    /// Stop the turn in progress
    Interrupt(TargetArgs),
    /// List the models the agent offers
    Models,
    /// Switch the model of a direct chat for its next turn (the history stays)
    Model(ChatModelArgs),
}

#[derive(Debug, Args)]
pub struct ChatModelArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// The model (see `hutan provider models <provider>`)
    pub model: String,
    /// Switch to another provider as well (default: keep the chat's provider)
    #[arg(long, value_name = "ID")]
    pub provider: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    /// Configure a provider; a key is stored separately with `hutan provider key`
    Add(ProviderAddArgs),
    /// Store an API key in the Keychain (read from stdin without echo, or from an environment
    /// variable)
    Key(ProviderKeyArgs),
    /// List the configured providers
    Ls,
    /// List the models a provider offers
    Models(ProviderIdArgs),
    /// Forget a provider and its stored key
    Rm(ProviderIdArgs),
    /// Look for Ollama (127.0.0.1:11434) and LM Studio (127.0.0.1:1234) on this machine
    Discover(ProviderDiscoverArgs),
}

#[derive(Debug, Args)]
pub struct ProviderAddArgs {
    /// openai, anthropic, ollama or openai-compatible
    #[arg(value_parser = ["openai", "anthropic", "ollama", "openai-compatible"])]
    pub kind: String,
    /// Display name; the provider's id is made from it (default: the kind)
    #[arg(long)]
    pub name: Option<String>,
    /// Where the provider is (default: the kind's own; required for openai-compatible, e.g.
    /// http://127.0.0.1:1234/v1)
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,
    /// The model chats use when they name none
    #[arg(long = "model", value_name = "MODEL")]
    pub default_model: Option<String>,
}

#[derive(Debug, Args)]
pub struct ProviderKeyArgs {
    /// The provider's id (see `hutan provider ls`)
    pub provider: String,
    /// Read the key from this environment variable instead of stdin
    #[arg(long, value_name = "VAR", conflicts_with = "clear")]
    pub from_env: Option<String>,
    /// Delete the stored key instead
    #[arg(long)]
    pub clear: bool,
}

#[derive(Debug, Args)]
pub struct ProviderIdArgs {
    /// The provider's id (see `hutan provider ls`)
    pub provider: String,
}

#[derive(Debug, Args)]
pub struct ProviderDiscoverArgs {
    /// Configure the servers that were found and are not configured yet
    #[arg(long)]
    pub add: bool,
}

#[derive(Debug, Args)]
pub struct ChatNewArgs {
    /// Workspace to create the session in: id, unique id prefix, or exact name (default:
    /// $HUTAN_WORKSPACE, else the workspace containing the directory, else Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Directory the agent works in (default: the current directory)
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
    /// Model from `hutan chat models` (default: the agent catalog's default); with --direct a
    /// model of the provider (default: the provider's default model)
    #[arg(long)]
    pub model: Option<String>,
    /// When the agent asks before acting: untrusted, on-request (default), never
    #[arg(long, value_name = "POLICY", value_parser = parse_policy)]
    pub approval_policy: Option<ApprovalPolicy>,
    /// What the agent may touch: read-only, workspace-write (default), danger-full-access
    #[arg(long, value_name = "MODE", value_parser = parse_sandbox)]
    pub sandbox: Option<SandboxMode>,
    /// Session title (default: `codex · <directory>`, or `<provider> · <model>` with --direct)
    #[arg(long)]
    pub title: Option<String>,
    /// Chat directly with a model of a provider (no agent, no tools); needs --provider
    #[arg(long, requires = "provider")]
    pub direct: bool,
    /// The provider of a direct chat (see `hutan provider ls`)
    #[arg(long, value_name = "ID", requires = "direct")]
    pub provider: Option<String>,
    /// System prompt of a direct chat
    #[arg(long, value_name = "TEXT", requires = "direct")]
    pub system: Option<String>,
}

#[derive(Debug, Args)]
pub struct ChatSendArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// The message
    pub text: String,
    /// Stay until the turn ends, printing the reply as it streams (and any approval request);
    /// exits 1 when the turn fails or is cancelled
    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Args)]
pub struct AskArgs {
    /// The question
    pub question: String,
    /// Ask the assistant of this workspace: id, unique id prefix, or exact name (default:
    /// $HUTAN_WORKSPACE, else the global assistant of Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Context to attach, repeatable: block:<terminal>:<block>, recent:<terminal>:<lines>,
    /// agent:<session>[:<turns>], file:<path>[:<first>-<last>], search:<query>
    #[arg(
        long = "attach",
        value_name = "KIND:VALUE",
        value_parser = crate::attach_spec::parse_attachment
    )]
    pub attach: Vec<hutan_protocol::assistant::Attachment>,
}

#[derive(Debug, Subcommand)]
pub enum AssistantCommand {
    /// List the assistant sessions, with their scope and which is the default
    Ls(AssistantLsArgs),
    /// Print the default assistant session of a scope, creating it on first use
    Open(AssistantOpenArgs),
}

#[derive(Debug, Args)]
pub struct AssistantLsArgs {
    /// Only the assistants of this workspace (id, unique id prefix, or exact name)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct AssistantOpenArgs {
    /// The workspace: id, unique id prefix, or exact name (default: $HUTAN_WORKSPACE, else the
    /// global assistant of Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Create one more assistant instead of returning the default
    #[arg(long)]
    pub new: bool,
}

#[derive(Debug, Args)]
pub struct ApproveArgs {
    /// Approval id or unique id prefix (at least 4 characters)
    pub approval: String,
    /// One of the options the request offers (`accept`, `cancel`, ...)
    pub option: String,
}

fn parse_policy(text: &str) -> Result<ApprovalPolicy, String> {
    text.parse()
}

fn parse_sandbox(text: &str) -> Result<SandboxMode, String> {
    text.parse()
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start hutand detached and wait for its socket; does nothing if it already runs
    Start,
    /// Show whether hutand runs, and its pid, uptime, and counts
    Status,
    /// Ask hutand to shut down; sessions that were alive come back at the next start
    Stop {
        /// Stop live sessions first; otherwise shutdown is refused while any run. They are
        /// respawned by the next hutand unless --forget is given
        #[arg(long)]
        stop_sessions: bool,
        /// Record the stopped sessions as stopped so the next hutand does not respawn them
        #[arg(long, requires = "stop_sessions")]
        forget: bool,
    },
    /// Stop hutand and start a new one
    Restart {
        /// Stop live sessions first; otherwise the restart is refused while any run. The new
        /// hutand respawns them
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
    /// Print `export HUTAN_WORKSPACE=<id>`: new sessions of `hutan new` go to that workspace
    /// (use as `eval "$(hutan ws use NAME)"`)
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
    /// $HUTAN_WORKSPACE, else Home)
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
    /// Work in the worktree of this branch: created when there is none (see `hutan wt new`),
    /// then the session starts in it and goes to the workspace that contains the repository
    #[arg(long, value_name = "BRANCH", conflicts_with = "cwd")]
    pub worktree: Option<String>,
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

/// The `agent` subcommands run inside Hutan sessions on behalf of agent hooks: outside a
/// session (no `HUTAN_SESSION_ID`), without a daemon, on any daemon error, and after 200 ms
/// they do nothing and exit 0, so a hook can never break the agent that runs it.
#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per process
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
    /// Report about this node of the agent tree instead of the main agent (protocol 0.10)
    #[arg(long, value_name = "ID")]
    pub node: Option<String>,
    /// The spawning node (default: the main agent); read when the node is first reported
    #[arg(long, value_name = "ID")]
    pub parent: Option<String>,
    /// `main` (default) or `sub`; only meaningful with --node
    #[arg(long, value_enum, default_value = "main")]
    pub kind: NodeKindArg,
    /// The node's name
    #[arg(long)]
    pub name: Option<String>,
    /// The node's agent definition or role
    #[arg(long)]
    pub agent_type: Option<String>,
    /// The model the node runs
    #[arg(long)]
    pub model: Option<String>,
    /// What the node is doing now (at most 200 characters are kept)
    #[arg(long)]
    pub activity: Option<String>,
    /// Token and cost counters as JSON: `{"kind":"omp.tokens","input":1200,"output":80}`; fields
    /// you do not know stay out (they are never zero-filled)
    #[arg(long, value_name = "JSON")]
    pub usage_json: Option<String>,
    /// Finishes the node
    #[arg(long, value_enum)]
    pub outcome: Option<OutcomeArg>,
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
    /// Must increase per session, source and node; default: the current time in nanoseconds
    #[arg(long)]
    pub seq: Option<u64>,
    /// End only this node of the session's agent tree (default: the main agent)
    #[arg(long, value_name = "ID")]
    pub node: Option<String>,
    /// How the node ended (default: completed)
    #[arg(long, value_enum)]
    pub outcome: Option<OutcomeArg>,
}

/// `hutan agents`: the agents (and subagents) of the sessions, as the daemon observes them.
#[derive(Debug, Args)]
pub struct AgentsArgs {
    /// Only this session: id, unique id prefix, or title
    #[arg(value_name = "SESSION")]
    pub target: Option<String>,
    /// Keep running and print the trees again whenever one changes
    #[arg(long)]
    pub watch: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutcomeArg {
    Completed,
    Failed,
    Interrupted,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NodeKindArg {
    Main,
    Sub,
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
    /// Add Hutan's hooks to the agent's configuration (idempotent; other entries are kept and
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

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Words to find (all must match); `"quoted phrases"` match as phrases
    #[arg(required = true, value_name = "QUERY")]
    pub query: Vec<String>,
    /// Only this source; repeatable
    #[arg(long, value_enum)]
    pub source: Vec<SourceArg>,
    /// Only rows newer than this, like `90m`, `12h`, `7d`, `2w`
    #[arg(long, value_name = "AGE", value_parser = parse_age_ms)]
    pub since: Option<u64>,
    /// Most sessions listed
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: u32,
    /// Instead of listing, open hit N (1-based, as numbered by the same search) in a new
    /// terminal session in its working directory, running the agent's resume command; attaches
    /// when run from a terminal
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub resume: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SourceArg {
    Hutan,
    Claude,
    Codex,
    Omp,
}

#[derive(Debug, Subcommand)]
pub enum IndexCommand {
    /// Show whether indexing is on, what it has indexed, and the database size
    Status,
    /// Turn indexing on (recorded in daemon.toml) and index the existing transcripts
    Enable,
    /// Turn indexing off; nothing is read any more, the index stays until `clear`
    Disable,
    /// Delete the index; files are indexed again when they change or on `rebuild`
    Clear,
    /// Read every transcript again from scratch, in the background
    Rebuild,
}

/// `90m`, `12h`, `7d`, `2w` (also `s`) as milliseconds.
fn parse_age_ms(text: &str) -> Result<u64, String> {
    let split = text.len().saturating_sub(1);
    let (number, unit) = text.split_at_checked(split).ok_or("expected like 7d")?;
    let unit_ms: u64 = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => {
            return Err(format!(
                "`{text}`: use s, m, h, d or w after the number, like 7d"
            ));
        }
    };
    let number: u64 = number
        .parse()
        .map_err(|_| format!("`{text}`: expected a number before the unit, like 7d"))?;
    number
        .checked_mul(unit_ms)
        .ok_or_else(|| format!("`{text}` is too long ago"))
}

fn parse_env(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
        _ => Err(format!("expected K=V, got `{text}`")),
    }
}
