//! `daemon.toml`: settings only the daemon reads.
//!
//! ```toml
//! [daemon]
//! restore_sessions = "respawn"   # or "stopped"
//!
//! [workspaces]
//! home_folder = "~/Notes"        # the Home workspace's folder; default ~/Heeler
//! ```

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::Error as _;

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DaemonSettings {
    pub restore_sessions: RestoreMode,
    /// Absolute folder of the Home workspace; `None` uses the default
    /// ([`Paths::default_home_folder`](crate::Paths)).
    pub home_folder: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    daemon: DaemonTable,
    #[serde(default)]
    workspaces: WorkspacesTable,
}

#[derive(Debug, Default, Deserialize)]
struct WorkspacesTable {
    home_folder: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct DaemonTable {
    #[serde(default)]
    restore_sessions: RestoreMode,
}

impl DaemonSettings {
    /// A missing file means defaults; an unreadable or invalid one is an error, so a typo
    /// does not silently change what happens to the user's sessions. `user_home` is what a
    /// leading `~` in `home_folder` stands for.
    pub fn load(path: &Path, user_home: Option<&Path>) -> Result<Self, DaemonError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(DaemonError::io(format!("reading {}", path.display()))(e)),
        };
        Self::parse(&text, user_home).map_err(|source| DaemonError::Config {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    fn parse(text: &str, user_home: Option<&Path>) -> Result<Self, toml::de::Error> {
        let file: File = toml::from_str(text)?;
        let home_folder = file
            .workspaces
            .home_folder
            .map(|folder| expand_home_folder(&folder, user_home))
            .transpose()?;
        Ok(Self {
            restore_sessions: file.daemon.restore_sessions,
            home_folder,
        })
    }
}

/// `~` and `~/...` stand for the user's home directory; the result must be absolute.
fn expand_home_folder(folder: &str, user_home: Option<&Path>) -> Result<PathBuf, toml::de::Error> {
    let path = match folder.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = user_home.ok_or_else(|| {
                toml::de::Error::custom(
                    "workspaces.home_folder uses `~` but there is no home directory",
                )
            })?;
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(folder),
    };
    if !path.is_absolute() {
        return Err(toml::de::Error::custom(format!(
            "workspaces.home_folder must be an absolute path or start with `~/`, got `{folder}`"
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_respawn() {
        assert_eq!(
            DaemonSettings::parse("", None).unwrap().restore_sessions,
            RestoreMode::Respawn
        );
        assert_eq!(
            DaemonSettings::parse("[daemon]\n", None)
                .unwrap()
                .restore_sessions,
            RestoreMode::Respawn
        );
    }

    #[test]
    fn reads_the_restore_mode() {
        let parse = |mode: &str| {
            DaemonSettings::parse(&format!("[daemon]\nrestore_sessions = \"{mode}\"\n"), None)
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
        let settings =
            DaemonSettings::parse("[other]\nx = 1\n[daemon]\nfuture = true\n", None).unwrap();
        assert_eq!(settings, DaemonSettings::default());
    }

    #[test]
    fn a_missing_file_means_defaults_and_a_bad_one_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        assert_eq!(
            DaemonSettings::load(&path, None).unwrap(),
            DaemonSettings::default()
        );
        std::fs::write(&path, "[daemon]\nrestore_sessions = 3\n").unwrap();
        let error = DaemonSettings::load(&path, None).unwrap_err();
        assert!(error.to_string().contains("daemon.toml"), "{error}");
    }

    #[test]
    fn the_home_folder_is_absolute_and_expands_a_leading_tilde() {
        let home = Path::new("/Users/u");
        let folder = |text: &str, user_home| {
            DaemonSettings::parse(
                &format!("[workspaces]\nhome_folder = \"{text}\"\n"),
                user_home,
            )
            .map(|s| s.home_folder)
        };
        assert_eq!(
            folder("/srv/notes", None).unwrap(),
            Some("/srv/notes".into())
        );
        assert_eq!(
            folder("~/Notes", Some(home)).unwrap(),
            Some("/Users/u/Notes".into())
        );
        assert_eq!(folder("~", Some(home)).unwrap(), Some("/Users/u".into()));
        assert!(folder("~/Notes", None).is_err());
        assert!(folder("notes", Some(home)).is_err(), "relative");
        assert!(
            folder("~other/x", Some(home)).is_err(),
            "another user's home"
        );
        assert_eq!(DaemonSettings::parse("", None).unwrap().home_folder, None);
    }
}
