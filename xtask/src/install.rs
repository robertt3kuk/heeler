//! `cargo run -p xtask -- install [--debug] [--dest DIR]`: builds `Hutan.app` (release unless
//! `--debug`), signs it, and puts it into `/Applications` (`docs/packaging.md`).
//!
//! A Hutan already running from the destination is asked to quit first, so its window does not
//! keep the old version on screen. A daemon it leaves behind (keep running) keeps serving the
//! old binary, which macOS keeps mapped, until it is restarted; installing never stops sessions.
//! The copy goes next to the old bundle first and is swapped in with a rename, so a failed copy
//! never leaves a half-written app behind.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::bundle;

const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
const BUNDLE_ID: &str = "dev.hutan.Hutan";

#[derive(Debug)]
pub struct Options {
    pub debug: bool,
    /// Folder that gets `Hutan.app`; default `/Applications`.
    pub dest: Option<PathBuf>,
}

pub fn run(options: Options) -> Result<()> {
    let built = bundle::build_bundle(!options.debug, None)?;
    let dest_dir = options
        .dest
        .unwrap_or_else(|| PathBuf::from("/Applications"));
    ensure!(dest_dir.is_dir(), "{} is not a folder", dest_dir.display());
    let installed = dest_dir.join(bundle::APP_NAME);
    quit_running(&installed)?;
    replace(&built, &installed)?;
    bundle::verify(&installed)?;
    let registered = Command::new(LSREGISTER)
        .arg("-f")
        .arg(&installed)
        .status()
        .context("running lsregister")?;
    ensure!(registered.success(), "lsregister failed");
    println!("{}", installed.display());
    Ok(())
}

/// Copies `from` over `to` through a sibling `.new` bundle and a rename.
fn replace(from: &Path, to: &Path) -> Result<()> {
    let staging = to.with_extension("app.new");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("removing {}", staging.display()))?;
    }
    let copied = Command::new("ditto")
        .arg(from)
        .arg(&staging)
        .status()
        .context("running ditto")?;
    ensure!(copied.success(), "copying {} failed", from.display());
    if to.exists() {
        std::fs::remove_dir_all(to)
            .with_context(|| format!("removing the old {}", to.display()))?;
    }
    std::fs::rename(&staging, to)
        .with_context(|| format!("moving {} into place", staging.display()))?;
    Ok(())
}

/// The pids of `hutan-desktop` processes running from `app`.
fn desktops_of(app: &Path) -> Vec<String> {
    let executable = app.join("Contents/MacOS/hutan-desktop");
    let Ok(output) = Command::new("pgrep")
        .arg("-f")
        .arg(executable.as_os_str())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Asks the Hutan running from `app` to quit and waits for it (its own quit flow decides about
/// the daemon); refuses to install over a window that does not close.
fn quit_running(app: &Path) -> Result<()> {
    if desktops_of(app).is_empty() {
        return Ok(());
    }
    eprintln!("asking the running Hutan to quit");
    let _ = Command::new("osascript")
        .args([
            "-e",
            &format!("tell application id \"{BUNDLE_ID}\" to quit"),
        ])
        .status();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if desktops_of(app).is_empty() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!(
        "Hutan is still running from {} (a quit dialog may be waiting for an answer); quit it and run the install again",
        app.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_swaps_the_bundle_and_leaves_no_staging_copy() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("src/Hutan.app");
        std::fs::create_dir_all(from.join("Contents")).unwrap();
        std::fs::write(from.join("Contents/new"), "new").unwrap();
        let to = dir.path().join("dest/Hutan.app");
        std::fs::create_dir_all(to.join("Contents")).unwrap();
        std::fs::write(to.join("Contents/old"), "old").unwrap();

        replace(&from, &to).unwrap();

        assert!(to.join("Contents/new").is_file());
        assert!(!to.join("Contents/old").exists(), "the old bundle is gone");
        assert!(!dir.path().join("dest/Hutan.app.new").exists());
    }
}
