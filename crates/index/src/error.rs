use std::io;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("{context}: {source}")]
    Sqlite {
        context: String,
        #[source]
        source: rusqlite::Error,
    },
    /// The search text has nothing to look for (no word characters) or a filter is invalid.
    #[error("{0}")]
    BadQuery(String),
    /// The indexer thread has stopped.
    #[error("the history indexer has stopped")]
    Stopped,
}

impl IndexError {
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let context = context.into();
        move |source| Self::Io { context, source }
    }

    pub(crate) fn sqlite(context: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> Self {
        let context = context.into();
        move |source| Self::Sqlite { context, source }
    }
}

pub type Result<T, E = IndexError> = std::result::Result<T, E>;
