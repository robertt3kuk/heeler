//! Installing Hutan's agent integrations (`docs/integrations.md`) from the desktop: the hooks in
//! Codex, Claude Code and OMP that report what an agent is doing.
//!
//! The desktop does not write those files itself: it runs the `hutan` CLI that sits beside it
//! (`Contents/MacOS/hutan` in the bundle, `target/debug/hutan` in a development build), the
//! same `hutan integration install|status|uninstall <harness>` the user would type, so there is
//! one implementation. Everything goes through [`IntegrationInstaller`]; tests use
//! [`FakeInstaller`] and never run a CLI or write into `~/.omp`, `~/.claude` or `~/.codex`.
//!
//! Every method blocks (it waits for a process): call them off the main thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// What `hutan integration status` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationStatus {
    Installed,
    /// Some hooks or the script are missing or outdated.
    Partial,
    NotInstalled,
}

impl IntegrationStatus {
    /// The words the setup shows.
    pub fn label(self) -> &'static str {
        match self {
            IntegrationStatus::Installed => "hooks installed",
            IntegrationStatus::Partial => "hooks incomplete",
            IntegrationStatus::NotInstalled => "hooks not installed",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "installed" => Some(Self::Installed),
            "partial" => Some(Self::Partial),
            "not_installed" => Some(Self::NotInstalled),
            _ => None,
        }
    }
}

/// The three operations of `hutan integration`, for one harness (`omp`, `claude`, `codex`).
pub trait IntegrationInstaller: Send + Sync + std::fmt::Debug {
    fn status(&self, harness: &str) -> Result<IntegrationStatus, String>;
    /// Idempotent: nothing changes when the hooks are installed already.
    fn install(&self, harness: &str) -> Result<(), String>;
    fn uninstall(&self, harness: &str) -> Result<(), String>;
}

/// The command a user types for `action` (`install`, `uninstall`) on `harness`.
pub fn command_line(action: &str, harness: &str) -> String {
    format!("hutan integration {action} {harness}")
}

/// The real installer: the `hutan` executable at `hutan`.
#[derive(Debug, Clone)]
pub struct CliInstaller {
    hutan: PathBuf,
}

impl CliInstaller {
    pub fn new(hutan: PathBuf) -> Self {
        Self { hutan }
    }

    fn run(&self, action: &str, harness: &str) -> Result<String, String> {
        let output = Command::new(&self.hutan)
            .args(["--json", "integration", action, harness])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("cannot run {}: {error}", self.hutan.display()))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        Err(if message.is_empty() {
            format!(
                "{} {action} {harness} failed: {}",
                self.hutan.display(),
                output.status
            )
        } else {
            message.to_owned()
        })
    }
}

impl IntegrationInstaller for CliInstaller {
    fn status(&self, harness: &str) -> Result<IntegrationStatus, String> {
        let text = self.run("status", harness)?;
        let report: serde_json::Value = serde_json::from_str(text.trim())
            .map_err(|error| format!("cannot read the status of {harness}: {error}"))?;
        report
            .get("status")
            .and_then(serde_json::Value::as_str)
            .and_then(IntegrationStatus::parse)
            .ok_or_else(|| format!("the status of {harness} is not one the desktop knows"))
    }

    fn install(&self, harness: &str) -> Result<(), String> {
        self.run("install", harness).map(drop)
    }

    fn uninstall(&self, harness: &str) -> Result<(), String> {
        self.run("uninstall", harness).map(drop)
    }
}

/// What the fake saw: `(action, harness)` in the order it happened.
pub type Calls = Vec<(&'static str, String)>;

#[derive(Debug, Default)]
struct FakeState {
    statuses: HashMap<String, IntegrationStatus>,
    failing: HashMap<String, String>,
    failing_install: HashMap<String, String>,
    calls: Calls,
}

/// A scripted installer for tests. Installing makes a harness `Installed`, uninstalling
/// `NotInstalled`, as the CLI does; a harness it was told to fail on answers every call with
/// that error. Cloning shares the state, so the test keeps a handle.
#[derive(Debug, Clone, Default)]
pub struct FakeInstaller {
    state: Arc<Mutex<FakeState>>,
}

impl FakeInstaller {
    pub fn new() -> Self {
        Self::default()
    }

    /// The status `harness` starts with (`NotInstalled` when never set).
    pub fn set_status(&self, harness: &str, status: IntegrationStatus) {
        self.with(|state| {
            state.statuses.insert(harness.to_owned(), status);
        });
    }

    /// Every call on `harness` fails with `message`.
    pub fn fail(&self, harness: &str, message: &str) {
        self.with(|state| {
            state.failing.insert(harness.to_owned(), message.to_owned());
        });
    }

    /// `install` (only) fails on `harness` with `message`.
    pub fn fail_install(&self, harness: &str, message: &str) {
        self.with(|state| {
            state
                .failing_install
                .insert(harness.to_owned(), message.to_owned());
        });
    }

    /// Every call so far, oldest first.
    pub fn calls(&self) -> Calls {
        self.with(|state| state.calls.clone())
    }

    /// The `install` calls so far, by harness.
    pub fn installs(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|(action, _)| *action == "install")
            .map(|(_, harness)| harness)
            .collect()
    }

    fn with<T>(&self, f: impl FnOnce(&mut FakeState) -> T) -> T {
        // A test that panicked while holding the lock is already failing.
        match self.state.lock() {
            Ok(mut state) => f(&mut state),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    fn call(&self, action: &'static str, harness: &str) -> Result<(), String> {
        self.with(|state| {
            state.calls.push((action, harness.to_owned()));
            let only_install = (action == "install")
                .then(|| state.failing_install.get(harness))
                .flatten();
            match state.failing.get(harness).or(only_install) {
                Some(message) => Err(message.clone()),
                None => Ok(()),
            }
        })
    }
}

impl IntegrationInstaller for FakeInstaller {
    fn status(&self, harness: &str) -> Result<IntegrationStatus, String> {
        self.call("status", harness)?;
        Ok(self.with(|state| {
            state
                .statuses
                .get(harness)
                .copied()
                .unwrap_or(IntegrationStatus::NotInstalled)
        }))
    }

    fn install(&self, harness: &str) -> Result<(), String> {
        self.call("install", harness)?;
        self.set_status(harness, IntegrationStatus::Installed);
        Ok(())
    }

    fn uninstall(&self, harness: &str) -> Result<(), String> {
        self.call("uninstall", harness)?;
        self.set_status(harness, IntegrationStatus::NotInstalled);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_parse_as_the_cli_prints_them() {
        assert_eq!(
            IntegrationStatus::parse("installed"),
            Some(IntegrationStatus::Installed)
        );
        assert_eq!(
            IntegrationStatus::parse("not_installed"),
            Some(IntegrationStatus::NotInstalled)
        );
        assert_eq!(
            IntegrationStatus::parse("partial"),
            Some(IntegrationStatus::Partial)
        );
        assert_eq!(IntegrationStatus::parse("nope"), None);
    }

    #[test]
    fn the_cli_installer_reads_the_json_status_and_reports_a_failed_run() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hutan");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             [ \"$1 $2\" = \"--json integration\" ] || exit 9\n\
             case \"$3 $4\" in\n\
               \"status omp\") echo '{\"target\":\"omp\",\"status\":\"partial\"}' ;;\n\
               \"install omp\") echo '{}' ;;\n\
               *) echo \"no such thing: $3 $4\" >&2; exit 1 ;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let installer = CliInstaller::new(script);
        assert_eq!(installer.status("omp"), Ok(IntegrationStatus::Partial));
        assert_eq!(installer.install("omp"), Ok(()));
        assert_eq!(
            installer.uninstall("omp"),
            Err("no such thing: uninstall omp".to_owned())
        );
        let missing = CliInstaller::new(dir.path().join("absent"));
        assert!(missing.status("omp").unwrap_err().contains("cannot run"));
    }

    #[test]
    fn the_fake_follows_the_cli_and_records_calls() {
        let fake = FakeInstaller::new();
        assert_eq!(fake.status("omp"), Ok(IntegrationStatus::NotInstalled));
        fake.install("omp").unwrap();
        assert_eq!(fake.status("omp"), Ok(IntegrationStatus::Installed));
        fake.fail("codex", "denied");
        assert_eq!(fake.install("codex"), Err("denied".to_owned()));
        assert_eq!(fake.installs(), ["omp", "codex"]);
        fake.uninstall("omp").unwrap();
        assert_eq!(fake.status("omp"), Ok(IntegrationStatus::NotInstalled));
    }
}
