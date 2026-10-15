//! The workbench side of the macOS integration (roadmap M6.4, `crate::macos`): keep running
//! as a login item, the menu bar status item, the Dock badge and the global Assistant
//! shortcut.
//!
//! Each of the four is driven by settings (`app.keep_running`, `app.menu_bar`,
//! `app.dock_badge`, `assistant.global_shortcut`) and follows the attention model. The
//! `sync_*` methods compare the wanted state with what was last handed to the system and only
//! call the trait when something changed, so they are cheap to run on every session update and
//! on every settings reload. Nothing here knows whether the traits are real.

use std::rc::Rc;

use gpui::{App, Context, Task, Window};
use hutan_client::ConnectionState;

use super::view::Workbench;
use crate::macos::{
    DockBadge, GlobalHotkey, LoginItem, LoginItemStatus, MacEvent, MacServices, MenuCommand,
    Shortcut, StatusItemHost, StatusItemModel,
};
use crate::settings::{AppSettings, AssistantSettings};
use hutan_settings::ChangeSet;

/// State of the macOS integration inside the workbench.
pub(crate) struct MacControl {
    login_item: Rc<dyn LoginItem>,
    status_item: Rc<dyn StatusItemHost>,
    dock: Rc<dyn DockBadge>,
    hotkey: Rc<dyn GlobalHotkey>,
    /// The status of the login item the last time it was read.
    login_status: LoginItemStatus,
    /// What the Dock badge shows now (`None` until it was set once).
    badge: Option<usize>,
    /// What the menu bar item shows now (`None`: not on the menu bar).
    item: Option<StatusItemModel>,
    /// The shortcut setting the system was last asked about, whatever the outcome: a refused
    /// shortcut is reported once, not on every settings reload.
    shortcut_tried: Option<String>,
    shortcut_active: bool,
    pub(crate) launchd_daemon: bool,
    pub(crate) launchd_grace: std::time::Duration,
    pub(crate) grace_task: Option<Task<()>>,
    pub(crate) events_task: Option<Task<()>>,
}

impl MacControl {
    /// Takes the services apart; the receiver of their events is returned for the workbench
    /// to listen on.
    pub(crate) fn new(services: MacServices) -> (Self, Option<crate::macos::EventReceiver>) {
        let MacServices {
            login_item,
            status_item,
            dock,
            hotkey,
            events,
            launchd_daemon,
            launchd_grace,
        } = services;
        let login_status = login_item.status();
        (
            Self {
                login_item,
                status_item,
                dock,
                hotkey,
                login_status,
                badge: None,
                item: None,
                shortcut_tried: None,
                shortcut_active: false,
                launchd_daemon,
                launchd_grace,
                grace_task: None,
                events_task: None,
            },
            events,
        )
    }

    pub(crate) fn login_status(&self) -> LoginItemStatus {
        self.login_status
    }

    pub(crate) fn keep_running_available(&self) -> bool {
        self.login_item.available()
    }
}

/// Where the connection stands, in the words the menu bar item uses.
fn connection_words(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Connecting => "connecting to hutand…",
        ConnectionState::Connected => "hutand connected",
        ConnectionState::Reconnecting { .. } => "hutand reconnecting…",
        ConnectionState::Stopped => "hutand stopped",
    }
}

impl Workbench {
    // ---- keep running ----------------------------------------------------------------------

    /// `app.keep_running` as set (the toggle's position, whatever the system says).
    pub fn keep_running_enabled(&self) -> bool {
        self.settings.get::<AppSettings>().keep_running
    }

    /// Whether the toggle can be used: the desktop runs from Hutan.app.
    pub fn keep_running_available(&self) -> bool {
        self.mac.keep_running_available()
    }

    /// What the system said about the launch agent when it was last asked.
    pub fn keep_running_status(&self) -> LoginItemStatus {
        self.mac.login_status()
    }

    /// The launch agent is registered *and* allowed: launchd owns the daemon, quitting never
    /// stops it and the quit question is not asked. Reads the system fresh, because the user
    /// may have changed it in System Settings since the window last looked.
    pub fn keep_running_active(&self) -> bool {
        self.keep_running_available()
            && self.keep_running_enabled()
            && self.mac.login_item.status() == LoginItemStatus::Enabled
    }

    /// Settings → General, the `Daemon` menu and the menu bar item: flips keep running.
    pub fn toggle_keep_running(&mut self, cx: &mut Context<Self>) {
        self.set_keep_running(!self.keep_running_enabled(), cx);
    }

    /// Writes `app.keep_running`, which registers or unregisters the launch agent.
    pub fn set_keep_running(&mut self, on: bool, cx: &mut Context<Self>) {
        if !self.keep_running_available() {
            self.set_notice("Keep running is available in Hutan.app", cx);
            return;
        }
        if let Err(error) = self
            .settings
            .write_user("app.keep_running", toml::Value::Boolean(on))
        {
            tracing::warn!("cannot save keep running: {error}");
            self.set_notice(format!("Cannot save keep running: {error}"), cx);
            return;
        }
        // The settings change would do it too; doing it here keeps the answer immediate.
        self.sync_keep_running(cx);
    }

    /// "Open Login Items": System Settings → General → Login Items.
    pub fn open_login_items(&mut self, _cx: &mut Context<Self>) {
        self.mac.login_item.open_login_items_settings();
    }

    /// Makes the launch agent match `app.keep_running`: registers it when the setting is on
    /// and nothing is registered, unregisters it when the setting is off and something is.
    /// A failure is shown and the setting goes back to what is true.
    pub(crate) fn sync_keep_running(&mut self, cx: &mut Context<Self>) {
        if self.keep_running_available() {
            let wanted = self.keep_running_enabled();
            let status = self.mac.login_item.status();
            if wanted && !status.registered() {
                self.register_launch_agent(cx);
            } else if !wanted && status.registered() {
                self.unregister_launch_agent(cx);
            }
        }
        self.refresh_login_status(cx);
    }

    fn register_launch_agent(&mut self, cx: &mut Context<Self>) {
        let failure = match self.mac.login_item.register() {
            Err(error) => Some(format!("Cannot keep Hutan running: {error}")),
            Ok(()) => match self.mac.login_item.status() {
                LoginItemStatus::NotFound => Some(
                    "Cannot keep Hutan running: the launch agent is missing from Hutan.app"
                        .to_owned(),
                ),
                LoginItemStatus::RequiresApproval => {
                    self.set_notice(
                        "Allow Hutan in System Settings → General → Login Items to keep it running",
                        cx,
                    );
                    None
                }
                LoginItemStatus::Enabled | LoginItemStatus::NotRegistered => None,
            },
        };
        if let Some(message) = failure {
            tracing::warn!("{message}");
            self.write_keep_running_back(false, cx);
            self.set_notice(message, cx);
        }
    }

    fn unregister_launch_agent(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.mac.login_item.unregister() {
            let message = format!("Cannot stop keeping Hutan running: {error}");
            tracing::warn!("{message}");
            self.write_keep_running_back(true, cx);
            self.set_notice(message, cx);
        }
    }

    /// Puts `app.keep_running` back to what the system says after a refused change.
    fn write_keep_running_back(&mut self, value: bool, cx: &mut Context<Self>) {
        if let Err(error) = self
            .settings
            .write_user("app.keep_running", toml::Value::Boolean(value))
        {
            tracing::warn!("cannot restore keep running: {error}");
            self.set_notice(format!("Cannot save keep running: {error}"), cx);
        }
    }

    /// Reads the login item's status; when it changed (the user answered in System Settings,
    /// or a registration just happened) the menus and the status item follow.
    pub fn refresh_login_status(&mut self, cx: &mut Context<Self>) {
        let status = self.mac.login_item.status();
        if status != self.mac.login_status {
            self.mac.login_status = status;
            self.sync_daemon_menu(cx);
            self.sync_status_item(cx);
            cx.notify();
        }
    }

    /// The window waited `launchd_grace` for a launchd-started daemon: when none answered, the
    /// connection may start `hutand` itself (it was stopped on purpose, or the login item has
    /// not run). While the user has stopped the daemon from the app, it stays down.
    pub(crate) fn launchd_grace_over(&mut self, cx: &mut Context<Self>) {
        if !self.daemon_connected(cx) && !self.daemon.stopped {
            tracing::info!("no launchd-started daemon answered; starting hutand");
            self.connection(cx).set_autostart(true);
        }
    }

    // ---- menu bar item and Dock badge ------------------------------------------------------

    fn status_item_model(&self, cx: &App) -> StatusItemModel {
        let daemon = if let Some(busy) = self.daemon.busy {
            busy.label().to_owned()
        } else if self.daemon.stopped {
            connection_words(ConnectionState::Stopped).to_owned()
        } else {
            connection_words(self.sessions.read(cx).connection_state()).to_owned()
        };
        StatusItemModel {
            daemon,
            needs_you: self.sessions.read(cx).attention_entries(),
            keep_running: self.keep_running_enabled(),
            keep_running_available: self.keep_running_available(),
            keep_running_status: self.mac.login_status,
        }
    }

    /// Puts the menu bar item on the menu bar, updates it, or takes it off, per
    /// `app.menu_bar` and the attention model.
    pub(crate) fn sync_status_item(&mut self, cx: &mut Context<Self>) {
        if self.settings.get::<AppSettings>().menu_bar {
            let model = self.status_item_model(cx);
            if self.mac.item.as_ref() != Some(&model) {
                self.mac.status_item.show(&model);
                self.mac.item = Some(model);
            }
        } else if self.mac.item.take().is_some() {
            self.mac.status_item.hide();
        }
    }

    /// Sets the Dock badge to the number of sessions that need the user (`app.dock_badge`).
    pub(crate) fn sync_dock(&mut self, cx: &mut Context<Self>) {
        let count = if self.settings.get::<AppSettings>().dock_badge {
            self.sessions.read(cx).attention_entries().len()
        } else {
            0
        };
        if self.mac.badge != Some(count) {
            self.mac.dock.set_count(count);
            self.mac.badge = Some(count);
        }
    }

    // ---- global shortcut -------------------------------------------------------------------

    /// Registers `assistant.global_shortcut` when it changed since the system was last asked.
    /// A shortcut that cannot be registered (taken by another app, or not a shortcut) is
    /// reported once in the window; the setting is left alone.
    pub(crate) fn sync_hotkey(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .settings
            .get::<AssistantSettings>()
            .global_shortcut
            .trim()
            .to_owned();
        if self.mac.shortcut_tried.as_deref() == Some(wanted.as_str()) {
            return;
        }
        self.mac.shortcut_tried = Some(wanted.clone());
        if self.mac.shortcut_active {
            self.mac.hotkey.unregister();
            self.mac.shortcut_active = false;
        }
        if wanted.is_empty() {
            return;
        }
        let notice = match Shortcut::parse(&wanted) {
            Err(error) => Some(format!(
                "The global shortcut \"{wanted}\" is not valid: {error} \
                 (assistant.global_shortcut)"
            )),
            Ok(shortcut) => match self.mac.hotkey.register(&shortcut) {
                Ok(()) => {
                    self.mac.shortcut_active = true;
                    None
                }
                Err(error) => Some(format!(
                    "The global shortcut {} is not available: {error}. The setting \
                     assistant.global_shortcut is unchanged; the shortcut still works inside Hutan.",
                    shortcut.display
                )),
            },
        };
        if let Some(text) = notice {
            tracing::warn!("{text}");
            self.set_notice(text, cx);
        }
    }

    // ---- wiring ----------------------------------------------------------------------------

    /// Brings every surface up to date (the window just opened).
    pub(crate) fn sync_mac_all(&mut self, cx: &mut Context<Self>) {
        self.sync_keep_running(cx);
        self.sync_status_item(cx);
        self.sync_dock(cx);
        self.sync_hotkey(cx);
    }

    /// The sessions, approvals or connection changed.
    pub(crate) fn sync_mac_surfaces(&mut self, cx: &mut Context<Self>) {
        self.sync_status_item(cx);
        self.sync_dock(cx);
    }

    /// A settings reload: the namespaces this module reads.
    pub(crate) fn on_mac_settings_changed(&mut self, changes: &ChangeSet, cx: &mut Context<Self>) {
        if changes.contains("app") {
            self.sync_keep_running(cx);
            self.sync_status_item(cx);
            self.sync_dock(cx);
        }
        if changes.contains("assistant") {
            self.sync_hotkey(cx);
        }
    }

    /// The system asked for something: a menu bar choice or the global shortcut.
    pub(crate) fn on_mac_event(
        &mut self,
        event: MacEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.daemon.quitting {
            return;
        }
        match event {
            MacEvent::Hotkey => {
                cx.activate(true);
                window.activate_window();
                self.open_assistant(window, cx);
            }
            MacEvent::Menu(MenuCommand::OpenHutan) => {
                cx.activate(true);
                window.activate_window();
            }
            MacEvent::Menu(MenuCommand::NewTerminal) => {
                cx.activate(true);
                window.activate_window();
                self.new_terminal(window, cx);
            }
            MacEvent::Menu(MenuCommand::FocusSession(session)) => {
                cx.activate(true);
                window.activate_window();
                if self.sessions.read(cx).session(session).is_none() {
                    self.set_notice("That session no longer exists", cx);
                } else if !self.show_assistant_session(session, window, cx) {
                    self.open_session(session, window, cx);
                }
            }
            MacEvent::Menu(MenuCommand::ToggleKeepRunning) => self.toggle_keep_running(cx),
            MacEvent::Menu(MenuCommand::Quit) => self.request_quit(window, cx),
        }
    }
}
