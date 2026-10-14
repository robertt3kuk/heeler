//! `daemon.toml`: settings only the daemon reads.
//!
//! ```toml
//! [daemon]
//! restore_sessions = "respawn"   # or "stopped"
//!
//! [workspaces]
//! home_folder = "~/Notes"        # the Home workspace's folder; default ~/Heeler
//!
//! [history]                      # session history index, off unless enabled (docs/history-index.md)
//! enabled = false
//! range_days = 0                 # 0 = everything, or 30, or 90
//! heeler = true                  # per-source switches
//! claude = true
//! codex = true
//! omp = true
//!
//! [history.roots]                # override where transcripts live (default: the agents' own dirs)
//! claude = "/path/to/claude/projects"
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
    pub history: HistorySettings,
}

/// Which source directories the history index reads; `None` = the agent's default location.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct HistoryRoots {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub omp: Option<PathBuf>,
}

/// `[history]` of `daemon.toml`. Nothing is read from any transcript directory unless
/// `enabled` is true.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub enabled: bool,
    /// Index only the last N days: 0 (everything), 30, or 90.
    pub range_days: u32,
    pub heeler: bool,
    pub claude: bool,
    pub codex: bool,
    pub omp: bool,
    pub roots: HistoryRoots,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            range_days: 0,
            heeler: true,
            claude: true,
            codex: true,
            omp: true,
            roots: HistoryRoots::default(),
        }
    }
}

/// The `range_days` values the settings accept.
pub const RANGE_DAYS: [u32; 3] = [0, 30, 90];

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    daemon: DaemonTable,
    #[serde(default)]
    workspaces: WorkspacesTable,
    #[serde(default)]
    history: HistorySettings,
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
        if !RANGE_DAYS.contains(&file.history.range_days) {
            return Err(toml::de::Error::custom(format!(
                "history.range_days must be 0, 30, or 90, not {}",
                file.history.range_days
            )));
        }
        let home_folder = file
            .workspaces
            .home_folder
            .map(|folder| expand_home_folder(&folder, user_home))
            .transpose()?;
        Ok(Self {
            restore_sessions: file.daemon.restore_sessions,
            home_folder,
            history: file.history,
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

/// Records `[history] enabled` in `daemon.toml`, creating the file or the table when missing and
/// leaving every other key and comment as it is. Written to a temporary file first, so a crash
/// never leaves a half-written config.
pub fn set_history_enabled(path: &Path, enabled: bool) -> Result<(), DaemonError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let context = |what: &str| format!("{what} {}", path.display());
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(DaemonError::io(context("reading"))(e)),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        DaemonError::io(context("parsing"))(io::Error::new(io::ErrorKind::InvalidData, e))
    })?;
    let history = doc
        .entry("history")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(table) = history.as_table_like_mut() else {
        return Err(DaemonError::io(context("updating"))(io::Error::new(
            io::ErrorKind::InvalidData,
            "`history` is not a table",
        )));
    };
    // Updating in place keeps the comments in front of the key.
    match table.get_mut("enabled") {
        Some(item) => *item = toml_edit::value(enabled),
        None => {
            table.insert("enabled", toml_edit::value(enabled));
        }
    }

    let tmp = path.with_extension("toml.tmp");
    let write = || -> io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(doc.to_string().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DaemonError::io(context("writing"))(e)
    })
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
    fn history_is_off_by_default_and_reads_its_table() {
        assert!(!DaemonSettings::parse("", None).unwrap().history.enabled);
        let settings = DaemonSettings::parse(
            "[history]\nenabled = true\nrange_days = 30\ncodex = false\n[history.roots]\nclaude = \"/tmp/c\"\n",
            None,
        )
        .unwrap()
        .history;
        assert!(settings.enabled && settings.heeler && settings.claude && settings.omp);
        assert!(!settings.codex);
        assert_eq!(settings.range_days, 30);
        assert_eq!(settings.roots.claude.as_deref(), Some(Path::new("/tmp/c")));
        assert_eq!(settings.roots.codex, None);
    }

    #[test]
    fn history_range_must_be_a_known_value() {
        for days in [0, 30, 90] {
            assert!(
                DaemonSettings::parse(&format!("[history]\nrange_days = {days}\n"), None).is_ok()
            );
        }
        let error = DaemonSettings::parse("[history]\nrange_days = 45\n", None).unwrap_err();
        assert!(error.to_string().contains("range_days"), "{error}");
        assert!(DaemonSettings::parse("[history]\nenabled = \"yes\"\n", None).is_err());
    }

    #[test]
    fn enabling_history_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        set_history_enabled(&path, true).unwrap();
        assert!(DaemonSettings::load(&path, None).unwrap().history.enabled);

        std::fs::write(
            &path,
            "# my settings\n[daemon]\nrestore_sessions = \"stopped\"\n\n[history]\n# off for now\nenabled = false\nrange_days = 90\n",
        )
        .unwrap();
        set_history_enabled(&path, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my settings") && text.contains("# off for now"),
            "{text}"
        );
        let settings = DaemonSettings::load(&path, None).unwrap();
        assert!(settings.history.enabled);
        assert_eq!(settings.history.range_days, 90);
        assert_eq!(settings.restore_sessions, RestoreMode::Stopped);

        set_history_enabled(&path, false).unwrap();
        assert!(!DaemonSettings::load(&path, None).unwrap().history.enabled);
        assert!(!dir.path().join("daemon.toml.tmp").exists());

        std::fs::write(&path, "[history\n").unwrap();
        assert!(set_history_enabled(&path, true).is_err());
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
