//! Hutan's local session history index (roadmap M17, `docs/history-index.md`).
//!
//! - [`adapters`]: one [`Adapter`] per transcript format (Claude Code, Codex, OMP).
//! - [`scan`]: incremental reading of transcript files by byte offset.
//! - [`store`]: the SQLite schema (FTS5) and its statements.
//! - [`search`]: BM25 queries with role and column weights, grouped by session.
//! - [`redact`]: best-effort secret removal before anything is stored.
//! - [`Indexer`]: the low-priority worker thread, the FSEvents watcher, and the [`IndexHandle`]
//!   the daemon talks to.
//!
//! The crate depends on no other Hutan crate: it indexes text, the daemon decides what text
//! and when.

pub mod adapters;
mod error;
mod indexer;
pub mod redact;
pub mod scan;
pub mod search;
pub mod store;
mod sys;
pub mod text;
mod types;

pub use adapters::{Adapter, ParseState, Parsed};
pub use error::{IndexError, Result};
pub use indexer::{IndexConfig, IndexHandle, Indexer};
pub use scan::Metrics;
pub use sys::process_cpu_micros;
pub use types::{
    Entry, Hit, HutanRecord, Role, SNIPPET_CLOSE, SNIPPET_OPEN, SearchQuery, SessionText,
    SessionTextQuery, Source, SourceStatus, Status,
};

/// The command that resumes an agent session, `None` for sources without one (Hutan's own
/// sessions). The daemon validates it before handing it to a client.
pub fn resume_argv(source: Source, session_key: &str) -> Option<Vec<String>> {
    let words: [&str; 3] = match source {
        Source::Claude => ["claude", "--resume", session_key],
        Source::Codex => ["codex", "resume", session_key],
        Source::Omp => return Some(vec!["omp".to_owned(), format!("--resume={session_key}")]),
        Source::Hutan => return None,
    };
    Some(words.iter().map(|w| (*w).to_owned()).collect())
}
