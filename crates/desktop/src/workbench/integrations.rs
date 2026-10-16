//! Agent integrations that install themselves (`integrations.auto_install`, default on): when
//! Hutan starts and the daemon answers, the hooks of every harness that is installed and whose
//! integration is not are installed with `hutan integration install` (`crate::integrations`),
//! the way a terminal app puts its hooks in place. Off, nothing is installed by itself; it never
//! uninstalls either (the Agents step of the setup offers that, one harness at a time).
//!
//! "Installed" is the detection the Accounts page uses (`HarnessDirectory`), so the installs
//! wait for `account.list`. The CLI runs off the main thread. Results go to the log; failures
//! show one notice with the command to run by hand.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::Context;

use super::harnesses::{Detection, HARNESSES, Listing, harness_name};
use super::view::Workbench;
use crate::integrations::{IntegrationInstaller, IntegrationStatus, command_line};
use crate::settings::IntegrationsSettings;

/// State of the integrations inside the workbench.
#[derive(Default)]
pub(crate) struct IntegrationControl {
    installer: Option<Arc<dyn IntegrationInstaller>>,
    /// An automatic install waits for the account listing.
    pending: bool,
    /// The automatic install of this launch is done (or was never wanted).
    ran: bool,
    /// What `status` last said, per harness id.
    statuses: HashMap<&'static str, IntegrationStatus>,
    /// A manual install or uninstall is running.
    busy: bool,
}

impl IntegrationControl {
    pub(crate) fn new(installer: Option<Arc<dyn IntegrationInstaller>>) -> Self {
        Self {
            installer,
            ..Self::default()
        }
    }
}

/// What one run found out.
struct Outcome {
    statuses: Vec<(&'static str, IntegrationStatus)>,
    /// `(harness, what went wrong)`.
    failures: Vec<(&'static str, String)>,
    /// The harnesses that were installed by this run.
    installed: Vec<&'static str>,
}

impl Workbench {
    // ---- read access -----------------------------------------------------------------------

    /// `integrations.auto_install`.
    pub fn integrations_auto_install(&self) -> bool {
        self.settings.get::<IntegrationsSettings>().auto_install
    }

    /// Whether the desktop can install integrations at all (it was given an installer).
    pub fn integrations_available(&self) -> bool {
        self.integrations.installer.is_some()
    }

    /// What the last `status` said about `harness` (`None` before it was asked).
    pub fn integration_status(&self, harness: &str) -> Option<IntegrationStatus> {
        self.integrations.statuses.get(harness).copied()
    }

    /// The automatic install of this launch has finished.
    pub fn auto_install_done(&self) -> bool {
        self.integrations.ran
    }

    /// A manual install or uninstall is running.
    pub fn integration_busy(&self) -> bool {
        self.integrations.busy
    }

    /// The harnesses the daemon found on its machine (the Accounts page's detection).
    fn detected_harnesses(&self, cx: &gpui::App) -> Vec<&'static str> {
        let directory = self.env.harnesses.read(cx);
        HARNESSES
            .iter()
            .filter(|h| directory.detection(h.id) == Detection::Installed)
            .map(|h| h.id)
            .collect()
    }

    // ---- installing on launch ----------------------------------------------------------------

    /// Starts the automatic install when it is wanted, has not run yet, and the daemon is up:
    /// asks for the account listing, and the install follows when it arrives
    /// ([`auto_install_step`](Self::auto_install_step)). Cheap to call whenever the connection
    /// or the setting changes.
    pub(crate) fn maybe_auto_install(&mut self, cx: &mut Context<Self>) {
        let control = &self.integrations;
        if control.installer.is_none()
            || control.ran
            || control.pending
            || !self.integrations_auto_install()
            || !self.daemon_connected(cx)
        {
            return;
        }
        self.integrations.pending = true;
        self.refresh_accounts(cx);
    }

    /// The setting was switched on again after this launch's install was skipped.
    pub(crate) fn on_integrations_setting_changed(&mut self, cx: &mut Context<Self>) {
        if self.integrations_auto_install() {
            self.maybe_auto_install(cx);
        }
    }

    /// The account listing changed: when an automatic install waits for it, runs it.
    pub(crate) fn auto_install_step(&mut self, cx: &mut Context<Self>) {
        if !self.integrations.pending {
            return;
        }
        match self.env.harnesses.read(cx).listing().clone() {
            Listing::Ready => {}
            Listing::Failed(error) => {
                tracing::warn!(
                    "integrations are not installed: the harnesses are unknown: {error}"
                );
                self.integrations.pending = false;
                self.integrations.ran = true;
                return;
            }
            Listing::Idle | Listing::Loading => return,
        }
        self.integrations.pending = false;
        self.integrations.ran = true;
        let Some(installer) = self.integrations.installer.clone() else {
            return;
        };
        if !self.integrations_auto_install() {
            return;
        }
        let targets = self.detected_harnesses(cx);
        if targets.is_empty() {
            tracing::info!("no agent harness found; no integration to install");
            return;
        }
        let work = cx.background_executor().spawn(async move {
            let mut outcome = Outcome {
                statuses: Vec::new(),
                failures: Vec::new(),
                installed: Vec::new(),
            };
            for harness in targets {
                match installer.status(harness) {
                    Ok(IntegrationStatus::Installed) => {
                        outcome
                            .statuses
                            .push((harness, IntegrationStatus::Installed));
                    }
                    Ok(_) => match installer.install(harness) {
                        Ok(()) => {
                            outcome
                                .statuses
                                .push((harness, IntegrationStatus::Installed));
                            outcome.installed.push(harness);
                        }
                        Err(error) => outcome.failures.push((harness, error)),
                    },
                    Err(error) => outcome.failures.push((harness, error)),
                }
            }
            outcome
        });
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| this.integrations_installed(outcome, cx))
                .ok();
        })
        .detach();
    }

    fn integrations_installed(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        for (harness, status) in outcome.statuses {
            self.integrations.statuses.insert(harness, status);
        }
        for harness in &outcome.installed {
            tracing::info!(
                "installed the Hutan integration for {}",
                harness_name(harness)
            );
        }
        for (harness, error) in &outcome.failures {
            tracing::warn!(
                "cannot install the Hutan integration for {}: {error}",
                harness_name(harness)
            );
        }
        if let Some(notice) = failure_notice(&outcome.failures, "install") {
            self.set_notice(notice, cx);
        }
        cx.notify();
    }

    // ---- looking and changing by hand (the setup's Agents step) ----------------------------

    /// Asks `hutan integration status` about every detected harness.
    pub fn refresh_integration_statuses(&mut self, cx: &mut Context<Self>) {
        let Some(installer) = self.integrations.installer.clone() else {
            return;
        };
        let targets = self.detected_harnesses(cx);
        if targets.is_empty() {
            return;
        }
        let work = cx.background_executor().spawn(async move {
            targets
                .into_iter()
                .filter_map(|harness| Some((harness, installer.status(harness).ok()?)))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let statuses = work.await;
            this.update(cx, |this, cx| {
                for (harness, status) in statuses {
                    this.integrations.statuses.insert(harness, status);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Installs the hooks of `harness` now, whatever `integrations.auto_install` says.
    pub fn install_integration(&mut self, harness: &'static str, cx: &mut Context<Self>) {
        self.change_integration(harness, true, cx);
    }

    /// Removes the hooks of `harness` (`hutan integration uninstall`).
    pub fn uninstall_integration(&mut self, harness: &'static str, cx: &mut Context<Self>) {
        self.change_integration(harness, false, cx);
    }

    fn change_integration(&mut self, harness: &'static str, install: bool, cx: &mut Context<Self>) {
        let Some(installer) = self.integrations.installer.clone() else {
            return;
        };
        if self.integrations.busy {
            return;
        }
        self.integrations.busy = true;
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let changed = if install {
                installer.install(harness)
            } else {
                installer.uninstall(harness)
            };
            changed.map(|()| installer.status(harness).ok())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.integrations.busy = false;
                let action = if install { "install" } else { "uninstall" };
                match result {
                    Ok(status) => {
                        let status = status.unwrap_or(if install {
                            IntegrationStatus::Installed
                        } else {
                            IntegrationStatus::NotInstalled
                        });
                        this.integrations.statuses.insert(harness, status);
                    }
                    Err(error) => {
                        tracing::warn!(
                            "cannot {action} the Hutan integration for {}: {error}",
                            harness_name(harness)
                        );
                        if let Some(notice) = failure_notice(&[(harness, error)], action) {
                            this.set_notice(notice, cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// The one notice for `failures` of `action`: what went wrong first, and the commands that
/// do it by hand. `None` without failures.
fn failure_notice(failures: &[(&'static str, String)], action: &str) -> Option<String> {
    let (_, first) = failures.first()?;
    let names: Vec<String> = failures.iter().map(|(h, _)| harness_name(h)).collect();
    let commands: Vec<String> = failures
        .iter()
        .map(|(harness, _)| command_line(action, harness))
        .collect();
    Some(format!(
        "Could not {action} Hutan's hooks for {}: {first}. Run {} yourself.",
        names.join(", "),
        commands.join(" and ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_notice_names_every_failure_and_the_commands() {
        assert_eq!(failure_notice(&[], "install"), None);
        let notice = failure_notice(
            &[
                ("claude", "denied".to_owned()),
                ("codex", "full".to_owned()),
            ],
            "install",
        )
        .unwrap();
        assert!(notice.contains("Claude Code, Codex"), "{notice}");
        assert!(notice.contains("denied"), "{notice}");
        assert!(
            notice.contains("hutan integration install claude"),
            "{notice}"
        );
        assert!(
            notice.contains("hutan integration install codex"),
            "{notice}"
        );
    }
}
