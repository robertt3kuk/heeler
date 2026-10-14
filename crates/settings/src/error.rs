//! Diagnostics shared by every part of the settings engine.

use std::fmt;
use std::path::{Path, PathBuf};

/// How bad a [`SettingsError`] is. Warnings never change effective values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Something is wrong and was ignored or reverted (invalid value, parse error, refused key).
    Error,
    /// Something is suspicious but harmless (unknown key, binding conflict).
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
        })
    }
}

/// One diagnostic: which file, which key path inside it, and what is wrong.
///
/// Used both as the `Err` type of fallible store operations and as the entries of
/// `SettingsStore::errors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError {
    pub severity: Severity,
    /// File the problem was found in; `None` for in-memory layers (runtime, defaults).
    pub file: Option<PathBuf>,
    /// Dotted key path (`terminal.font_size`, `binding[2].keys`); empty for whole-file problems.
    pub key_path: String,
    pub message: String,
}

impl SettingsError {
    pub fn error(
        file: Option<&Path>,
        key_path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Severity::Error, file, key_path, message)
    }

    pub fn warning(
        file: Option<&Path>,
        key_path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Severity::Warning, file, key_path, message)
    }

    fn new(
        severity: Severity,
        file: Option<&Path>,
        key_path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            file: file.map(Path::to_path_buf),
            key_path: key_path.into(),
            message: message.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.severity)?;
        if let Some(file) = &self.file {
            write!(f, ": {}", file.display())?;
        }
        if !self.key_path.is_empty() {
            write!(f, ": {}", self.key_path)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for SettingsError {}

/// Builds a syntax error for `file`, adding the 1-based line when the parser gave a span.
pub(crate) fn syntax_error(file: &Path, text: &str, err: &toml::de::Error) -> SettingsError {
    let message = match err.span() {
        Some(span) => {
            let line = text.as_bytes()[..span.start.min(text.len())]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
                + 1;
            format!("invalid TOML (line {line}): {}", err.message())
        }
        None => format!("invalid TOML: {}", err.message()),
    };
    SettingsError::error(Some(file), "", message)
}
