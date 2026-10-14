//! Vocabulary shared by the adapters, the store, and the API.

use std::fmt;
use std::str::FromStr;

/// Where an indexed session came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Heeler's own terminal blocks and agent reports.
    Heeler,
    Claude,
    Codex,
    Omp,
}

impl Source {
    pub const ALL: [Source; 4] = [Source::Heeler, Source::Claude, Source::Codex, Source::Omp];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Heeler => "heeler",
            Source::Claude => "claude",
            Source::Codex => "codex",
            Source::Omp => "omp",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Source {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Source::ALL
            .into_iter()
            .find(|source| source.as_str() == text)
            .ok_or_else(|| format!("unknown history source `{text}`"))
    }
}

/// Who or what wrote an indexed row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    User,
    Assistant,
    /// A tool call, a tool result, or an agent state report.
    Tool,
    /// A command typed in a Heeler terminal.
    Command,
    /// The output of a Heeler terminal command.
    Output,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::User,
        Role::Assistant,
        Role::Tool,
        Role::Command,
        Role::Output,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::Command => "command",
            Role::Output => "output",
        }
    }

    /// Multiplier on the (negative) BM25 score: larger means the role ranks higher. What people
    /// type (commands, prompts) beats what agents answer, which beats tool chatter and output.
    pub fn weight(self) -> f64 {
        match self {
            Role::Command => 1.4,
            Role::User => 1.3,
            Role::Assistant => 1.0,
            Role::Tool => 0.7,
            Role::Output => 0.6,
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Role::ALL
            .into_iter()
            .find(|role| role.as_str() == text)
            .ok_or_else(|| format!("unknown history role `{text}`"))
    }
}

/// One piece of text an adapter found, before redaction and chunking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub role: Role,
    pub ts_ms: Option<u64>,
    pub text: String,
}

impl Entry {
    pub fn new(role: Role, ts_ms: Option<u64>, text: impl Into<String>) -> Self {
        Self {
            role,
            ts_ms,
            text: text.into(),
        }
    }
}

/// A Heeler-side record pushed by the daemon (terminal block parts, agent reports).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeelerRecord {
    /// The Heeler session id.
    pub session_key: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub role: Role,
    pub ts_ms: u64,
    pub text: String,
    /// Identifies the record inside its session: pushing it again replaces the rows.
    pub ext_key: String,
}

/// A search request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub text: String,
    /// Most sessions returned.
    pub limit: usize,
    pub sources: Vec<Source>,
    pub roles: Vec<Role>,
    /// Only sessions whose cwd is this directory or inside it.
    pub project: Option<String>,
    pub since_ms: Option<u64>,
    pub until_ms: Option<u64>,
}

/// The best row of one session for a query.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub source: Source,
    pub session_key: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub role: Role,
    pub ts_ms: Option<u64>,
    /// Matched words are wrapped in [`SNIPPET_OPEN`] and [`SNIPPET_CLOSE`].
    pub snippet: String,
    /// Higher is better.
    pub score: f64,
    /// Rows of the session that match.
    pub matches: u32,
}

pub const SNIPPET_OPEN: &str = "«";
pub const SNIPPET_CLOSE: &str = "»";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    pub source: Source,
    pub files: u64,
    pub rows: u64,
    pub last_indexed_ms: Option<u64>,
    pub errors: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub sources: Vec<SourceStatus>,
    pub db_bytes: u64,
}

/// A request for the rows of one session, in conversation order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionTextQuery {
    /// The session's id in its source (the agent's session id, or the Heeler session id).
    pub key: String,
    /// Only these sources; empty = all.
    pub sources: Vec<Source>,
    /// Only these roles; empty = all.
    pub roles: Vec<Role>,
    /// Newest rows kept when the session has more; the answer is still oldest first.
    pub limit: usize,
}

/// One indexed chunk of a session. A long message is several chunks in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionText {
    pub source: Source,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub role: Role,
    pub ts_ms: Option<u64>,
    /// Redacted text, as indexed.
    pub text: String,
}
