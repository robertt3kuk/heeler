//! Application bootstrap: keymap, menus, window options, quit routing, and the `run` entry point.

use crate::macos::status_item::keep_running_title;
use crate::macos::{LoginItem, LoginItemStatus, MacServices};
use crate::settings::{AppSettings, OnQuit, WorkspaceSort};
use crate::workbench::actions::{self, ApplyPreset, SetOnQuit, daemon};
use crate::workbench::model::LayoutPreset;
use crate::workbench::persistence;
use crate::workbench::{Workbench, WorkbenchOptions};
use gpui::{
    App, AppContext as _, Bounds, Context, Menu, MenuItem, SystemMenuType, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use hutan_client::{ConnectOptions, Connection};
use hutan_protocol::messages::{ClientInfo, ClientKind};
use hutan_settings::{SettingsPaths, SettingsStore};
use std::path::{Path, PathBuf};

/// What the `Daemon` menu shows: the working mode (checked) and whether the daemon is up
/// (Restart and Stop apply) or not (Start applies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonMenu {
    pub on_quit: OnQuit,
    pub running: bool,
    /// `app.keep_running`, whether it can be used (the app runs from Hutan.app) and what the
    /// system says about the launch agent.
    pub keep_running: bool,
    pub keep_running_available: bool,
    pub keep_running_status: LoginItemStatus,
}

impl Default for DaemonMenu {
    fn default() -> Self {
        Self {
            on_quit: OnQuit::Ask,
            running: true,
            keep_running: false,
            keep_running_available: false,
            keep_running_status: LoginItemStatus::NotRegistered,
        }
    }
}

/// Runs `run` on the workbench window; quits the app when there is none. Deferred because
/// the caller may be inside that window's own update.
fn with_workbench(
    cx: &mut App,
    run: impl FnOnce(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
) {
    let handle = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<Workbench>());
    match handle {
        Some(handle) => cx.defer(move |cx| {
            if handle.update(cx, run).is_err() {
                cx.quit();
            }
        }),
        None => cx.quit(),
    }
}

/// Install the global actions and the menu bar. Key bindings come from the workbench (defaults
/// plus `keymap.toml`), which rebinds them live. Quit is decided by the workbench (live
/// sessions, `app.on_quit`); this handler only catches the action when no window has focus.
pub fn init(cx: &mut App, presets: &[LayoutPreset]) {
    cx.on_action(|_: &actions::Quit, cx| {
        with_workbench(cx, |workbench, window, cx| {
            workbench.request_quit(window, cx)
        });
    });
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    cx.set_menus(menus(presets, &DaemonMenu::default()));
}

/// App, Session, Workspace, View, Daemon, and Window menus. Items dispatch the same named actions as the
/// keymap and palette.
pub fn menus(presets: &[LayoutPreset], daemon_menu: &DaemonMenu) -> Vec<Menu> {
    let preset_items = presets.iter().map(|p| {
        MenuItem::action(
            crate::workbench::preset_label(&p.name),
            ApplyPreset(p.name.clone()),
        )
    });
    vec![
        Menu::new("Hutan").items([
            MenuItem::action("About Hutan", actions::About),
            MenuItem::separator(),
            MenuItem::action("Settings…", actions::OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Hutan", actions::Quit),
        ]),
        Menu::new("Session").items([
            MenuItem::action("New Terminal", actions::NewTerminal),
            MenuItem::action("New Agent", actions::NewAgent),
            MenuItem::action("New Chat", actions::NewChat),
            MenuItem::separator(),
            MenuItem::action("Rename…", actions::RenameSession),
            MenuItem::action("Restart", actions::RestartSession),
            MenuItem::action("Stop…", actions::StopSession),
            MenuItem::action("Copy Session ID", actions::CopySessionId),
            MenuItem::separator(),
            MenuItem::action("Jump to Next That Needs You", actions::JumpToNextAttention),
        ]),
        Menu::new("Workspace").items([
            MenuItem::action("Open Project…", actions::OpenProject),
            MenuItem::action("Open Folder…", actions::OpenFolder),
            MenuItem::action("Jump to Workspace or Session…", actions::OpenJump),
            MenuItem::action("Go to Home", actions::GoHome),
            MenuItem::separator(),
            MenuItem::action("Pin / Unpin", actions::PinActiveWorkspace),
            MenuItem::action("Rename…", actions::RenameActiveWorkspace),
            MenuItem::action("Add Folder…", actions::AddFolderToWorkspace),
            MenuItem::action("Reveal in Finder", actions::RevealWorkspace),
            MenuItem::action("Close Workspace", actions::CloseActiveWorkspace),
            MenuItem::separator(),
            MenuItem::submenu(
                Menu::new("Sort").items(
                    WorkspaceSort::ALL.map(|mode| {
                        MenuItem::action(mode.label(), actions::SetWorkspaceSort(mode))
                    }),
                ),
            ),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Left Dock", actions::ToggleLeftDock),
            MenuItem::action("Toggle Right Dock", actions::ToggleRightDock),
            MenuItem::action("Toggle Bottom Dock", actions::ToggleBottomDock),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Layout Preset").items(preset_items)),
            MenuItem::action("Save Layout as Preset…", actions::SaveLayoutAsPreset),
            MenuItem::action("Overview", actions::ToggleOverview),
            MenuItem::action("Toggle Zoom Pane", actions::ToggleZoom),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Density").items([
                MenuItem::action("Compact", actions::SetDensityCompact),
                MenuItem::action("Comfortable", actions::SetDensityComfortable),
            ])),
            MenuItem::separator(),
            MenuItem::action("Command Palette…", actions::TogglePalette),
        ]),
        Menu::new("Daemon").items([
            MenuItem::action("Status…", daemon::Status),
            MenuItem::separator(),
            MenuItem::action("Restart Daemon", daemon::Restart).disabled(!daemon_menu.running),
            MenuItem::action("Stop Daemon", daemon::Stop).disabled(!daemon_menu.running),
            MenuItem::action("Start Daemon", daemon::Start).disabled(daemon_menu.running),
            MenuItem::action("Open Logs", daemon::OpenLogs),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Working Mode").items(OnQuit::ALL.map(|mode| {
                MenuItem::action(mode.label(), SetOnQuit(mode)).checked(mode == daemon_menu.on_quit)
            }))),
            MenuItem::separator(),
            MenuItem::action(
                keep_running_title(
                    daemon_menu.keep_running_available,
                    daemon_menu.keep_running_status,
                ),
                daemon::ToggleKeepRunning,
            )
            .checked(daemon_menu.keep_running)
            .disabled(!daemon_menu.keep_running_available),
            MenuItem::action("Open Login Items", daemon::OpenLoginItems)
                .disabled(daemon_menu.keep_running_status != LoginItemStatus::RequiresApproval),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", actions::MinimizeWindow),
            MenuItem::action("Zoom", actions::ZoomWindow),
        ]),
    ]
}

/// Window options for the workbench: transparent titlebar with inset traffic lights and a
/// blurred background (the left dock paints translucent; everything else paints opaque).
pub fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Hutan".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(13.0), px(11.0))),
        }),
        window_background: WindowBackgroundAppearance::Blurred,
        window_min_size: Some(size(px(640.0), px(400.0))),
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// Open a workbench window and focus the app.
pub fn open_workbench(
    cx: &mut App,
    options: WorkbenchOptions,
) -> gpui::Result<WindowHandle<Workbench>> {
    let window_options = window_options(cx);
    let handle = cx.open_window(window_options, move |window, cx| {
        cx.new(|cx| {
            let workbench = Workbench::new(options, window, cx);
            window.on_next_frame(|_, _| hutan_terminal_view::trace::startup("first_frame"));
            crate::bench::start(window, cx);
            workbench
        })
    })?;
    hutan_terminal_view::trace::startup("window_opened");
    cx.activate(true);
    Ok(handle)
}

/// `hutand` next to the running binary (`target/debug`, an app bundle), else the name for a
/// `PATH` lookup.
pub fn hutand_path() -> PathBuf {
    hutand_beside(std::env::current_exe().ok().as_deref())
}

fn hutand_beside(exe: Option<&Path>) -> PathBuf {
    exe.and_then(Path::parent)
        .map(|dir| dir.join("hutand"))
        .filter(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("hutand"))
}

/// Connection options of the desktop: the standard socket (`HUTAN_SOCKET` / `HUTAN_HOME`
/// respected) with `hutand` autostarted when nothing listens.
pub fn connect_options() -> ConnectOptions {
    ConnectOptions {
        socket: None,
        autostart: Some(hutand_path()),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    }
}

/// Starts the connection. With keep running on and approved, launchd starts the daemon, so the
/// connection only dials ([`Connection::start_held`]) and the workbench lets it start `hutand`
/// itself if none has answered after a grace period; otherwise it autostarts as always.
/// The second value says which of the two it was.
pub fn start_connection(
    options: ConnectOptions,
    settings: &SettingsStore,
    login_item: &dyn LoginItem,
) -> (Connection, bool) {
    crate::settings::register_all(settings);
    let keep_running = settings.get::<AppSettings>().keep_running
        && login_item.available()
        && login_item.status() == LoginItemStatus::Enabled;
    if keep_running {
        (Connection::start_held(options), true)
    } else {
        (Connection::start(options), false)
    }
}

/// The settings store over `settings.toml` / `keymap.toml` in the Hutan home.
pub fn settings_store() -> SettingsStore {
    let paths = SettingsPaths::from_env().unwrap_or_else(|err| {
        tracing::warn!("{err}; settings are kept in the temp directory");
        SettingsPaths::in_dir(std::env::temp_dir().join("hutan"))
    });
    SettingsStore::new(paths)
}

/// Binary entry point: `HUTAN_HOME=/tmp/x hutan-desktop`.
pub fn run() {
    hutan_terminal_view::trace::startup("main");
    if let Some(path) = crate::crash::default_log_path() {
        crate::crash::install(path);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    gpui_platform::application().run(|cx: &mut App| {
        let settings = settings_store();
        let mut mac = MacServices::system();
        let (connection, launchd) =
            start_connection(connect_options(), &settings, mac.login_item.as_ref());
        mac.launchd_daemon = launchd;
        let options =
            WorkbenchOptions::builtin(settings, connection, persistence::default_state_path())
                .with_mac(mac);
        init(cx, &LayoutPreset::builtin());
        watch_signals(cx);
        if let Err(err) = open_workbench(cx, options) {
            tracing::error!("cannot open the workbench window: {err:#}");
            cx.quit();
        }
    });
}

/// SIGTERM / SIGINT / SIGHUP quit like `app.on_quit = keep_running`: layout saved, connection
/// closed, exit code 0, the daemon untouched; never a question.
fn watch_signals(cx: &mut App) {
    match crate::signals::watch() {
        Ok(mut signals) => cx
            .spawn(async move |cx| {
                while let Some(signal) = signals.recv().await {
                    tracing::info!(
                        ?signal,
                        "terminated by a signal; leaving the daemon running"
                    );
                    cx.update(|cx| {
                        with_workbench(cx, |workbench, _, cx| workbench.quit_leaving_daemon(cx));
                    });
                }
            })
            .detach(),
        Err(err) => tracing::warn!("cannot watch for termination signals: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hutand_is_found_beside_the_binary_else_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("hutan-desktop");
        assert_eq!(hutand_beside(Some(&exe)), PathBuf::from("hutand"));
        std::fs::write(dir.path().join("hutand"), "").unwrap();
        assert_eq!(hutand_beside(Some(&exe)), dir.path().join("hutand"));
        assert_eq!(hutand_beside(None), PathBuf::from("hutand"));
    }

    #[test]
    fn the_desktop_of_an_app_bundle_starts_the_bundled_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let macos = dir.path().join("Hutan.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        for name in ["hutan-desktop", "hutand", "hutan"] {
            std::fs::write(macos.join(name), "").unwrap();
        }
        assert_eq!(
            hutand_beside(Some(&macos.join("hutan-desktop"))),
            macos.join("hutand")
        );
    }
}
