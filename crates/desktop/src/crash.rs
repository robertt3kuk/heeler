//! Crash diagnostics: a panic hook that appends the panic message, location and backtrace to
//! `desktop.log` before the process dies.
//!
//! A panic inside an AppKit/GPUI callback cannot unwind (the callbacks are `extern "C"`), so it
//! ends in `abort()` and macOS shows its "quit unexpectedly" dialog. The default hook only
//! writes to stderr, which a launched `.app` does not keep.

use std::backtrace::Backtrace;
use std::ffi::OsString;
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};

/// File name inside the log directory.
pub const LOG_FILE: &str = "desktop.log";

/// The directory the desktop and the daemon log into: `$HUTAN_HOME/logs`, else
/// `~/Library/Logs/Hutan` (architecture.md § Paths). `None` when neither is known. The one
/// place this is decided: `desktop.log` here, `hutand.log` in `workbench::daemon`.
pub fn log_dir_from(hutan_home: Option<OsString>, user_home: Option<OsString>) -> Option<PathBuf> {
    match hutan_home.filter(|h| !h.is_empty()) {
        Some(home) => Some(PathBuf::from(home).join("logs")),
        None => user_home
            .filter(|h| !h.is_empty())
            .map(|home| PathBuf::from(home).join("Library/Logs/Hutan")),
    }
}

/// [`log_dir_from`] over the process environment.
pub fn log_dir() -> Option<PathBuf> {
    log_dir_from(std::env::var_os("HUTAN_HOME"), std::env::var_os("HOME"))
}

/// `desktop.log` inside [`log_dir`].
pub fn default_log_path() -> Option<PathBuf> {
    log_dir().map(|dir| dir.join(LOG_FILE))
}

/// One report: when, which thread, what, where, and the backtrace text.
pub fn report_text(
    unix_secs: u64,
    thread: &str,
    message: &str,
    location: &str,
    backtrace: &str,
) -> String {
    format!(
        "[{unix_secs}] hutan-desktop panicked on thread '{thread}' at {location}: {message}\n{backtrace}\n"
    )
}

/// Appends `text` to the log at `path`, creating its directory.
pub fn append_report(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

fn describe(info: &PanicHookInfo<'_>) -> (String, String) {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(non-string panic payload)".to_owned());
    let location = info
        .location()
        .map_or_else(|| "unknown location".to_owned(), ToString::to_string);
    (message, location)
}

/// Installs the hook: write the report to `path`, then run the previous hook (stderr output).
pub fn install(path: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let (message, location) = describe(info);
        let thread = std::thread::current();
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let text = report_text(
            secs,
            thread.name().unwrap_or("<unnamed>"),
            &message,
            &location,
            &Backtrace::force_capture().to_string(),
        );
        if let Err(err) = append_report(&path, &text) {
            eprintln!("hutan-desktop: cannot write {}: {err}", path.display());
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hutan_home_logs_dir_wins_then_library_logs() {
        assert_eq!(
            log_dir_from(Some("/tmp/hh".into()), Some("/Users/x".into())),
            Some(PathBuf::from("/tmp/hh/logs"))
        );
        assert_eq!(
            log_dir_from(None, Some("/Users/x".into())),
            Some(PathBuf::from("/Users/x/Library/Logs/Hutan"))
        );
        assert_eq!(
            log_dir_from(Some("".into()), Some("/Users/x".into())),
            log_dir_from(None, Some("/Users/x".into()))
        );
        assert_eq!(log_dir_from(None, None), None);
    }

    #[test]
    fn reports_append_with_message_and_location() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join(LOG_FILE);
        append_report(&path, &report_text(1, "main", "boom", "a.rs:1:2", "frames")).unwrap();
        append_report(
            &path,
            &report_text(2, "main", "again", "b.rs:3:4", "frames"),
        )
        .unwrap();
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains("thread 'main' at a.rs:1:2: boom"));
        assert!(log.contains("b.rs:3:4: again"));
        assert_eq!(log.matches("panicked").count(), 2);
    }

    #[test]
    fn the_installed_hook_writes_the_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        // The hook is process-wide: restore the harness's own afterwards.
        let original = std::panic::take_hook();
        install(path.clone());
        let result = std::panic::catch_unwind(|| panic!("hook-test-message"));
        let _ = std::panic::take_hook();
        std::panic::set_hook(original);
        assert!(result.is_err());
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains("hook-test-message"), "{log}");
        assert!(log.contains("crash.rs"), "{log}");
    }
}
