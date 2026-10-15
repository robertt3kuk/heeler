#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The macOS integration (M6-C) against an in-process daemon, with fakes for every system API:
//! keep running (login item), the menu bar item, the Dock badge and the global shortcut. No test
//! touches `SMAppService`, `NSStatusBar`, the Dock tile or the hotkey API.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_options, wait_until};
use gpui::{Entity, OwnedMenuItem, TestAppContext, VisualTestContext};
use hutan_client::{ConnectOptions, Connection};
use hutan_core::SessionId;
use hutan_desktop::app::start_connection;
use hutan_desktop::macos::login_item::LoginItem as _;
use hutan_desktop::macos::{
    FakeDock, FakeHotkey, FakeLoginItem, FakeStatusItem, LoginItemCall, LoginItemStatus, MacEvent,
    MacServices, MenuCommand, MenuSpec, event_channel,
};
use hutan_desktop::sessions::AttentionKind;
use hutan_desktop::workbench::actions::{self, daemon as daemon_actions};
use hutan_desktop::workbench::model::PanelId;
use hutan_desktop::workbench::{Workbench, WorkbenchOptions};
use hutan_protocol::chat::{ChatSend, ChatSent};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, ClientInfo, ClientKind, Request,
};
use hutan_settings::{SettingsPaths, SettingsStore};

/// A session that fails (exit 3) once `go` exists.
fn failing_session(
    daemon: &TestDaemon,
    title: &str,
    go: &Path,
) -> hutan_protocol::messages::SessionInfo {
    daemon.sh_titled(
        title,
        &format!(
            "while [ ! -e {0} ]; do sleep 0.05; done; exit 3",
            go.display()
        ),
    )
}

/// Every fake, kept by the test while the workbench holds clones.
struct Fakes {
    login: FakeLoginItem,
    item: FakeStatusItem,
    dock: FakeDock,
    hotkey: FakeHotkey,
}

impl Fakes {
    fn new() -> Self {
        Self {
            login: FakeLoginItem::new(),
            item: FakeStatusItem::new(),
            dock: FakeDock::new(),
            hotkey: FakeHotkey::new(),
        }
    }

    fn services(&self) -> MacServices {
        MacServices {
            login_item: Rc::new(self.login.clone()),
            status_item: Rc::new(self.item.clone()),
            dock: Rc::new(self.dock.clone()),
            hotkey: Rc::new(self.hotkey.clone()),
            ..MacServices::inert()
        }
    }
}

fn open_with_services<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    mac: MacServices,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let options = WorkbenchOptions::builtin(settings, daemon.connection(), None).with_mac(mac);
    open_options(cx, options)
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    fakes: &Fakes,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with_services(cx, daemon, fakes.services())
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext, sessions: usize) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
}

fn write_settings(daemon: &TestDaemon, text: &str) {
    std::fs::write(daemon.home().join("settings.toml"), text).unwrap();
}

fn set(view: &Entity<Workbench>, cx: &mut VisualTestContext, key: &str, value: toml::Value) {
    view.read_with(cx, |wb, _| wb.settings_store().write_user(key, value))
        .unwrap();
    cx.run_until_parked();
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
}

fn clear_notice(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update(cx, |wb, cx| wb.clear_notice(cx));
}

fn keep_running_setting(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.keep_running_enabled())
}

fn dispatch(cx: &mut VisualTestContext, action: impl gpui::Action) {
    cx.dispatch_action(action);
    cx.run_until_parked();
}

/// Title, checked and enabled state of the `Keep Running` item of the `Daemon` menu.
fn keep_running_menu_item(cx: &mut VisualTestContext) -> (String, bool, bool) {
    let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
    let menu = menus.iter().find(|m| m.name.as_ref() == "Daemon").unwrap();
    menu.items
        .iter()
        .find_map(|item| match item {
            OwnedMenuItem::Action {
                name,
                checked,
                disabled,
                ..
            } if name.starts_with("Keep Running") => Some((name.to_string(), *checked, !disabled)),
            _ => None,
        })
        .expect("a Keep Running item in the Daemon menu")
}

fn login_items_menu_enabled(cx: &mut VisualTestContext) -> bool {
    let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
    let menu = menus.iter().find(|m| m.name.as_ref() == "Daemon").unwrap();
    menu.items
        .iter()
        .find_map(|item| match item {
            OwnedMenuItem::Action { name, disabled, .. } if name == "Open Login Items" => {
                Some(!disabled)
            }
            _ => None,
        })
        .expect("an Open Login Items item in the Daemon menu")
}

fn pump(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(Duration::from_millis(50));
        condition(cx)
    });
}

fn report_blocked(daemon: &TestDaemon, session: SessionId) {
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        session_id: session,
        source: "test".into(),
        agent: "omp".into(),
        state: Some(AgentState::Blocked),
        message: Some("needs a decision".into()),
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);
}

// ---- keep running ------------------------------------------------------------------------

#[gpui::test]
fn the_keep_running_toggle_registers_and_unregisters_the_launch_agent(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    // Off by default; nothing was registered by opening the window.
    assert!(!keep_running_setting(&view, cx));
    assert!(fakes.login.calls().is_empty());
    assert_eq!(
        keep_running_menu_item(cx),
        ("Keep Running".to_owned(), false, true)
    );

    // On (the Daemon menu's action): registered, enabled, shown as such.
    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert_eq!(fakes.login.calls(), [LoginItemCall::Register]);
    assert!(keep_running_setting(&view, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.keep_running_status()),
        LoginItemStatus::Enabled
    );
    assert_eq!(
        keep_running_menu_item(cx),
        ("Keep Running".to_owned(), true, true)
    );
    assert!(!login_items_menu_enabled(cx), "nothing to approve");
    assert!(
        std::fs::read_to_string(daemon.home().join("settings.toml"))
            .unwrap()
            .contains("keep_running = true"),
        "the setting is what is saved"
    );

    // Off: unregistered.
    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert_eq!(
        fakes.login.calls(),
        [LoginItemCall::Register, LoginItemCall::Unregister]
    );
    assert!(!keep_running_setting(&view, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.keep_running_status()),
        LoginItemStatus::NotRegistered
    );
}

#[gpui::test]
fn the_setting_in_the_file_and_the_settings_sheet_drive_the_same_registration(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    write_settings(&daemon, "[app]\nkeep_running = true\n");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert_eq!(
        fakes.login.calls(),
        [LoginItemCall::Register],
        "a window opened with the setting on registers the agent"
    );

    // Settings → General: the toggle of the sheet goes through the same path.
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("app"), window, cx)
    });
    view.update(cx, |wb, cx| wb.sheet_toggle("keep_running", cx));
    cx.run_until_parked();
    assert_eq!(
        fakes.login.calls(),
        [LoginItemCall::Register, LoginItemCall::Unregister]
    );
    assert!(!keep_running_setting(&view, cx));

    // The file edited by hand (hot reload) registers it again.
    set(&view, cx, "app.keep_running", toml::Value::Boolean(true));
    assert_eq!(fakes.login.calls().last(), Some(&LoginItemCall::Register));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.keep_running_status()),
        LoginItemStatus::Enabled
    );
}

#[gpui::test]
fn requires_approval_is_shown_and_opens_login_items(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    fakes
        .login
        .set_after_register(LoginItemStatus::RequiresApproval);
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.keep_running_status()),
        LoginItemStatus::RequiresApproval
    );
    assert!(keep_running_setting(&view, cx), "the toggle stays on");
    assert_eq!(
        keep_running_menu_item(cx).0,
        "Keep Running (requires approval)"
    );
    assert!(notice(&view, cx).is_some_and(|n| n.contains("Login Items")));
    assert!(login_items_menu_enabled(cx));
    // Registered but not allowed: the daemon is not launchd's yet.
    assert!(!view.read_with(cx, |wb, _| wb.keep_running_active()));

    dispatch(cx, daemon_actions::OpenLoginItems);
    assert_eq!(
        fakes.login.calls().last(),
        Some(&LoginItemCall::OpenSettings)
    );

    // The user allows it in System Settings; coming back to the window picks that up.
    fakes.login.set_status(LoginItemStatus::Enabled);
    view.update(cx, |wb, cx| wb.refresh_login_status(cx));
    assert!(view.read_with(cx, |wb, _| wb.keep_running_active()));
    assert_eq!(keep_running_menu_item(cx).0, "Keep Running");
    assert!(!login_items_menu_enabled(cx));
}

#[gpui::test]
fn a_registration_the_system_refuses_is_reported_and_the_setting_goes_back(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    fakes.login.fail_register("Operation not permitted");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert_eq!(fakes.login.calls(), [LoginItemCall::Register]);
    assert!(!keep_running_setting(&view, cx), "setting restored");
    assert!(notice(&view, cx).is_some_and(|n| n.contains("Operation not permitted")));

    // The system cannot find the plist in the bundle.
    fakes.login.set_after_register(LoginItemStatus::NotFound);
    fakes.login.fail_register("");
    clear_notice(&view, cx);
    let fakes2 = Fakes::new();
    fakes2.login.set_after_register(LoginItemStatus::NotFound);
    drop(view);
    let (view, cx) = open(cx, &daemon, &fakes2);
    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert!(!keep_running_setting(&view, cx));
    assert!(notice(&view, cx).is_some_and(|n| n.contains("missing from Hutan.app")));

    // Unregistering can fail too: the setting stays on, as the system still has it.
    let fakes3 = Fakes::new();
    fakes3.login.set_status(LoginItemStatus::Enabled);
    fakes3.login.fail_unregister("busy");
    write_settings(&daemon, "[app]\nkeep_running = true\n");
    drop(view);
    let (view, cx) = open(cx, &daemon, &fakes3);
    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert_eq!(fakes3.login.calls(), [LoginItemCall::Unregister]);
    assert!(keep_running_setting(&view, cx));
    assert!(notice(&view, cx).is_some_and(|n| n.contains("busy")));
}

#[gpui::test]
fn outside_the_bundle_the_toggle_is_disabled_and_registers_nothing(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_with_services(cx, &daemon, MacServices::inert());
    wait_synced(&view, cx, 0);

    assert!(!view.read_with(cx, |wb, _| wb.keep_running_available()));
    assert_eq!(
        keep_running_menu_item(cx),
        (
            "Keep Running (available in Hutan.app)".to_owned(),
            false,
            false
        )
    );
    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert!(!keep_running_setting(&view, cx), "nothing was written");
    assert!(notice(&view, cx).is_some_and(|n| n.contains("available in Hutan.app")));
    assert!(!daemon.home().join("settings.toml").exists());
    assert!(!view.read_with(cx, |wb, _| wb.keep_running_active()));
}

#[gpui::test]
fn quitting_with_keep_running_on_never_asks_and_never_stops_the_daemon(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let fakes = Fakes::new();
    write_settings(&daemon, "[app]\nkeep_running = true\n");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);
    assert!(view.read_with(cx, |wb, _| wb.keep_running_active()));

    dispatch(cx, actions::Quit);
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    assert!(
        view.read_with(cx, |wb, _| wb.modal().is_none()),
        "no quit dialog"
    );
    assert!(daemon.paths.socket.exists(), "the daemon was left running");
}

#[gpui::test]
fn quitting_with_keep_running_on_and_no_sessions_leaves_the_daemon_too(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    write_settings(&daemon, "[app]\nkeep_running = true\n");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    dispatch(cx, actions::Quit);
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    draw(cx);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        daemon.paths.socket.exists(),
        "without keep running an empty daemon stops with the app; with it, launchd owns it"
    );
}

#[gpui::test]
fn quitting_still_asks_when_keep_running_is_off_or_not_approved(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string())),
        Some("Quit Hutan?".to_owned()),
        "off: the usual question"
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));

    // On, but the user has not approved the login item (or switched it off again).
    fakes
        .login
        .set_after_register(LoginItemStatus::RequiresApproval);
    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert!(keep_running_setting(&view, cx));
    dispatch(cx, actions::Quit);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string())),
        Some("Quit Hutan?".to_owned()),
        "not approved: nothing launchd-owned to leave"
    );
    assert!(!view.read_with(cx, |wb, _| wb.quitting()));
}

fn connect_options(socket: &Path, autostart: Option<&Path>) -> ConnectOptions {
    ConnectOptions {
        socket: Some(socket.to_owned()),
        autostart: autostart.map(Path::to_owned),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "desktop-test".into(),
        },
    }
}

fn spawn_marker_script(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let marker = dir.join("spawned");
    let script = dir.join("fake-hutand");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, marker)
}

#[gpui::test]
fn the_desktop_waits_for_the_launchd_daemon_instead_of_spawning_one(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (script, marker) = spawn_marker_script(dir.path());
    let socket = dir.path().join("hutand.sock");
    let settings = |text: &str| {
        let home = tempfile::tempdir_in(dir.path()).unwrap();
        std::fs::write(home.path().join("settings.toml"), text).unwrap();
        (SettingsStore::new(SettingsPaths::in_dir(home.path())), home)
    };

    // Keep running on and approved: held, so the first dial cannot race launchd.
    let enabled = FakeLoginItem::new();
    enabled.set_status(LoginItemStatus::Enabled);
    let (store, _home) = settings("[app]\nkeep_running = true\n");
    let (held, launchd) =
        start_connection(connect_options(&socket, Some(&script)), &store, &enabled);
    assert!(launchd);
    assert!(!held.autostart());
    held.shutdown();

    // Every other combination autostarts as before.
    let (store, _home) = settings("[app]\nkeep_running = false\n");
    let (conn, launchd) = start_connection(
        connect_options(&dir.path().join("a.sock"), None),
        &store,
        &enabled,
    );
    assert!(!launchd);
    assert!(!conn.autostart(), "no binary to autostart anyway");
    conn.shutdown();
    let pending = FakeLoginItem::new();
    pending.set_status(LoginItemStatus::RequiresApproval);
    let (store, _home) = settings("[app]\nkeep_running = true\n");
    let (conn, launchd) = start_connection(
        connect_options(&dir.path().join("b.sock"), Some(&script)),
        &store,
        &pending,
    );
    assert!(
        !launchd,
        "registered but not approved: launchd does not run it"
    );
    assert!(conn.autostart());
    conn.shutdown();
    assert!(
        !marker.exists(),
        "nothing was spawned by the held connection"
    );
    let _ = cx;
}

#[gpui::test]
fn without_a_launchd_daemon_after_the_grace_the_desktop_starts_one(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (script, marker) = spawn_marker_script(dir.path());
    let socket = dir.path().join("hutand.sock");
    let connection = Connection::start_held(connect_options(&socket, Some(&script)));
    let fakes = Fakes::new();
    let settings = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
    let mut mac = fakes.services();
    mac.launchd_daemon = true;
    mac.launchd_grace = Duration::from_millis(400);
    let options = WorkbenchOptions::builtin(settings, connection.clone(), None).with_mac(mac);
    let (_view, cx) = open_options(cx, options);

    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !marker.exists(),
        "inside the grace period nothing is started"
    );
    assert!(!connection.autostart());

    pump(cx, "the grace period to end", |_| connection.autostart());
    wait_until(cx, "hutand to be started", |_| marker.exists());
    connection.shutdown();
}

#[gpui::test]
fn a_launchd_daemon_that_answers_in_time_is_left_alone(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let (script, marker) = spawn_marker_script(dir.path());
    // The daemon is already up (launchd started it): dialled, not spawned.
    let connection = Connection::start_held(connect_options(&daemon.paths.socket, Some(&script)));
    let fakes = Fakes::new();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let mut mac = fakes.services();
    mac.launchd_daemon = true;
    mac.launchd_grace = Duration::from_millis(300);
    let options = WorkbenchOptions::builtin(settings, connection.clone(), None).with_mac(mac);
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx, 0);
    for _ in 0..8 {
        cx.executor().advance_clock(Duration::from_millis(100));
        draw(cx);
    }
    assert!(!connection.autostart());
    assert!(!marker.exists());
    connection.shutdown();
}

// ---- menu bar item and Dock badge --------------------------------------------------------

#[gpui::test]
fn the_status_item_and_the_badge_follow_the_attention_model(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let blocked = daemon.sh_titled("agent", "sleep 600");
    let go = daemon.home().join("go");
    let failing = failing_session(&daemon, "build", &go);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 2);

    // Nothing needs the user yet: the item is on the menu bar without a count.
    let shown = fakes.item.shown().expect("menu_bar defaults to on");
    assert_eq!(shown.daemon, "hutand connected");
    assert!(shown.needs_you.is_empty());
    assert_eq!(shown.title(), "");
    assert_eq!(fakes.dock.badge(), 0);

    // An agent reports blocked: counted and listed, and its row focuses the session.
    report_blocked(&daemon, blocked.id);
    wait_until(cx, "the blocked agent to be counted", |_| {
        fakes.dock.badge() == 1
    });
    let shown = fakes.item.shown().unwrap();
    assert_eq!(shown.title(), "1");
    assert_eq!(shown.needs_you.len(), 1);
    assert_eq!(shown.needs_you[0].session, blocked.id);
    assert_eq!(shown.needs_you[0].kind, AttentionKind::Blocked);
    assert!(shown.menu().iter().any(|row| matches!(
        row,
        MenuSpec::Item { command: MenuCommand::FocusSession(id), title, .. }
            if *id == blocked.id && title.starts_with("agent — blocked")
    )));

    // A session fails while the user is elsewhere: it counts too.
    std::fs::write(&go, "").unwrap();
    wait_until(cx, "the failure to be counted", |_| fakes.dock.badge() == 2);
    let shown = fakes.item.shown().unwrap();
    assert_eq!(shown.title(), "2");
    let kinds: Vec<_> = shown
        .needs_you
        .iter()
        .map(|e| (e.session, e.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            (blocked.id, AttentionKind::Blocked),
            (failing.id, AttentionKind::Failed)
        ]
    );

    // Looking at the failed session clears it; the badge steps down.
    view.update_in(cx, |wb, window, cx| wb.open_session(failing.id, window, cx));
    wait_until(cx, "the failure to be read", |_| fakes.dock.badge() == 1);
    assert_eq!(fakes.item.shown().unwrap().title(), "1");

    // The agent is unblocked: nothing is left, and the badge is gone.
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        session_id: blocked.id,
        source: "test".into(),
        agent: "omp".into(),
        state: Some(AgentState::Working),
        message: None,
        seq: 2,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);
    wait_until(cx, "the badge to clear", |_| fakes.dock.badge() == 0);
    assert_eq!(fakes.item.shown().unwrap().title(), "");

    // Every step was one transition, never a repeated value.
    assert_eq!(fakes.dock.history(), [0, 1, 2, 1, 0]);
}

#[gpui::test]
fn a_pending_approval_is_counted_with_the_session_it_blocks(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let chat = daemon.create_chat(&env.cwd());
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);
    assert_eq!(fakes.dock.badge(), 0);

    daemon.call::<ChatSent>(Request::ChatSend(ChatSend {
        session_id: chat.id,
        text: "Run `touch hi.txt` in a shell, then reply with the word done.".to_owned(),
        attachments: Vec::new(),
    }));
    pump(cx, "the approval to be counted", |_| {
        fakes.dock.badge() == 1
    });
    let shown = fakes.item.shown().unwrap();
    assert_eq!(shown.needs_you.len(), 1);
    assert_eq!(shown.needs_you[0].session, chat.id);
    assert_eq!(shown.needs_you[0].kind, AttentionKind::Approval);
    assert_eq!(shown.title(), "1");
}

#[gpui::test]
fn the_status_item_names_the_daemon_state(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert_eq!(fakes.item.shown().unwrap().daemon, "hutand connected");

    // Stopped from the app: the item says so (and nothing is reconnecting behind it).
    dispatch(cx, daemon_actions::Stop);
    pump(cx, "the daemon to stop", |_| {
        fakes
            .item
            .shown()
            .is_some_and(|m| m.daemon == "hutand stopped")
    });
}

#[gpui::test]
fn the_menu_bar_item_comes_and_goes_with_its_setting(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert!(fakes.item.shown().is_some());
    assert_eq!(fakes.item.hides(), 0);

    set(&view, cx, "app.menu_bar", toml::Value::Boolean(false));
    assert!(
        fakes.item.shown().is_none(),
        "removed when the setting goes off"
    );
    assert_eq!(fakes.item.hides(), 1);

    // Unrelated changes do not bring it back or remove it twice.
    set(
        &view,
        cx,
        "app.on_quit",
        toml::Value::String("keep_running".into()),
    );
    assert!(fakes.item.shown().is_none());
    assert_eq!(fakes.item.hides(), 1);

    let shows = fakes.item.shows();
    set(&view, cx, "app.menu_bar", toml::Value::Boolean(true));
    assert!(fakes.item.shown().is_some(), "and back, without a restart");
    assert_eq!(fakes.item.shows(), shows + 1);
}

#[gpui::test]
fn the_dock_badge_follows_its_setting(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let blocked = daemon.sh("sleep 600");
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);
    report_blocked(&daemon, blocked.id);
    wait_until(cx, "the count", |_| fakes.dock.badge() == 1);

    set(&view, cx, "app.dock_badge", toml::Value::Boolean(false));
    assert_eq!(fakes.dock.badge(), 0, "off: the badge is cleared");
    // The menu bar item still counts: the two are separate settings.
    assert_eq!(fakes.item.shown().unwrap().title(), "1");

    set(&view, cx, "app.dock_badge", toml::Value::Boolean(true));
    assert_eq!(fakes.dock.badge(), 1);
    assert_eq!(fakes.dock.history(), [0, 1, 0, 1]);
}

#[gpui::test]
fn menu_bar_choices_drive_the_window(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let go = daemon.home().join("go");
    let failing = failing_session(&daemon, "build", &go);
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 1);
    std::fs::write(&go, "").unwrap();
    wait_until(cx, "the failure", |_| fakes.dock.badge() == 1);

    // "build — failed" in the menu: its tab opens and the failure counts as seen.
    tx.try_send(MacEvent::Menu(MenuCommand::FocusSession(failing.id)))
        .unwrap();
    wait_until(cx, "the session to open", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(failing.id).is_some())
    });
    wait_until(cx, "the badge to clear", |_| fakes.dock.badge() == 0);

    // New Terminal.
    tx.try_send(MacEvent::Menu(MenuCommand::NewTerminal))
        .unwrap();
    wait_synced(&view, cx, 2);

    // Keep Running (checkbox).
    tx.try_send(MacEvent::Menu(MenuCommand::ToggleKeepRunning))
        .unwrap();
    wait_until(cx, "the registration", |_| {
        fakes.login.calls() == [LoginItemCall::Register]
    });

    // A session that is gone is said so.
    tx.try_send(MacEvent::Menu(MenuCommand::FocusSession(SessionId::new())))
        .unwrap();
    wait_until(cx, "the notice", |cx| {
        notice(&view, cx).is_some_and(|n| n.contains("no longer exists"))
    });

    // Quit Hutan runs the usual flow (keep running is on: nothing to ask, daemon stays).
    tx.try_send(MacEvent::Menu(MenuCommand::Quit)).unwrap();
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    assert!(daemon.paths.socket.exists());
}

// ---- global shortcut ---------------------------------------------------------------------

#[gpui::test]
fn the_default_shortcut_is_registered_and_opens_the_assistant(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 0);
    assert_eq!(fakes.hotkey.active().as_deref(), Some("⌥Space"));
    assert_eq!(fakes.hotkey.attempts(), ["⌥Space"]);
    assert_eq!(notice(&view, cx), None);

    // Pressed while another app is in front: the window comes forward with the Assistant.
    assert_ne!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some(PanelId::new("assistant"))
    );
    tx.try_send(MacEvent::Hotkey).unwrap();
    wait_until(cx, "the Assistant to open", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().docks.right.active == Some(PanelId::new("assistant"))
        })
    });
}

#[gpui::test]
fn a_taken_shortcut_is_reported_once_and_the_setting_is_left_alone(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    fakes.hotkey.take("⌥Space");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    assert_eq!(fakes.hotkey.active(), None);
    let text = notice(&view, cx).expect("a notice");
    assert!(
        text.contains("⌥Space") && text.contains("another app"),
        "{text}"
    );
    assert!(
        !daemon.home().join("settings.toml").exists(),
        "the setting was not touched"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_store()
            .get::<hutan_desktop::settings::AssistantSettings>()
            .global_shortcut
            .clone()),
        "alt-space"
    );

    // Other settings reloading does not retry or repeat the notice.
    clear_notice(&view, cx);
    set(&view, cx, "app.menu_bar", toml::Value::Boolean(false));
    set(&view, cx, "app.menu_bar", toml::Value::Boolean(true));
    assert_eq!(notice(&view, cx), None);
    assert_eq!(fakes.hotkey.attempts(), ["⌥Space"]);

    // A different shortcut works; the other app letting go makes the original possible again
    // once the setting is touched.
    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String("cmd-alt-space".into()),
    );
    assert_eq!(fakes.hotkey.active().as_deref(), Some("⌥⌘Space"));
    assert_eq!(notice(&view, cx), None);
    fakes.hotkey.release("⌥Space");
    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String("alt-space".into()),
    );
    assert_eq!(fakes.hotkey.active().as_deref(), Some("⌥Space"));
}

#[gpui::test]
fn an_empty_shortcut_turns_it_off_and_garbage_is_reported(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "[assistant]\nglobal_shortcut = \"\"\n");
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert!(
        fakes.hotkey.attempts().is_empty(),
        "empty: nothing is registered"
    );
    assert_eq!(notice(&view, cx), None);

    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String("ctrl-alt-k".into()),
    );
    assert_eq!(fakes.hotkey.active().as_deref(), Some("⌃⌥K"));

    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String(String::new()),
    );
    assert_eq!(fakes.hotkey.active(), None);
    assert_eq!(fakes.hotkey.unregistered(), 1);

    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String("space".into()),
    );
    assert_eq!(fakes.hotkey.active(), None);
    assert!(notice(&view, cx).is_some_and(|n| n.contains("not valid")));
}

// ---- schema ------------------------------------------------------------------------------

#[gpui::test]
fn the_new_namespaces_and_fields_are_in_the_settings_sheet(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("app"), window, cx)
    });
    let app = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.dotted())
            .collect::<Vec<_>>()
    });
    assert_eq!(app, ["on_quit", "keep_running", "menu_bar", "dock_badge"]);
    view.update(cx, |wb, cx| wb.select_settings_namespace("assistant", cx));
    let assistant = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.dotted())
            .collect::<Vec<_>>()
    });
    assert_eq!(assistant, ["global_shortcut"]);
    // Drawing the sheet with the status next to the toggle must not panic.
    view.update(cx, |wb, cx| wb.select_settings_namespace("app", cx));
    draw(cx);
}
