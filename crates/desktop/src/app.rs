//! Application bootstrap: keymap, menus, window options, and the `run` entry point.

use crate::workbench::actions::{self, ApplyPreset};
use crate::workbench::model::LayoutPreset;
use crate::workbench::persistence;
use crate::workbench::{Workbench, WorkbenchOptions};
use gpui::{
    App, AppContext as _, Bounds, Menu, MenuItem, SystemMenuType, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use heeler_client::{ConnectOptions, Connection};
use heeler_protocol::messages::{ClientInfo, ClientKind};
use heeler_settings::{SettingsPaths, SettingsStore};
use std::path::{Path, PathBuf};

/// Install the global actions and the menu bar. Key bindings come from the workbench (defaults
/// plus `keymap.toml`), which rebinds them live.
pub fn init(cx: &mut App, presets: &[LayoutPreset]) {
    cx.on_action(|_: &actions::Quit, cx| cx.quit());
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    cx.set_menus(menus(presets));
}

/// App, View, and Window menus. Items dispatch the same named actions as the keymap and palette.
pub fn menus(presets: &[LayoutPreset]) -> Vec<Menu> {
    let preset_items = presets.iter().map(|p| {
        MenuItem::action(
            crate::workbench::preset_label(&p.name),
            ApplyPreset(p.name.clone()),
        )
    });
    vec![
        Menu::new("Heeler").items([
            MenuItem::action("About Heeler", actions::About),
            MenuItem::separator(),
            MenuItem::action("Settings…", actions::OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Heeler", actions::Quit),
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
            title: Some("Heeler".into()),
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
        cx.new(|cx| Workbench::new(options, window, cx))
    })?;
    cx.activate(true);
    Ok(handle)
}

/// `heelerd` next to the running binary (`target/debug`, an app bundle), else the name for a
/// `PATH` lookup.
pub fn heelerd_path() -> PathBuf {
    heelerd_beside(std::env::current_exe().ok().as_deref())
}

fn heelerd_beside(exe: Option<&Path>) -> PathBuf {
    exe.and_then(Path::parent)
        .map(|dir| dir.join("heelerd"))
        .filter(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("heelerd"))
}

/// Connection options of the desktop: the standard socket (`HEELER_SOCKET` / `HEELER_HOME`
/// respected) with `heelerd` autostarted when nothing listens.
pub fn connect_options() -> ConnectOptions {
    ConnectOptions {
        socket: None,
        autostart: Some(heelerd_path()),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    }
}

/// The settings store over `settings.toml` / `keymap.toml` in the Heeler home.
pub fn settings_store() -> SettingsStore {
    let paths = SettingsPaths::from_env().unwrap_or_else(|err| {
        tracing::warn!("{err}; settings are kept in the temp directory");
        SettingsPaths::in_dir(std::env::temp_dir().join("heeler"))
    });
    SettingsStore::new(paths)
}

/// Binary entry point: `HEELER_HOME=/tmp/x heeler-desktop`.
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    gpui_platform::application().run(|cx: &mut App| {
        let settings = settings_store();
        let connection = Connection::start(connect_options());
        let options =
            WorkbenchOptions::builtin(settings, connection, persistence::default_state_path());
        init(cx, &LayoutPreset::builtin());
        if let Err(err) = open_workbench(cx, options) {
            tracing::error!("cannot open the workbench window: {err:#}");
            cx.quit();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heelerd_is_found_beside_the_binary_else_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("heeler-desktop");
        assert_eq!(heelerd_beside(Some(&exe)), PathBuf::from("heelerd"));
        std::fs::write(dir.path().join("heelerd"), "").unwrap();
        assert_eq!(heelerd_beside(Some(&exe)), dir.path().join("heelerd"));
        assert_eq!(heelerd_beside(None), PathBuf::from("heelerd"));
    }
}
