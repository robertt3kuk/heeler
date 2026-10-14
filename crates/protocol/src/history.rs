//! Session history search (protocol 0.8, capability `history.search`): the local FTS index over
//! Heeler terminal blocks and external agent transcripts (`docs/history-index.md`).

use serde::{Deserialize, Serialize};

/// Most hits one `history.search` returns, whatever `limit` asks for.
pub const SEARCH_MAX_LIMIT: u32 = 100;
/// `limit` when a request leaves it out.
pub const SEARCH_DEFAULT_LIMIT: u32 = 20;

/// Where an indexed session came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    /// Heeler's own terminal blocks and agent reports.
    Heeler,
    Claude,
    Codex,
    Omp,
}

impl HistorySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heeler => "heeler",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Omp => "omp",
        }
    }
}

/// Who or what wrote an indexed row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRole {
    User,
    Assistant,
    /// A tool call, a tool result, or an agent state report.
    Tool,
    /// A command typed in a Heeler terminal.
    Command,
    /// The output of a Heeler terminal command.
    Output,
}

impl HistoryRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Command => "command",
            Self::Output => "output",
        }
    }
}

fn default_limit() -> u32 {
    SEARCH_DEFAULT_LIMIT
}

/// `history.search`. `query` is words and `"quoted phrases"`; every one must match (quotes and
/// operators have no special meaning). Hits are grouped by session: the best row of each session
/// with the number of rows of that session that matched, best session first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySearch {
    pub query: String,
    /// Sessions returned; 1 to [`SEARCH_MAX_LIMIT`].
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Only these sources; empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<HistorySource>,
    /// Only rows of these roles; empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<HistoryRole>,
    /// Only sessions whose working directory is this directory or inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Only rows at or after this time (Unix milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// Only rows at or before this time (Unix milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
}

/// The best matching row of one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryHit {
    pub source: HistorySource,
    /// The session's id in its source: the agent's session id, or the Heeler session id.
    pub session_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub role: HistoryRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<u64>,
    /// Matched text with the matching words wrapped in `«` and `»`.
    pub snippet: String,
    /// BM25 score with role weights; higher is better, comparable only within one response.
    pub score: f64,
    /// Rows of this session that match.
    pub matches: u32,
    /// Command that resumes the agent session (validated like `agent.report` resume commands);
    /// absent for sources that cannot be resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryHits {
    pub hits: Vec<HistoryHit>,
}

/// `history.set_enabled`: turns indexing on or off and records it in `daemon.toml`
/// (`[history] enabled`). Answered with the new [`HistoryStatusInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySetEnabled {
    pub enabled: bool,
}

/// Counters of one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySourceStatus {
    /// `heeler`, `claude`, `codex`, `omp`.
    pub name: String,
    /// Transcript files known (0 for `heeler`).
    pub files: u64,
    /// Rows in the index.
    pub rows: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_indexed_ms: Option<u64>,
    /// Lines that could not be read, summed over the files.
    pub errors: u64,
}

/// Result of `history.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryStatusInfo {
    pub enabled: bool,
    /// The sources that are switched on (empty while disabled).
    pub sources: Vec<HistorySourceStatus>,
    /// Size of the index database with its WAL; 0 when there is none.
    pub db_bytes: u64,
}
