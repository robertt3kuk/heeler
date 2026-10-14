#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests for the daemon's lifetime (design.md § Daemon lifecycle): the quit flow and
//! `app.on_quit`, the termination-signal path, the daemon controls (popover, palette, menu),
//! and the working-mode control. Daemons are in-process on a temporary `HEELER_HOME`, or a
//! real `heelerd` process where the test starts one again.

mod common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::{ProcessDaemon, TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, OwnedMenuItem, TestAppContext, VisualTestContext};
use heeler_client::{Connection, ConnectionState};
use heeler_core::SessionId;
use heeler_desktop::settings::OnQuit;
use heeler_desktop::workbench::actions::{self, daemon as daemon_actions, palette};
use heeler_desktop::workbench::model::DockPosition;
use heeler_desktop::workbench::{DaemonBusy, Region, Workbench};
use heeler_settings::{SettingsPaths, SettingsStore};
use heeler_terminal_view::ViewStatus;

fn settings_in(dir: &Path) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(dir))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(
        cx,
        settings_in(daemon.home()),
        daemon.connection(),
        state_path,
    )
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext, sessions: usize) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
}

fn connection_state(view: &Entity<Workbench>, cx: &VisualTestContext) -> ConnectionState {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection_state())
}

fn quitting(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.quitting())
}

fn modal_title(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string()))
}

fn status_line(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, cx| wb.daemon_status_line(cx))
}

fn dispatch(cx: &mut VisualTestContext, action: impl gpui::Action) {
    cx.dispatch_action(action);
    cx.run_until_parked();
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} was not drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn confirm(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    cx.run_until_parked();
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

/// Keeps pumping GPUI for `span` so anything that would still happen does.
fn pump_for(cx: &mut VisualTestContext, span: Duration) {
    let until = Instant::now() + span;
    while Instant::now() < until {
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_socket_gone(cx: &mut VisualTestContext, socket: &Path) {
    wait_until(cx, "the daemon to remove its socket", |_| !socket.exists());
}

fn open_session(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
}

fn terminal_status(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> Option<ViewStatus> {
    view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(id).map(|v| v.read(cx).status().clone())
    })
}

/// Name and checked state of the items in `Daemon ▸ Working Mode`, and the enabled state of
/// the `Daemon` menu's actions, as the menu bar holds them right now.
type NamedFlags = Vec<(String, bool)>;

fn daemon_menu(cx: &mut VisualTestContext) -> (NamedFlags, NamedFlags) {
    let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
    let menu = menus
        .iter()
        .find(|m| m.name.as_ref() == "Daemon")
        .expect("a Daemon menu");
    let mut modes = Vec::new();
    let mut actions = Vec::new();
    for item in &menu.items {
        match item {
            OwnedMenuItem::Action { name, disabled, .. } => {
                actions.push((name.clone(), !disabled));
            }
            OwnedMenuItem::Submenu(sub) if sub.name.as_ref() == "Working Mode" => {
                for item in &sub.items {
                    if let OwnedMenuItem::Action { name, checked, .. } = item {
                        modes.push((name.clone(), *checked));
                    }
                }
            }
            _ => {}
        }
    }
    (modes, actions)
}

// quit: no live sessions ------------------------------------------------------------------

#[gpui::test]
fn quitting_without_live_sessions_stops_the_daemon_and_saves_the_layout(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open(cx, &daemon, Some(state.clone()));
    wait_synced(&view, cx, 0);

    // A layout change whose debounced save has not run yet must still reach the disk.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.toggle_dock(DockPosition::Bottom);
        });
    });
    assert!(!state.exists());

    dispatch(cx, actions::Quit);
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert!(state.exists(), "the layout was flushed before quitting");
    assert_eq!(
        connection_state(&view, cx),
        ConnectionState::Stopped,
        "the connection was closed"
    );
}

#[gpui::test]
fn closing_the_last_window_runs_the_same_flow(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);

    // The window stays until the flow has decided; the flow then quits the app.
    assert!(!cx.simulate_close(), "the window is kept");
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
}

#[gpui::test]
fn closing_the_last_window_with_live_sessions_asks(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    assert!(!cx.simulate_close());
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Quit Heeler?"));
    assert!(!quitting(&view, cx));
    assert!(daemon.paths.socket.exists());
}

#[gpui::test]
fn quitting_while_disconnected_quits_at_once(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    daemon.stop();
    wait_until(cx, "the connection to drop", |cx| {
        connection_state(&view, cx) != ConnectionState::Connected
    });
    dispatch(cx, actions::Quit);
    assert!(quitting(&view, cx), "no question, no wait");
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
}

// quit: live sessions, app.on_quit ---------------------------------------------------------

#[gpui::test]
fn ask_keep_running_quits_and_leaves_the_sessions(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Quit Heeler?"));
    let checkbox = view.read_with(cx, |wb, _| {
        wb.modal()
            .and_then(|m| m.checkbox.as_ref().map(|c| c.checked))
    });
    assert_eq!(checkbox, Some(false), "Don't ask again starts unticked");
    assert!(!quitting(&view, cx), "nothing happens before the choice");

    confirm(&view, cx);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert_eq!(connection_state(&view, cx), ConnectionState::Stopped);
    assert!(daemon.paths.socket.exists(), "the daemon keeps running");
    assert!(daemon.session(session.id).state.is_live());
    assert_eq!(
        read(daemon.home(), "settings.toml"),
        "",
        "without the checkbox nothing is written"
    );
}

#[gpui::test]
fn ask_stop_sessions_and_daemon_stops_everything_then_quits(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    view.update_in(cx, |wb, window, cx| wb.choose_modal_alternative(window, cx));
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert_eq!(connection_state(&view, cx), ConnectionState::Stopped);
}

#[gpui::test]
fn ask_cancel_changes_nothing(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    // Even a ticked checkbox is not remembered on Cancel.
    view.update(cx, |wb, cx| wb.set_modal_checkbox(true, cx));
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    pump_for(cx, Duration::from_millis(300));
    assert!(!quitting(&view, cx));
    assert_eq!(connection_state(&view, cx), ConnectionState::Connected);
    assert!(daemon.paths.socket.exists());
    assert_eq!(read(daemon.home(), "settings.toml"), "");
}

#[gpui::test]
fn dont_ask_again_writes_the_choice_to_settings(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    view.update(cx, |wb, cx| wb.set_modal_checkbox(true, cx));
    confirm(&view, cx);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    let text = read(daemon.home(), "settings.toml");
    assert!(
        text.contains("[app]") && text.contains("on_quit = \"keep_running\""),
        "{text}"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.on_quit_mode()),
        OnQuit::KeepRunning
    );
}

#[gpui::test]
fn dont_ask_again_with_stop_remembers_stop_daemon(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    view.update(cx, |wb, cx| wb.set_modal_checkbox(true, cx));
    view.update_in(cx, |wb, window, cx| wb.choose_modal_alternative(window, cx));
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    let text = read(daemon.home(), "settings.toml");
    assert!(text.contains("on_quit = \"stop_daemon\""), "{text}");
}

#[gpui::test]
fn keep_running_quits_without_asking_and_leaves_the_daemon(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "settings.toml",
        "[app]\non_quit = \"keep_running\"\n",
    );
    let session = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()), "no dialog");
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert!(daemon.paths.socket.exists());
    assert!(daemon.session(session.id).state.is_live());
}

#[gpui::test]
fn stop_daemon_mode_stops_sessions_and_daemon_without_asking(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "settings.toml",
        "[app]\non_quit = \"stop_daemon\"\n",
    );
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()), "no dialog");
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
}

#[gpui::test]
fn a_termination_signal_saves_disconnects_and_never_stops_the_daemon(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    // Even stop_daemon mode: signals behave like keep_running.
    write(
        daemon.home(),
        "settings.toml",
        "[app]\non_quit = \"stop_daemon\"\n",
    );
    let session = daemon.sh("sleep 600");
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open(cx, &daemon, Some(state.clone()));
    wait_synced(&view, cx, 1);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.toggle_dock(DockPosition::Bottom);
        });
    });

    // What the SIGTERM / SIGINT / SIGHUP watcher calls (`app::watch_signals`).
    view.update(cx, |wb, cx| wb.quit_leaving_daemon(cx));
    cx.run_until_parked();
    assert!(quitting(&view, cx));
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()), "never asks");
    assert!(state.exists(), "layout saved");
    assert_eq!(connection_state(&view, cx), ConnectionState::Stopped);
    pump_for(cx, Duration::from_millis(300));
    assert!(daemon.paths.socket.exists(), "the daemon is untouched");
    assert!(daemon.session(session.id).state.is_live());
}

// daemon controls ---------------------------------------------------------------------------

#[gpui::test]
fn the_connection_status_item_opens_the_popover_with_the_daemons_status(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    click(&view, cx, Region::StatusItem("connection"));
    assert!(view.read_with(cx, |wb, _| wb.daemon_popover_open()));
    wait_until(cx, "daemon.status", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_status().is_some())
    });
    let status = view
        .read_with(cx, |wb, _| wb.daemon_status().cloned())
        .unwrap();
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.sessions, 1);
    let epoch = view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);
    assert_eq!(Some(status.epoch), epoch);
    draw(cx);
    for control in ["daemon-restart", "daemon-stop", "daemon-logs"] {
        assert!(
            view.read_with(cx, |wb, _| wb
                .region_bounds(Region::Popover(control))
                .is_some()),
            "{control} is offered while connected"
        );
    }
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Popover("daemon-start"))
            .is_none()),
        "Start only appears when the daemon is down"
    );

    // Escape closes it.
    dispatch(cx, palette::Dismiss);
    assert!(!view.read_with(cx, |wb, _| wb.daemon_popover_open()));
    // Clicking the item opens it again; clicking it while open closes it (the backdrop).
    click(&view, cx, Region::StatusItem("connection"));
    assert!(view.read_with(cx, |wb, _| wb.daemon_popover_open()));
    click(&view, cx, Region::StatusItem("connection"));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_popover_open()));
}

#[gpui::test]
fn open_logs_reveals_the_log_file_and_has_no_other_side_effects(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);
    let log = daemon.paths.log_file();
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    view.update(cx, |wb, _| {
        wb.set_daemon_log_path(log.clone());
        let revealed = Rc::clone(&revealed);
        wb.set_reveal_handler(move |path, _| revealed.borrow_mut().push(path.to_owned()));
    });

    // No log file yet: a notice, nothing revealed.
    dispatch(cx, daemon_actions::OpenLogs);
    assert!(revealed.borrow().is_empty());
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n.starts_with("No daemon log yet"))
    );

    write(log.parent().unwrap(), "heelerd.log", "started\n");
    dispatch(cx, daemon_actions::OpenLogs);
    pump_for(cx, Duration::from_millis(200));
    assert_eq!(*revealed.borrow(), std::slice::from_ref(&log));
    assert_eq!(read(log.parent().unwrap(), "heelerd.log"), "started\n");
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    assert!(!quitting(&view, cx));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    assert_eq!(view.read_with(cx, |wb, _| wb.daemon_busy()), None);
    assert_eq!(connection_state(&view, cx), ConnectionState::Connected);
    assert!(daemon.paths.socket.exists());
}

#[gpui::test]
fn the_palette_lists_every_daemon_command(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);

    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    view.update(cx, |wb, cx| wb.set_palette_query("Daemon:", cx));
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.palette_titles()
            .iter()
            .map(ToString::to_string)
            .collect()
    });
    for expected in [
        "Daemon: Status",
        "Daemon: Restart Daemon",
        "Daemon: Stop Daemon",
        "Daemon: Start Daemon",
        "Daemon: Open Logs",
        "Daemon: Working Mode — Ask on Quit",
        "Daemon: Working Mode — Keep Sessions Running After Quit",
        "Daemon: Working Mode — Stop Daemon With the App",
    ] {
        assert!(
            titles.iter().any(|t| t == expected),
            "{expected} in {titles:?}"
        );
    }
}

#[gpui::test]
fn stop_from_the_palette_confirms_with_live_sessions_and_keeps_the_app_open(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, session.id);
    wait_until(cx, "the terminal", |cx| {
        terminal_status(&view, cx, session.id) == Some(ViewStatus::Live)
    });

    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    view.update(cx, |wb, cx| wb.set_palette_query("Daemon: Stop Daemon", cx));
    cx.simulate_keystrokes("enter");
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Stop the daemon?"));
    assert!(daemon.paths.socket.exists(), "not before the confirmation");
    // Cancelling leaves everything as it was.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));

    dispatch(cx, daemon_actions::Stop);
    confirm(&view, cx);
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_busy().is_none())
    });
    assert!(!quitting(&view, cx), "the app stays open");
    assert!(view.read_with(cx, |wb, _| wb.daemon_stopped()));
    assert_eq!(status_line(&view, cx), "heelerd stopped");
    assert!(
        !view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection().autostart()),
        "autostart is off so the daemon is not brought back"
    );
    wait_until(cx, "the terminal to say so", |cx| {
        matches!(
            terminal_status(&view, cx, session.id),
            Some(ViewStatus::Ended(text)) if text.contains("daemon is stopped")
        )
    });
    // The menu offers Start, not Stop/Restart.
    let (_, actions) = daemon_menu(cx);
    let enabled = |name: &str| actions.iter().find(|(n, _)| n == name).map(|(_, on)| *on);
    assert_eq!(enabled("Start Daemon"), Some(true));
    assert_eq!(enabled("Stop Daemon"), Some(false));
    assert_eq!(enabled("Restart Daemon"), Some(false));
}

#[gpui::test]
fn stop_without_live_sessions_needs_no_confirmation(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);

    dispatch(cx, daemon_actions::Stop);
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_busy().is_none() && wb.daemon_stopped()
        })
    });
    // The popover shows the way back: Start (button and banner), no Restart/Stop.
    click(&view, cx, Region::StatusItem("connection"));
    draw(cx);
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::Popover("daemon-start")).is_some()
    }));
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::Popover("daemon-banner-start"))
            .is_some()
    }));
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::Popover("daemon-stop")).is_none()
    }));
}

#[gpui::test]
fn the_popover_controls_stop_restart_and_start_a_real_daemon(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    let first = process.create_session(&["sleep", "600"]);
    let settings = settings_in(&process.dir.path().join("config"));
    let connection = Connection::start(process.options());
    let (view, cx) = open_with(cx, settings, connection, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, first.id);
    wait_until(cx, "the terminal", |cx| {
        terminal_status(&view, cx, first.id) == Some(ViewStatus::Live)
    });
    let epoch =
        |cx: &VisualTestContext| view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);
    let epoch_before = epoch(cx);

    // Restart from the popover: confirms because a session is live.
    click(&view, cx, Region::StatusItem("connection"));
    wait_until(cx, "daemon.status", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_status().is_some())
    });
    let pid_before = view.read_with(cx, |wb, _| wb.daemon_status().unwrap().pid);
    click(&view, cx, Region::Popover("daemon-restart"));
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Restart the daemon?")
    );
    assert!(process.socket().exists(), "not before the confirmation");
    confirm(&view, cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.daemon_busy()),
        Some(DaemonBusy::Restarting)
    );
    wait_until(cx, "the new daemon", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() == ConnectionState::Connected
                && wb.daemon_status().is_some()
        })
    });
    assert_ne!(epoch(cx), epoch_before, "a new daemon epoch");
    assert_ne!(
        view.read_with(cx, |wb, _| wb.daemon_status().unwrap().pid),
        pid_before,
        "a new daemon process"
    );
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    wait_until(cx, "the old terminal to end", |cx| {
        matches!(
            terminal_status(&view, cx, first.id),
            Some(ViewStatus::Ended(text)) if text.contains("daemon restarted")
        )
    });
    assert!(view.read_with(cx, |wb, _| wb.modal().is_none()));

    // Stop with a live session in the new daemon: confirmed, then it stays down.
    let second = process.create_session(&["sleep", "600"]);
    // The restarted daemon restored the first session next to the new one.
    wait_synced(&view, cx, 2);
    open_session(&view, cx, second.id);
    wait_until(cx, "the second terminal", |cx| {
        terminal_status(&view, cx, second.id) == Some(ViewStatus::Live)
    });
    click(&view, cx, Region::Popover("daemon-stop"));
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Stop the daemon?"));
    confirm(&view, cx);
    wait_socket_gone(cx, &process.socket());
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_busy().is_none() && wb.daemon_stopped()
        })
    });
    assert_eq!(status_line(&view, cx), "heelerd stopped");
    // Autostart would bring a daemon back within a few hundred milliseconds.
    pump_for(cx, Duration::from_millis(1500));
    assert!(
        !process.socket().exists(),
        "the stopped daemon stayed stopped"
    );
    assert!(matches!(
        terminal_status(&view, cx, second.id),
        Some(ViewStatus::Ended(text)) if text.contains("daemon is stopped")
    ));

    // Start from the popover: a daemon comes back and the old terminal ends as gone.
    click(&view, cx, Region::Popover("daemon-start"));
    wait_until(cx, "the daemon to start again", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() == ConnectionState::Connected
        })
    });
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    assert_eq!(status_line(&view, cx), "heelerd connected");
    wait_until(cx, "the second terminal to end", |cx| {
        matches!(
            terminal_status(&view, cx, second.id),
            Some(ViewStatus::Ended(text)) if text.contains("daemon restarted")
        )
    });
    assert!(process.socket().exists());
}

#[gpui::test]
fn start_and_restart_from_the_palette_work_on_a_stopped_daemon(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    process.create_session(&["true"]);
    let connection = Connection::start(process.options());
    let (view, cx) = open_with(
        cx,
        settings_in(&process.dir.path().join("config")),
        connection,
        None,
    );
    wait_until(cx, "the connection", |cx| {
        connection_state(&view, cx) == ConnectionState::Connected
    });

    // Starting a running daemon says so and changes nothing.
    dispatch(cx, daemon_actions::Start);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .as_deref(),
        Some("The daemon is already running")
    );

    dispatch(cx, daemon_actions::Stop);
    wait_socket_gone(cx, &process.socket());
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_busy().is_none() && wb.daemon_stopped()
        })
    });

    // Restart on a stopped daemon starts it.
    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    view.update(cx, |wb, cx| {
        wb.set_palette_query("Daemon: Restart Daemon", cx)
    });
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the daemon to start", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() == ConnectionState::Connected
        })
    });
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    assert!(process.socket().exists());
}

// working mode ----------------------------------------------------------------------------------

#[gpui::test]
fn the_mode_control_writes_app_on_quit_and_follows_external_edits(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);
    let mode = |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.on_quit_mode());
    assert_eq!(mode(cx), OnQuit::Ask);
    let (modes, _) = daemon_menu(cx);
    assert_eq!(
        modes,
        [
            ("Ask on quit".to_owned(), true),
            ("Keep sessions running after quit".to_owned(), false),
            ("Stop daemon with the app".to_owned(), false),
        ]
    );

    // The segmented control in the popover writes settings.toml.
    click(&view, cx, Region::StatusItem("connection"));
    click(&view, cx, Region::Popover("keep_running"));
    assert_eq!(mode(cx), OnQuit::KeepRunning);
    let text = read(daemon.home(), "settings.toml");
    assert!(text.contains("on_quit = \"keep_running\""), "{text}");
    let (modes, _) = daemon_menu(cx);
    assert_eq!(
        modes.iter().find(|(_, on)| *on).map(|(n, _)| n.as_str()),
        Some("Keep sessions running after quit")
    );

    // So do the palette and menu actions.
    dispatch(cx, actions::SetOnQuit(OnQuit::StopDaemon));
    assert_eq!(mode(cx), OnQuit::StopDaemon);
    assert!(read(daemon.home(), "settings.toml").contains("on_quit = \"stop_daemon\""));

    // An external edit shows up (hot reload) in the setting, the menu and the popover.
    write(daemon.home(), "settings.toml", "[app]\non_quit = \"ask\"\n");
    // The setting store may see the edit a tick before the workbench rebuilds the menu bar.
    wait_until(cx, "the edit to reach the menu", |cx| {
        mode(cx) == OnQuit::Ask
            && daemon_menu(cx)
                .0
                .iter()
                .find(|(_, on)| *on)
                .is_some_and(|(n, _)| n == "Ask on quit")
    });
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Popover("ask"))
            .is_some()),
        "the popover is still open and shows the segments"
    );
}

#[gpui::test]
fn the_settings_sheet_lists_the_app_namespace(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);

    cx.dispatch_action(actions::OpenSettingsFor("app".into()));
    draw(cx);
    let fields: Vec<String> = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .map(|s| s.fields().iter().map(|f| f.dotted()).collect())
            .unwrap_or_default()
    });
    assert_eq!(fields, ["on_quit"]);
}

#[gpui::test]
fn the_daemon_menu_follows_the_connection(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);
    let (_, actions) = daemon_menu(cx);
    let names: Vec<&str> = actions.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "Status…",
            "Restart Daemon",
            "Stop Daemon",
            "Start Daemon",
            "Open Logs"
        ]
    );
    let enabled = |name: &str, actions: &[(String, bool)]| {
        actions.iter().find(|(n, _)| n == name).map(|(_, on)| *on)
    };
    assert_eq!(enabled("Stop Daemon", &actions), Some(true));
    assert_eq!(enabled("Start Daemon", &actions), Some(false));

    // A stop from elsewhere (the CLI) shows through the reconnect state.
    daemon.stop();
    wait_until(cx, "the connection to drop", |cx| {
        connection_state(&view, cx) != ConnectionState::Connected
    });
    assert!(status_line(&view, cx).starts_with("reconnecting"));
    let (_, actions) = daemon_menu(cx);
    assert_eq!(enabled("Stop Daemon", &actions), Some(false));
    assert_eq!(enabled("Start Daemon", &actions), Some(true));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
}
