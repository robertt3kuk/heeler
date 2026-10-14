//! The daemon's lifetime as the desktop sees it (design.md § Daemon lifecycle): the quit flow
//! driven by `app.on_quit`, the daemon controls shared by the `Daemon` menu, the status-bar
//! popover and the palette, and the "daemon stopped" state that keeps a daemon the user
//! stopped from being brought back until they start it again.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, BackgroundExecutor, Context, SharedString, Task, Window};
use heeler_client::{ClientError, Connection, ConnectionState, StoreState};
use heeler_protocol::ErrorCode;
use heeler_protocol::messages::{DaemonShutdown, DaemonStatusInfo, Empty, Request};
use serde_json::Value;
use tokio::sync::watch;

use super::modal::{Modal, ModalCheckbox, ModalChoice};
use super::panels::{StatusTone, connection_label};
use super::view::Workbench;
use crate::app::DaemonMenu;
use crate::settings::{AppSettings, OnQuit};

/// Longest wait for a stopping daemon to remove its socket.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// Longest wait for a started daemon to accept the connection.
const START_WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(25);

/// Shows a file in Finder.
type RevealHandler = Rc<dyn Fn(&Path, &mut App)>;

/// A daemon operation the app is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonBusy {
    Stopping,
    Starting,
    Restarting,
}

impl DaemonBusy {
    fn label(self) -> &'static str {
        match self {
            DaemonBusy::Stopping => "stopping heelerd…",
            DaemonBusy::Starting => "starting heelerd…",
            DaemonBusy::Restarting => "restarting heelerd…",
        }
    }
}

/// The last `daemon.status` answer and when it arrived (the uptime counts on from there).
#[derive(Debug, Clone)]
pub(crate) struct FetchedStatus {
    pub info: DaemonStatusInfo,
    pub at: Instant,
}

/// State of the daemon controls and of the quit flow.
pub(crate) struct DaemonControl {
    /// The user stopped the daemon from the app: autostart is off until they start it.
    pub stopped: bool,
    pub busy: Option<DaemonBusy>,
    pub popover: bool,
    pub status: Option<FetchedStatus>,
    fetching: bool,
    /// Quitting has started; nothing else may run.
    pub quitting: bool,
    /// What the menu bar was last built for.
    menu: Option<DaemonMenu>,
    task: Option<Task<()>>,
    fetch: Option<Task<()>>,
    /// `heelerd.log`, as `Open Logs` reveals it and the popover shows it.
    pub log_path: Option<PathBuf>,
    /// Reveals a file in Finder.
    reveal: RevealHandler,
}

impl DaemonControl {
    pub fn new() -> Self {
        Self {
            stopped: false,
            busy: None,
            popover: false,
            status: None,
            fetching: false,
            quitting: false,
            menu: None,
            task: None,
            fetch: None,
            log_path: log_file(),
            reveal: Rc::new(|path, cx| cx.reveal_path(path)),
        }
    }
}

/// `heelerd.log` where the daemon writes it: `$HEELER_HOME/logs`, else `~/Library/Logs/Heeler`.
pub fn log_file() -> Option<PathBuf> {
    resolve_log_file(
        std::env::var_os("HEELER_HOME"),
        std::env::home_dir().as_deref(),
    )
}

fn resolve_log_file(heeler_home: Option<OsString>, user_home: Option<&Path>) -> Option<PathBuf> {
    let dir = match heeler_home.filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home).join("logs"),
        None => user_home?.join("Library/Logs/Heeler"),
    };
    Some(dir.join("heelerd.log"))
}

fn sessions_phrase(live: usize) -> String {
    format!("{live} live session{}", if live == 1 { "" } else { "s" })
}

/// Asks the daemon to shut down and waits until its socket is gone (it is removed just before
/// the daemon releases its lock, so a successor can start). A daemon that is already gone
/// counts as stopped.
async fn shutdown_and_wait(
    connection: &Connection,
    stop_sessions: bool,
    background: &BackgroundExecutor,
) -> Result<(), ClientError> {
    match connection
        .call::<Value>(Request::DaemonShutdown(DaemonShutdown { stop_sessions }))
        .await
    {
        Ok(_) | Err(ClientError::Disconnected) => {}
        Err(error) => return Err(error),
    }
    if let Some(socket) = connection.socket_path().map(Path::to_owned) {
        let gone = background
            .spawn(async move { wait_socket_gone(&socket, STOP_WAIT) })
            .await;
        if !gone {
            tracing::warn!("heelerd accepted the shutdown but its socket is still there");
        }
    }
    Ok(())
}

/// Blocking; runs on the background executor.
fn wait_socket_gone(socket: &Path, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while std::fs::symlink_metadata(socket).is_ok() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
    true
}

/// Waits (without a timer: reconnect attempts publish a new state at least every 5 s) until
/// the connection is up again; `false` after `limit`.
async fn wait_connected(state: &mut watch::Receiver<Arc<StoreState>>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if state.borrow_and_update().connection == ConnectionState::Connected {
            return true;
        }
        if Instant::now() >= deadline || state.changed().await.is_err() {
            return false;
        }
    }
}

impl Workbench {
    // ---- read access -------------------------------------------------------------------------

    /// Quitting has started (the connection is closed and the app is going away).
    pub fn quitting(&self) -> bool {
        self.daemon.quitting
    }

    /// The user stopped the daemon from the app and has not started it again.
    pub fn daemon_stopped(&self) -> bool {
        self.daemon.stopped
    }

    /// The daemon operation in progress, if any.
    pub fn daemon_busy(&self) -> Option<DaemonBusy> {
        self.daemon.busy
    }

    pub fn daemon_popover_open(&self) -> bool {
        self.daemon.popover
    }

    /// The last `daemon.status` the popover fetched (`None` while disconnected).
    pub fn daemon_status(&self) -> Option<&DaemonStatusInfo> {
        self.daemon.status.as_ref().map(|s| &s.info)
    }

    /// The working mode in effect (`app.on_quit`).
    pub fn on_quit_mode(&self) -> OnQuit {
        self.settings.get::<AppSettings>().on_quit
    }

    /// Sessions the daemon runs right now.
    pub fn live_sessions(&self, cx: &App) -> usize {
        self.sessions
            .read(cx)
            .sessions()
            .iter()
            .filter(|s| s.state.is_live())
            .count()
    }

    pub(crate) fn connection(&self, cx: &App) -> Connection {
        self.sessions.read(cx).connection().clone()
    }

    pub(crate) fn daemon_connected(&self, cx: &App) -> bool {
        self.sessions.read(cx).connection_state() == ConnectionState::Connected
    }

    /// What the connection status item and the popover headline say.
    pub(crate) fn daemon_line(&self, cx: &App) -> (String, StatusTone) {
        if let Some(busy) = self.daemon.busy {
            return (busy.label().to_owned(), StatusTone::Warn);
        }
        let state = if self.daemon.stopped {
            ConnectionState::Stopped
        } else {
            self.sessions.read(cx).connection_state()
        };
        connection_label(state)
    }

    /// The text of the connection status item ("heelerd connected", "heelerd stopped", …).
    pub fn daemon_status_line(&self, cx: &App) -> String {
        self.daemon_line(cx).0
    }

    // ---- working mode ------------------------------------------------------------------------

    /// Writes `app.on_quit` to `settings.toml`; every view of the setting follows through the
    /// store's change notification.
    pub fn set_on_quit(&mut self, mode: OnQuit, cx: &mut Context<Self>) {
        if let Err(err) = self
            .settings
            .write_user("app.on_quit", toml::Value::String(mode.key().to_owned()))
        {
            tracing::warn!("cannot save the working mode: {err}");
            self.set_notice(format!("Cannot save the working mode: {err}"), cx);
            return;
        }
        self.sync_daemon_menu(cx);
        cx.notify();
    }

    // ---- menu bar ----------------------------------------------------------------------------

    fn menu_state(&self, cx: &App) -> DaemonMenu {
        DaemonMenu {
            on_quit: self.on_quit_mode(),
            running: self.daemon_connected(cx),
        }
    }

    /// Rebuilds the menu bar for the current layout presets and daemon state.
    pub(crate) fn refresh_menus(&mut self, cx: &mut Context<Self>) {
        let state = self.menu_state(cx);
        cx.set_menus(crate::app::menus(&self.presets, &state));
        self.daemon.menu = Some(state);
    }

    /// Rebuilds the menu bar when the daemon menu's checks or enabled items changed.
    pub(crate) fn sync_daemon_menu(&mut self, cx: &mut Context<Self>) {
        if self.daemon.menu != Some(self.menu_state(cx)) {
            self.refresh_menus(cx);
        }
    }

    /// The connection changed: keep the menu, the popover data and the terminals current.
    pub(crate) fn on_daemon_state_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_daemon_menu(cx);
        if self.daemon.popover {
            self.refresh_daemon_status(cx);
        }
        if !self.daemon_connected(cx) {
            self.daemon.status = None;
        }
    }

    // ---- popover -----------------------------------------------------------------------------

    /// `daemon::Status`: opens the popover with the daemon's status and controls.
    pub fn show_daemon_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.context_menu = None;
        self.daemon.popover = true;
        window.focus(&self.daemon_focus, cx);
        self.refresh_daemon_status(cx);
        cx.notify();
    }

    pub fn close_daemon_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.daemon.popover) {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    fn refresh_daemon_status(&mut self, cx: &mut Context<Self>) {
        if !self.daemon_connected(cx) {
            self.daemon.status = None;
            return;
        }
        if self.daemon.fetching {
            return;
        }
        self.daemon.fetching = true;
        let connection = self.connection(cx);
        self.daemon.fetch = Some(cx.spawn(async move |this, cx| {
            let info = connection
                .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
                .await;
            this.update(cx, |this, cx| {
                this.daemon.fetching = false;
                match info {
                    Ok(info) => {
                        this.daemon.status = Some(FetchedStatus {
                            info,
                            at: Instant::now(),
                        });
                    }
                    Err(error) => tracing::debug!("daemon.status failed: {error}"),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    // ---- stop / start / restart --------------------------------------------------------------

    /// `daemon::Stop`: stops the sessions and the daemon; the app stays open and shows the
    /// daemon as stopped until it is started again. Confirms first while sessions are live.
    pub fn stop_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.daemon.busy.is_some() {
            return;
        }
        let live = self.live_sessions(cx);
        if !self.daemon_connected(cx) || live == 0 {
            self.begin_stop(false, window, cx);
            return;
        }
        self.open_confirm(
            "Stop the daemon?",
            format!(
                "{} will be stopped with the daemon. Heeler stays open.",
                sessions_phrase(live)
            ),
            "Stop Sessions & Daemon",
            |this, window, cx| this.begin_stop(false, window, cx),
            window,
            cx,
        );
    }

    /// `daemon::Restart`: stops the daemon, then starts a new one (a new epoch: terminals end
    /// or re-attach as the daemon says). Confirms first while sessions are live.
    pub fn restart_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.daemon.busy.is_some() {
            return;
        }
        let live = self.live_sessions(cx);
        if !self.daemon_connected(cx) {
            self.begin_start(false, cx);
            return;
        }
        if live == 0 {
            self.begin_stop(true, window, cx);
            return;
        }
        self.open_confirm(
            "Restart the daemon?",
            format!(
                "{} will be stopped. The new daemon starts empty.",
                sessions_phrase(live)
            ),
            "Stop Sessions & Restart",
            |this, window, cx| this.begin_stop(true, window, cx),
            window,
            cx,
        );
    }

    /// `daemon::Start`: starts `heelerd` and reconnects.
    pub fn start_daemon(&mut self, cx: &mut Context<Self>) {
        if self.daemon.busy.is_some() {
            return;
        }
        if self.daemon_connected(cx) {
            self.set_notice("The daemon is already running", cx);
            return;
        }
        self.begin_start(false, cx);
    }

    /// `daemon::OpenLogs`: reveals `heelerd.log` in Finder. Nothing else happens.
    pub fn open_daemon_logs(&mut self, cx: &mut Context<Self>) {
        match self.daemon.log_path.clone() {
            Some(path) if path.is_file() => (self.daemon.reveal)(&path, cx),
            Some(path) => {
                self.set_notice(format!("No daemon log yet ({})", path.display()), cx);
            }
            None => self.set_notice("Cannot locate the daemon log", cx),
        }
    }

    /// Where `Open Logs` looks for `heelerd.log` (default: the daemon's own log location).
    pub fn set_daemon_log_path(&mut self, path: PathBuf) {
        self.daemon.log_path = Some(path);
    }

    /// How `Open Logs` reveals a file (default: Finder through the platform).
    pub fn set_reveal_handler(&mut self, reveal: impl Fn(&Path, &mut App) + 'static) {
        self.daemon.reveal = Rc::new(reveal);
    }

    /// Stops reconnecting, asks the daemon to shut down (stopping its sessions) and waits for
    /// it to go. `restart` then starts a new one; otherwise the daemon stays stopped.
    fn begin_stop(&mut self, restart: bool, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.connection(cx);
        // Off first: the connection must not bring the daemon back while it shuts down.
        connection.set_autostart(false);
        self.daemon.busy = Some(if restart {
            DaemonBusy::Restarting
        } else {
            DaemonBusy::Stopping
        });
        if !restart {
            self.set_daemon_stopped(true, cx);
        }
        cx.notify();
        let background = cx.background_executor().clone();
        self.daemon.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = shutdown_and_wait(&connection, true, &background).await;
            this.update(cx, |this, cx| this.stop_finished(restart, result, cx))
                .ok();
        }));
    }

    fn stop_finished(
        &mut self,
        restart: bool,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Err(error) => {
                tracing::warn!("cannot stop the daemon: {error}");
                self.connection(cx).set_autostart(true);
                self.daemon.busy = None;
                self.set_daemon_stopped(false, cx);
                self.set_notice(format!("Cannot stop the daemon: {error}"), cx);
            }
            Ok(()) if restart => self.begin_start(true, cx),
            Ok(()) => {
                self.daemon.busy = None;
                cx.notify();
            }
        }
    }

    /// Allows autostart again (which reconnects at once and starts `heelerd`) and waits for the
    /// connection.
    fn begin_start(&mut self, restarting: bool, cx: &mut Context<Self>) {
        let connection = self.connection(cx);
        self.daemon.busy = Some(if restarting {
            DaemonBusy::Restarting
        } else {
            DaemonBusy::Starting
        });
        self.set_daemon_stopped(false, cx);
        let mut state = connection.state();
        connection.set_autostart(true);
        cx.notify();
        self.daemon.task = Some(cx.spawn(async move |this, cx| {
            let started = wait_connected(&mut state, START_WAIT).await;
            this.update(cx, |this, cx| {
                this.daemon.busy = None;
                if !started {
                    this.set_notice("The daemon did not start; see the daemon log", cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_daemon_stopped(&mut self, stopped: bool, cx: &mut Context<Self>) {
        self.daemon.stopped = stopped;
        self.sessions
            .update(cx, |store, cx| store.set_daemon_stopped(stopped, cx));
    }

    // ---- quit --------------------------------------------------------------------------------

    /// ⌘Q, the app menu and closing the last window (design.md § Daemon lifecycle): the daemon
    /// stops with the app when no session is live, else `app.on_quit` decides. While
    /// disconnected the app quits at once.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.daemon.quitting {
            return;
        }
        if !self.daemon_connected(cx) {
            self.finish_quit(cx);
            return;
        }
        if self.live_sessions(cx) == 0 {
            self.shutdown_then_quit(false, window, cx);
            return;
        }
        self.quit_with_live_sessions(window, cx);
    }

    fn quit_with_live_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.on_quit_mode() {
            OnQuit::Ask => self.open_quit_dialog(window, cx),
            OnQuit::KeepRunning => self.finish_quit(cx),
            OnQuit::StopDaemon => self.shutdown_then_quit(true, window, cx),
        }
    }

    /// SIGTERM / SIGINT / SIGHUP: save, disconnect and quit like `keep_running`; never asks
    /// and never stops the daemon.
    pub fn quit_leaving_daemon(&mut self, cx: &mut Context<Self>) {
        self.finish_quit(cx);
    }

    /// The window's close button: closing the last window quits (the same flow) and keeps the
    /// window until it is decided; closing any other window just closes it. Returns whether
    /// the window may close now.
    pub(crate) fn window_close_requested(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.flush_save();
        if self.daemon.quitting || cx.windows().len() > 1 {
            return true;
        }
        self.request_quit(window, cx);
        false
    }

    fn open_quit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live = self.live_sessions(cx);
        let message = format!(
            "{} still running. Keep them running in the background (heelerd keeps them), \
             or stop them together with the daemon?",
            sessions_phrase(live)
        );
        let modal = Modal {
            title: "Quit Heeler?".into(),
            message: SharedString::from(message),
            confirm_label: "Keep sessions running".into(),
            input: None,
            choice: Some(ModalChoice {
                label: "Stop sessions & daemon".into(),
                run: Rc::new(|this, result, window, cx| {
                    if result.checked {
                        this.set_on_quit(OnQuit::StopDaemon, cx);
                    }
                    this.shutdown_then_quit(true, window, cx);
                }),
            }),
            checkbox: Some(ModalCheckbox {
                label: "Don't ask again".into(),
                checked: false,
            }),
            on_confirm: Rc::new(|this, result, _, cx| {
                if result.checked {
                    this.set_on_quit(OnQuit::KeepRunning, cx);
                }
                this.finish_quit(cx);
            }),
        };
        self.open_modal(modal, window, cx);
    }

    /// `daemon.shutdown`, then quit. If sessions started since the decision (the daemon refuses
    /// a shutdown that does not stop them), the decision is made again.
    fn shutdown_then_quit(
        &mut self,
        stop_sessions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.connection(cx);
        let background = cx.background_executor().clone();
        self.set_notice("Stopping the daemon…", cx);
        self.daemon.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = shutdown_and_wait(&connection, stop_sessions, &background).await;
            this.update_in(cx, |this, window, cx| match result {
                Err(ClientError::Remote(body)) if body.code == ErrorCode::InvalidState => {
                    this.notice = None;
                    this.quit_with_live_sessions(window, cx);
                }
                Err(error) => {
                    tracing::warn!("quitting without a clean daemon shutdown: {error}");
                    this.finish_quit(cx);
                }
                Ok(()) => this.finish_quit(cx),
            })
            .ok();
        }));
    }

    /// Saves the layout, closes the connection and quits (exit code 0). The daemon is left as
    /// it is; callers that want it stopped have asked it through `daemon.shutdown` already.
    fn finish_quit(&mut self, cx: &mut Context<Self>) {
        if self.daemon.quitting {
            return;
        }
        self.daemon.quitting = true;
        self.flush_save();
        self.connection(cx).shutdown();
        cx.quit();
    }
}
