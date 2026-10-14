//! Filesystem layout of the daemon (architecture.md § Paths).
//!
//! `HEELER_HOME` moves everything under one directory; `HEELER_SOCKET` moves only the socket.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const SOCKET_FILE: &str = "heelerd.sock";
pub const LOCK_FILE: &str = "heelerd.lock";
pub const LOG_FILE: &str = "heelerd.log";
pub const DATABASE_FILE: &str = "heeler.db";
pub const INDEX_FILE: &str = "index.sqlite";
pub const CONFIG_FILE: &str = "daemon.toml";
/// Name of the Home folder created in the user's home directory (or in `HEELER_HOME`).
pub const HOME_FOLDER_NAME: &str = "Heeler";

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("cannot determine the home directory; set HEELER_HOME")]
    NoHomeDir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Directory holding the lock, the socket (unless overridden), the database, and the
    /// daemon config.
    pub base: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    /// SQLite database of workspaces and sessions (ADR 0006).
    pub database: PathBuf,
    /// The session history index (own database so that Clear is a file delete), `index.sqlite`.
    pub index_database: PathBuf,
    /// Optional daemon settings, `daemon.toml`.
    pub config_file: PathBuf,
    pub log_dir: PathBuf,
    /// Folder of the Home workspace unless `daemon.toml` names another: `~/Heeler`, or
    /// `Heeler` inside `HEELER_HOME` when that is set, so an isolated daemon never creates
    /// folders in the real home directory.
    pub default_home_folder: PathBuf,
    /// The user's home directory, for `~` in `daemon.toml`.
    pub user_home: Option<PathBuf>,
}

impl Paths {
    /// Resolves from the process environment.
    pub fn from_env() -> Result<Self, PathsError> {
        Self::resolve(
            std::env::var_os("HEELER_HOME"),
            std::env::var_os("HEELER_SOCKET"),
            std::env::home_dir(),
        )
    }

    /// Pure resolution; empty override values count as unset.
    pub fn resolve(
        heeler_home: Option<OsString>,
        heeler_socket: Option<OsString>,
        user_home: Option<PathBuf>,
    ) -> Result<Self, PathsError> {
        let mut paths = match heeler_home.filter(|v| !v.is_empty()) {
            Some(home) => Self {
                user_home,
                ..Self::in_home(PathBuf::from(home))
            },
            None => {
                let user_home = user_home.ok_or(PathsError::NoHomeDir)?;
                Self {
                    default_home_folder: user_home.join(HOME_FOLDER_NAME),
                    user_home: Some(user_home.clone()),
                    ..Self::new(
                        user_home.join("Library/Application Support/Heeler"),
                        user_home.join("Library/Logs/Heeler"),
                    )
                }
            }
        };
        if let Some(socket) = heeler_socket.filter(|v| !v.is_empty()) {
            paths.socket = PathBuf::from(socket);
        }
        Ok(paths)
    }

    /// Everything under one directory (the `HEELER_HOME` layout).
    pub fn in_home(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        let log_dir = base.join("logs");
        Self::new(base, log_dir)
    }

    fn new(base: PathBuf, log_dir: PathBuf) -> Self {
        Self {
            socket: base.join(SOCKET_FILE),
            lock: base.join(LOCK_FILE),
            database: base.join(DATABASE_FILE),
            index_database: base.join(INDEX_FILE),
            config_file: base.join(CONFIG_FILE),
            log_dir,
            default_home_folder: base.join(HOME_FOLDER_NAME),
            user_home: None,
            base,
        }
    }

    pub fn log_file(&self) -> PathBuf {
        self.log_dir.join(LOG_FILE)
    }

    /// Creates the base directory (`0700`), the log directory, and the socket's parent.
    pub fn prepare(&self) -> io::Result<()> {
        create_private_dir(&self.base)?;
        // An existing directory keeps its mode otherwise; the socket dir must not be shared.
        fs::set_permissions(&self.base, fs::Permissions::from_mode(0o700))?;
        create_private_dir(&self.log_dir)?;
        if let Some(parent) = self.socket.parent().filter(|p| !p.as_os_str().is_empty())
            && !parent.exists()
        {
            create_private_dir(parent)?;
        }
        Ok(())
    }
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn default_layout_is_under_application_support() {
        let p = Paths::resolve(None, None, Some("/Users/u".into())).unwrap();
        assert_eq!(
            p.socket,
            Path::new("/Users/u/Library/Application Support/Heeler/heelerd.sock")
        );
        assert_eq!(
            p.lock,
            Path::new("/Users/u/Library/Application Support/Heeler/heelerd.lock")
        );
        assert_eq!(p.log_dir, Path::new("/Users/u/Library/Logs/Heeler"));
        assert_eq!(
            p.database,
            Path::new("/Users/u/Library/Application Support/Heeler/heeler.db")
        );
    }

    #[test]
    fn heeler_home_moves_everything() {
        let p = Paths::resolve(os("/tmp/hh"), None, None).unwrap();
        assert_eq!(p.base, Path::new("/tmp/hh"));
        assert_eq!(p.socket, Path::new("/tmp/hh/heelerd.sock"));
        assert_eq!(p.lock, Path::new("/tmp/hh/heelerd.lock"));
        assert_eq!(p.log_dir, Path::new("/tmp/hh/logs"));
        assert_eq!(p.database, Path::new("/tmp/hh/heeler.db"));
        assert_eq!(p.index_database, Path::new("/tmp/hh/index.sqlite"));
        assert_eq!(p.config_file, Path::new("/tmp/hh/daemon.toml"));
        assert_eq!(p.log_file(), Path::new("/tmp/hh/logs/heelerd.log"));
    }

    #[test]
    fn the_home_folder_defaults_to_the_user_home_or_stays_inside_heeler_home() {
        let real = Paths::resolve(None, None, Some("/Users/u".into())).unwrap();
        assert_eq!(real.default_home_folder, Path::new("/Users/u/Heeler"));
        assert_eq!(real.user_home.as_deref(), Some(Path::new("/Users/u")));
        let isolated = Paths::resolve(os("/tmp/hh"), None, Some("/Users/u".into())).unwrap();
        assert_eq!(isolated.default_home_folder, Path::new("/tmp/hh/Heeler"));
        assert_eq!(isolated.user_home.as_deref(), Some(Path::new("/Users/u")));
        assert_eq!(
            Paths::in_home("/tmp/x").default_home_folder,
            Path::new("/tmp/x/Heeler")
        );
    }

    #[test]
    fn heeler_socket_overrides_only_the_socket() {
        let p = Paths::resolve(os("/tmp/hh"), os("/tmp/x.sock"), None).unwrap();
        assert_eq!(p.socket, Path::new("/tmp/x.sock"));
        assert_eq!(p.lock, Path::new("/tmp/hh/heelerd.lock"));
        assert_eq!(
            p.database,
            Path::new("/tmp/hh/heeler.db"),
            "the database follows the home"
        );
    }

    #[test]
    fn empty_overrides_are_ignored() {
        let p = Paths::resolve(os(""), os(""), Some("/Users/u".into())).unwrap();
        assert!(
            p.socket
                .starts_with("/Users/u/Library/Application Support/Heeler")
        );
    }

    #[test]
    fn missing_home_is_an_error() {
        assert!(matches!(
            Paths::resolve(None, None, None),
            Err(PathsError::NoHomeDir)
        ));
    }

    #[test]
    fn prepare_creates_private_base() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::in_home(dir.path().join("home"));
        p.prepare().unwrap();
        let mode = fs::metadata(&p.base).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(p.log_dir.is_dir());
    }
}
