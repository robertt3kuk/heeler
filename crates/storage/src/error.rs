use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
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
    #[error(
        "database {} has schema version {found}, but this build only understands up to \
         version {supported}; it was written by a newer Heeler",
        path.display()
    )]
    SchemaTooNew {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    #[error(
        "migration {version} ({name}) failed: {source}; the database was left unchanged{}",
        backup.as_ref().map(|b| format!(" (backup: {})", b.display())).unwrap_or_default()
    )]
    Migration {
        version: u32,
        name: String,
        backup: Option<PathBuf>,
        #[source]
        source: rusqlite::Error,
    },
    #[error("corrupt {what}: {detail}")]
    Corrupt { what: String, detail: String },
    #[error("cannot encode {what}: {detail}")]
    Encode { what: &'static str, detail: String },
    #[error("the storage writer has stopped")]
    Closed,
}

impl StorageError {
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let context = context.into();
        move |source| Self::Io { context, source }
    }

    pub(crate) fn sqlite(context: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> Self {
        let context = context.into();
        move |source| Self::Sqlite { context, source }
    }

    pub(crate) fn corrupt(what: impl Into<String>, detail: impl ToString) -> Self {
        Self::Corrupt {
            what: what.into(),
            detail: detail.to_string(),
        }
    }
}

pub type Result<T, E = StorageError> = std::result::Result<T, E>;
