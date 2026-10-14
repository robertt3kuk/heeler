//! `daemon.toml`: settings only the daemon reads.
//!
//! ```toml
//! [daemon]
//! restore_sessions = "respawn"   # or "stopped"
//! ```

use std::io;
use std::path::Path;

use serde::Deserialize;

use crate::error::DaemonError;

/// What the daemon does with sessions that were alive when it stopped (ADR 0009 step 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    /// Spawn them again with the same ids, titles, and workspaces, in their last known cwd.
    #[default]
    Respawn,
    /// List them as stopped; the user restarts what they want.
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DaemonSettings {
    pub restore_sessions: RestoreMode,
}

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    daemon: DaemonTable,
}

#[derive(Debug, Default, Deserialize)]
struct DaemonTable {
    #[serde(default)]
    restore_sessions: RestoreMode,
}

impl DaemonSettings {
    /// A missing file means defaults; an unreadable or invalid one is an error, so a typo
    /// does not silently change what happens to the user's sessions.
    pub fn load(path: &Path) -> Result<Self, DaemonError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(DaemonError::io(format!("reading {}", path.display()))(e)),
        };
        Self::parse(&text).map_err(|source| DaemonError::Config {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    fn parse(text: &str) -> Result<Self, toml::de::Error> {
        let file: File = toml::from_str(text)?;
        Ok(Self {
            restore_sessions: file.daemon.restore_sessions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_respawn() {
        assert_eq!(
            DaemonSettings::parse("").unwrap().restore_sessions,
            RestoreMode::Respawn
        );
        assert_eq!(
            DaemonSettings::parse("[daemon]\n")
                .unwrap()
                .restore_sessions,
            RestoreMode::Respawn
        );
    }

    #[test]
    fn reads_the_restore_mode() {
        let parse = |mode: &str| {
            DaemonSettings::parse(&format!("[daemon]\nrestore_sessions = \"{mode}\"\n"))
        };
        assert_eq!(
            parse("stopped").unwrap().restore_sessions,
            RestoreMode::Stopped
        );
        assert_eq!(
            parse("respawn").unwrap().restore_sessions,
            RestoreMode::Respawn
        );
        assert!(parse("resurrect").is_err());
    }

    #[test]
    fn unrelated_tables_are_ignored() {
        let settings = DaemonSettings::parse("[other]\nx = 1\n[daemon]\nfuture = true\n").unwrap();
        assert_eq!(settings, DaemonSettings::default());
    }

    #[test]
    fn a_missing_file_means_defaults_and_a_bad_one_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        assert_eq!(
            DaemonSettings::load(&path).unwrap(),
            DaemonSettings::default()
        );
        std::fs::write(&path, "[daemon]\nrestore_sessions = 3\n").unwrap();
        let error = DaemonSettings::load(&path).unwrap_err();
        assert!(error.to_string().contains("daemon.toml"), "{error}");
    }
}
