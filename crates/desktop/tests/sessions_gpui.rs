#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests against an in-process daemon: live sessions, terminal items, panels, attention,
//! lifecycle commands, settings hot reload, keymap, settings UI, and connection status.

mod common;

use std::path::Path;

use common::{TestDaemon, draw, open_with, screen_text, wait_until};
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};
use heeler_client::ConnectionState;
use heeler_core::{SessionId, SessionState};
use heeler_desktop::theme::Density;
use heeler_desktop::workbench::actions;
use heeler_desktop::workbench::model::{ItemRef, LayoutPreset};
use heeler_desktop::workbench::panels::{InspectorPanel, OverviewPanel, SessionsPanel};
use heeler_desktop::workbench::{Workbench, connection_label};
use heeler_protocol::messages::{Request, SessionInfo};
use heeler_settings::{SettingsPaths, SettingsStore};
use heeler_terminal_view::ViewStatus;

fn settings_for(daemon: &TestDaemon) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(cx, settings_for(daemon), daemon.connection(), None)
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext, sessions: usize) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
}

fn open_session(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
}

fn screen_of(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
) -> Option<String> {
    view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(id)
            .map(|v| screen_text(&v.read(cx).model()))
    })
}

fn wait_screen(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId, needle: &str) {
    let mut last = String::new();
    let needle = needle.to_owned();
    wait_until(cx, &format!("`{needle}` on the screen"), |cx| {
        last = screen_of(view, cx, id).unwrap_or_default();
        last.contains(&needle)
    });
}

fn state_of(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
) -> heeler_desktop::workbench::model::WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn session_state(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
) -> Option<SessionState> {
    view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).session(id).map(|s| s.state.clone())
    })
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

#[gpui::test]
fn a_session_item_shows_the_daemons_snapshot_and_takes_focus(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo hello-from-heeler; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);

    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "hello-from-heeler");

    // The welcome placeholder gave way to the session; its tab shows the session title.
    let state = state_of(&view, cx);
    assert_eq!(
        state.active_pane().unwrap().items,
        vec![ItemRef::Terminal {
            session: info.id.to_string()
        }]
    );
    assert_eq!(
        cx.window_title().as_deref(),
        Some(format!("{} — Heeler", info.title).as_str())
    );
    // The terminal has keyboard focus and the store knows which session is looked at.
    let focused = view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(info.id).map(|v| v.focus_handle(cx))
    });
    cx.update(|window, _| assert!(focused.unwrap().is_focused(window)));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).focused()),
        Some(info.id)
    );

    // Typing reaches the PTY: the shell script's `sleep` ignores it, so use a fresh cat.
    let echo = daemon.create_session(&["cat"], None);
    wait_synced(&view, cx, 2);
    open_session(&view, cx, echo.id);
    wait_until(cx, "the cat session to attach", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(echo.id)
                .is_some_and(|v| v.read(cx).model().is_ready())
        })
    });
    cx.simulate_input("typed-by-test\n");
    wait_screen(&view, cx, echo.id, "typed-by-test");
}

#[gpui::test]
fn a_pane_tab_and_an_overview_card_share_one_attachment(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo shared-model; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "shared-model");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
        1
    );

    let tab = view.read_with(cx, |wb, _| wb.terminal_view_of(info.id).unwrap());
    // Overview: the board draws the same model at a reduced size.
    cx.simulate_keystrokes("cmd-shift-o");
    let overview = loop {
        draw(cx);
        if let Some(panel) = view.read_with(cx, |wb, _| wb.panel::<OverviewPanel>("overview")) {
            break panel;
        }
    };
    wait_until(cx, "the preview", |cx| {
        overview.read_with(cx, |p, _| p.preview_view(info.id).is_some())
    });
    let preview = overview.read_with(cx, |p, _| p.preview_view(info.id).cloned().unwrap());
    assert_eq!(
        preview.read_with(cx, |v, _| v.shared().clone()),
        tab.read_with(cx, |v, _| v.shared().clone()),
        "one model, two views"
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
        1,
        "still one attachment"
    );
    assert!(
        preview
            .read_with(cx, |v, _| screen_text(&v.model()))
            .contains("shared-model"),
        "the preview shows the live screen"
    );
    // The preview never takes over the terminal's size.
    assert_eq!(
        preview.read_with(cx, |v, _| v.mode()),
        heeler_terminal_view::ViewMode::Preview
    );

    // Clicking a card leaves the overview (restoring the layout) and focuses the session.
    cx.dispatch_action(actions::OpenSessionFromOverview(info.id.to_string()));
    let state = state_of(&view, cx);
    assert_eq!(state.preset, LayoutPreset::DEFAULT);
    assert!(state.find_panel(&"overview".into()).is_none());
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(info.id)
    );
}

#[gpui::test]
fn the_sessions_panel_lists_groups_and_filters(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = daemon.create_workspace("api", &[ws_dir.path()]);
    let make = |title: &str, ws: Option<heeler_core::WorkspaceId>| {
        let info: heeler_protocol::messages::SessionResult = daemon.call(Request::SessionCreate(
            heeler_protocol::messages::SessionCreate {
                workspace_id: ws,
                spec: heeler_protocol::messages::SessionSpec::Terminal(
                    heeler_protocol::messages::TerminalSpec {
                        command: vec!["sleep".into(), "600".into()],
                        title: Some(title.to_owned()),
                        ..Default::default()
                    },
                ),
            },
        ));
        info.session
    };
    let alpha = make("alpha shell", None);
    let beta = make("beta build", Some(ws.id));
    let gamma = make("gamma tests", Some(ws.id));
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 3);
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel is drawn in the left dock");

    let titles = |cx: &mut VisualTestContext| panel.read_with(cx, |p, cx| p.visible_titles(cx));
    // Grouped by workspace: the default workspace first, then `api`.
    assert_eq!(titles(cx), ["alpha shell", "beta build", "gamma tests"]);

    panel.update(cx, |p, cx| p.set_query("build", cx));
    assert_eq!(titles(cx), ["beta build"]);
    panel.update(cx, |p, cx| p.set_query("TESTS gamma", cx));
    assert_eq!(
        titles(cx),
        ["gamma tests"],
        "words match in any order, case-insensitively"
    );
    panel.update(cx, |p, cx| p.set_query("zzz", cx));
    assert!(titles(cx).is_empty());
    panel.update(cx, |p, cx| p.set_query("", cx));

    // Row clicks go through the action: the session's tab opens.
    cx.dispatch_action(actions::OpenSession(beta.id.to_string()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(beta.id)
    );
    // A second open focuses the existing tab instead of adding another.
    cx.dispatch_action(actions::OpenSession(gamma.id.to_string()));
    cx.dispatch_action(actions::OpenSession(beta.id.to_string()));
    let items = state_of(&view, cx).active_pane().unwrap().items.clone();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(beta.id)
    );

    // The context menu lists the documented entries.
    cx.dispatch_action(actions::ShowSessionMenu(actions::SessionMenuAt {
        session: alpha.id.to_string(),
        position: gpui::point(gpui::px(50.0), gpui::px(50.0)),
    }));
    let menu = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu is open");
    let labels: Vec<String> = menu.iter().map(|(l, _)| l.to_string()).collect();
    assert_eq!(
        labels,
        [
            "Open",
            "Open in Split Right",
            "Open in Split Down",
            "Rename…",
            "Restart",
            "Stop…",
            "Copy Session ID"
        ]
    );
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(1, window, cx));
    assert_eq!(state_of(&view, cx).panes().len(), 2, "open in split right");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(alpha.id)
    );
    view.update_in(cx, |wb, window, cx| {
        wb.copy_session_id(alpha.id, cx);
        let _ = window;
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some(alpha.id.to_string())
    );

    // Grouping by state and the row fields are settings.
    write(
        daemon.home(),
        "settings.toml",
        "[panels.sessions]\ngroup_by = \"none\"\nneeds_you = false\nrow = [\"title\"]\n",
    );
    wait_until(cx, "the panel settings", |cx| {
        view.read_with(cx, |wb, _| {
            wb.settings_store()
                .get::<heeler_desktop::settings::SessionsPanelSettings>()
                .row
                == ["title"]
        })
    });
    assert_eq!(titles(cx).len(), 3);
}

#[gpui::test]
fn stop_and_restart_from_the_ui(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo started; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "started");

    // ⌘⇧W asks first while the session runs.
    cx.simulate_keystrokes("cmd-shift-w");
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    assert!(session_state(&view, cx, info.id).unwrap().is_live());

    cx.simulate_keystrokes("cmd-shift-w enter");
    wait_until(cx, "the session to stop", |cx| {
        session_state(&view, cx, info.id) == Some(SessionState::Stopped)
    });
    // The view says so and offers Restart / Close.
    let status = view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(info.id)
            .map(|v| v.read(cx).status().clone())
    });
    assert!(
        matches!(&status, Some(ViewStatus::Ended(text)) if text.contains("stopped")),
        "{status:?}"
    );
    // Stopping something that already ended says so instead of asking.
    cx.simulate_keystrokes("cmd-shift-w");
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));

    // Restart: the same tab continues on the new terminal without re-opening.
    cx.dispatch_action(actions::RestartSessionById(info.id.to_string()));
    wait_until(cx, "the session to run again", |cx| {
        session_state(&view, cx, info.id).is_some_and(|s| s.is_live())
    });
    wait_until(cx, "the view to go live", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == ViewStatus::Live)
        })
    });
    wait_screen(&view, cx, info.id, "started");
    assert_eq!(
        state_of(&view, cx).active_pane().unwrap().items.len(),
        1,
        "still one tab"
    );

    // Rename through the prompt.
    cx.dispatch_action(actions::RenameSessionById(info.id.to_string()));
    view.update(cx, |wb, cx| wb.set_modal_input("renamed by test", cx));
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the new title", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(info.id)
                .is_some_and(|s| s.title == "renamed by test")
        })
    });
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.active_title(cx)),
        "renamed by test"
    );
}

#[gpui::test]
fn unread_attention_is_read_when_the_session_is_focused(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let go = daemon.home().join("go");
    // Already over before the desktop connects: not news.
    let old = daemon.sh_titled("old", "exit 1");
    let failing = daemon.sh_titled(
        "failing",
        &format!(
            "while [ ! -e {0} ]; do sleep 0.05; done; exit 3",
            go.display()
        ),
    );
    let finishing = daemon.sh_titled(
        "finishing",
        &format!(
            "while [ ! -e {0} ]; do sleep 0.05; done; exit 0",
            go.display()
        ),
    );
    let calm = daemon.sh_titled("calm", "sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 4);
    // Something else has focus (the welcome pane), so nothing is read.
    let needs = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).needs_you())
    };
    wait_until(cx, "the old session to be seen as ended", |cx| {
        session_state(&view, cx, old.id).is_some_and(|s| !s.is_live())
    });
    assert!(needs(cx).is_empty(), "replayed history is not unread");

    std::fs::write(&go, "").unwrap();
    wait_until(cx, "both sessions to end", |cx| needs(cx).len() == 2);
    let presentation = |cx: &mut VisualTestContext, id| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.presentation(store.session(id).unwrap())
        })
    };
    let failed = presentation(cx, failing.id);
    assert_eq!(
        (failed.label.as_str(), failed.detail.as_str()),
        ("failed", "exit 3")
    );
    assert!(failed.needs_you);
    assert_eq!(presentation(cx, finishing.id).label, "done");
    assert_eq!(presentation(cx, old.id).label, "exited");
    assert_eq!(presentation(cx, calm.id).label, "idle");

    // The sessions panel lists them under "Needs you" first.
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .unwrap();
    let titles = panel.read_with(cx, |p, cx| p.visible_titles(cx));
    let mut head: Vec<&str> = titles.iter().take(2).map(String::as_str).collect();
    head.sort_unstable();
    assert_eq!(head, ["failing", "finishing"], "{titles:?}");

    // ⌘⇧J jumps to the first one that needs you; looking at it reads it.
    cx.simulate_keystrokes("cmd-shift-j");
    let first = view
        .read_with(cx, |wb, _| wb.focused_session())
        .expect("a session is focused");
    assert!(first == failing.id || first == finishing.id);
    let remaining = needs(cx);
    assert_eq!(remaining.len(), 1, "the focused one is read");
    assert!(!remaining.contains(&first));
    cx.simulate_keystrokes("cmd-shift-j");
    let second = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_ne!(first, second);
    assert!(needs(cx).is_empty());
    cx.simulate_keystrokes("cmd-shift-j");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string)),
        Some("Nothing needs you".to_owned())
    );
}

#[gpui::test]
fn settings_files_hot_reload_density_and_terminal_font(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo fonts; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "fonts");
    assert_eq!(view.read_with(cx, |wb, _| wb.density()), Density::Compact);
    let cell_height = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .and_then(|v| v.read(cx).cell_size())
                .map(|c| f32::from(c.height))
        })
        .unwrap()
    };
    let before = cell_height(cx);
    assert!((before - 13.0 * 1.2).abs() < 0.1, "{before}");
    // Terminal text keeps off the pane edge: 6 px compact, 8 px comfortable.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().padding),
        6.0
    );
    let pane = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(heeler_desktop::workbench::Region::Pane(
                wb.state().active_pane_id(),
            ))
        })
        .unwrap();
    let grid_cols = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .and_then(|v| v.read(cx).grid_size())
        })
        .unwrap()
        .0
    };
    let cell_w = 13.0 * 0.6;
    let expected = ((f32::from(pane.size.width) - 12.0) / cell_w).floor() as u16;
    assert_eq!(
        grid_cols(cx),
        expected,
        "columns come from the padded content box"
    );

    write(
        daemon.home(),
        "settings.toml",
        "[appearance]\ndensity = \"comfortable\"\n\n[terminal]\nfont_size = 20\nline_height = 1.5\n",
    );
    wait_until(cx, "the density to change", |cx| {
        view.read_with(cx, |wb, _| wb.density()) == Density::Comfortable
    });
    wait_until(cx, "the terminal font", |cx| {
        (cell_height(cx) - 30.0).abs() < 0.1
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        20.0
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().padding),
        8.0
    );
    // Panel geometry follows the density tokens: the status bar is 26 px comfortable.
    draw(cx);
    let bar = view.read_with(cx, |wb, _| {
        wb.region_bounds(heeler_desktop::workbench::Region::StatusBar)
    });
    let height = f32::from(bar.unwrap().size.height);
    assert!((height - 26.0).abs() <= 1.0, "{height}");

    // A broken edit keeps the last good values and is reported; a fix recovers.
    write(
        daemon.home(),
        "settings.toml",
        "[terminal]\nfont_size = \"big\"\n",
    );
    wait_until(cx, "the error", |cx| {
        view.read_with(cx, |wb, _| !wb.settings_store().errors().is_empty())
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        20.0,
        "the last good value stays"
    );
    write(
        daemon.home(),
        "settings.toml",
        "[terminal]\nfont_size = 16\n",
    );
    wait_until(cx, "the fix", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 16.0
    });
    assert_eq!(view.read_with(cx, |wb, _| wb.density()), Density::Compact);
}

#[gpui::test]
fn changing_the_terminal_theme_updates_the_daemons_osc_11_answer(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let reply_len = "\x1b]11;rgb:1234/5678/9abc\x07".len();
    let script = format!(
        r"stty raw -echo; printf READY; dd bs=1 count=1 2>/dev/null; printf '\033]11;?\007'; dd bs=1 count={reply_len} 2>/dev/null | cat -v; printf '\nDONE\n'; sleep 600"
    );
    let info = daemon.sh(&script);
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "READY");

    write(
        daemon.home(),
        "settings.toml",
        "[terminal]\ntheme = \"heeler-light\"\n",
    );
    wait_until(cx, "the light theme", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().palette.background)
            == heeler_terminal_view::Rgb(0xff, 0xff, 0xff)
    });
    // `terminal.set_colors` travels through the handle; give the connection a moment.
    for _ in 0..30 {
        draw(cx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cx.simulate_input("g");
    wait_screen(&view, cx, info.id, "DONE");
    let screen = screen_of(&view, cx, info.id).unwrap();
    assert!(
        screen.contains("^[]11;rgb:ffff/ffff/ffff^G"),
        "the daemon answers with the theme the user sees:\n{screen}"
    );
}

#[gpui::test]
fn keymap_edits_rebind_live(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 0);
    let panes = |cx: &mut VisualTestContext| state_of(&view, cx).panes().len();
    // Splits start terminals asynchronously; let them land.
    let settle = |cx: &mut VisualTestContext| {
        for _ in 0..30 {
            draw(cx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "the first split", |cx| panes(cx) == 2);

    write(
        daemon.home(),
        "keymap.toml",
        "[[binding]]\nkeys = \"cmd-d\"\naction = \"none\"\n\n[[binding]]\nkeys = \"cmd-shift-d\"\naction = \"workbench::SplitRight\"\n",
    );
    wait_until(cx, "the keymap to reload", |cx| {
        cx.update(|window, _| {
            window
                .highest_precedence_binding_for_action(&actions::SplitRight)
                .is_some_and(|b| b.keystrokes()[0].to_string() == "⌘⇧D")
        })
    });
    cx.simulate_keystrokes("cmd-d");
    settle(cx);
    assert_eq!(panes(cx), 2, "⌘D is unbound now");
    cx.simulate_keystrokes("cmd-shift-d");
    wait_until(cx, "⌘⇧D splitting right", |cx| panes(cx) == 3);

    // Deleting the file restores the defaults.
    std::fs::remove_file(daemon.home().join("keymap.toml")).unwrap();
    wait_until(cx, "the default keymap", |cx| {
        cx.update(|window, _| {
            window
                .highest_precedence_binding_for_action(&actions::SplitRight)
                .is_some_and(|b| b.keystrokes()[0].to_string() == "⌘D")
        })
    });
}

#[gpui::test]
fn the_settings_sheet_writes_values_and_keeps_comments(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let file = daemon.home().join("settings.toml");
    write(
        daemon.home(),
        "settings.toml",
        "# my settings\n[terminal]\nfont_size = 13 # keep this\ncopy_on_select = false\n",
    );
    let (view, cx) = open(cx, &daemon);

    cx.simulate_keystrokes("cmd-,");
    let sheet = view.read_with(cx, |wb, _| {
        wb.settings_sheet().map(|s| s.namespace().to_owned())
    });
    assert_eq!(sheet.as_deref(), Some("appearance"));
    // Every panel's gear and the status theme item open their own namespace.
    cx.dispatch_action(actions::OpenSettingsFor("terminal".into()));
    draw(cx);
    let fields: Vec<String> = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.dotted())
            .collect()
    });
    for expected in [
        "font_family",
        "font_size",
        "theme",
        "cursor.style",
        "cursor.blink",
        "option_as_meta",
    ] {
        assert!(
            fields.iter().any(|f| f == expected),
            "{expected} in {fields:?}"
        );
    }

    // A number box: type through the keyboard like a user.
    view.update(cx, |wb, cx| wb.sheet_begin_edit("font_size", cx));
    cx.simulate_keystrokes("backspace backspace backspace backspace 1 7 enter");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("# my settings") && text.contains("# keep this"),
        "{text}"
    );
    assert!(text.contains("font_size = 17"), "{text}");
    wait_until(cx, "the new font size", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 17.0
    });

    // Invalid input stays in the box with an error.
    view.update(cx, |wb, cx| wb.sheet_begin_edit("font_size", cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("huge", cx));
    cx.simulate_keystrokes("enter");
    let error = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .edit()
            .and_then(|e| e.error.clone())
    });
    assert_eq!(error.as_deref(), Some("expected a number"));
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().edit().is_none()));

    // Toggle, dropdown, list editor, reset.
    view.update(cx, |wb, cx| wb.sheet_toggle("copy_on_select", cx));
    view.update(cx, |wb, cx| wb.sheet_choose("cursor.style", "bar", cx));
    view.update(cx, |wb, cx| wb.sheet_choose("cursor.style", "nonsense", cx));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("copy_on_select = true"), "{text}");
    assert!(
        text.contains("# my settings") && text.contains("# keep this"),
        "{text}"
    );
    let parsed: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(parsed["terminal"]["cursor"]["style"].as_str(), Some("bar"));
    view.update(cx, |wb, cx| wb.sheet_reset("font_size", cx));
    wait_until(cx, "the default font size", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 13.0
    });

    cx.simulate_keystrokes("escape");
    assert!(
        view.read_with(cx, |wb, _| wb.settings_sheet().is_none()),
        "Escape closes the sheet"
    );

    // Lists: add and remove items of `panels.sessions.row`.
    cx.dispatch_action(actions::OpenSettingsFor("panels.sessions".into()));
    view.update(cx, |wb, cx| wb.sheet_remove_list_item("row", 4, cx));
    view.update(cx, |wb, cx| wb.sheet_begin_edit("row", cx));
    cx.simulate_keystrokes("a g e enter");
    let row = view.read_with(cx, |wb, _| wb.sheet_value("row")).unwrap();
    assert_eq!(
        row,
        serde_json::json!(["state", "kind", "title", "placement", "age"])
    );

    // Order: move items up and down; the ends do nothing.
    view.update(cx, |wb, cx| wb.sheet_move_list_item("row", 2, -1, cx));
    let row =
        |cx: &mut VisualTestContext| view.read_with(cx, |wb, _| wb.sheet_value("row")).unwrap();
    assert_eq!(
        row(cx),
        serde_json::json!(["state", "title", "kind", "placement", "age"])
    );
    view.update(cx, |wb, cx| wb.sheet_move_list_item("row", 0, -1, cx));
    view.update(cx, |wb, cx| wb.sheet_move_list_item("row", 4, 1, cx));
    assert_eq!(
        row(cx),
        serde_json::json!(["state", "title", "kind", "placement", "age"])
    );
    view.update(cx, |wb, cx| wb.sheet_move_list_item("row", 0, 1, cx));
    assert_eq!(
        row(cx),
        serde_json::json!(["title", "state", "kind", "placement", "age"])
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("# my settings"), "{text}");

    // Errors from the files show up in the store the sheet reads.
    write(
        daemon.home(),
        "settings.toml",
        "[terminal]\nfont_size = \"big\"\n[nope]\nx = 1\n",
    );
    wait_until(cx, "diagnostics", |cx| {
        view.read_with(cx, |wb, _| wb.settings_store().errors().len() >= 2)
    });
}

#[gpui::test]
fn save_layout_as_preset_writes_settings_and_hot_reloads_presets(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    cx.dispatch_action(actions::ToggleRightDock);
    cx.dispatch_action(actions::SaveLayoutAsPreset);
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    view.update(cx, |wb, cx| wb.set_modal_input("My Layout", cx));
    cx.simulate_keystrokes("enter");

    let text = std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap();
    let parsed: toml::Table = toml::from_str(&text).unwrap();
    let preset = &parsed["layout"]["presets"]["My-Layout"];
    assert_eq!(preset["left"]["visible"].as_bool(), Some(true));
    assert_eq!(preset["right"]["visible"].as_bool(), Some(false));
    assert_eq!(preset["left"]["panels"].as_array().unwrap().len(), 1);
    assert_eq!(state_of(&view, cx).preset, "My-Layout");
    wait_until(cx, "the palette to list the preset", |cx| {
        view.read_with(cx, |wb, _| {
            wb.commands()
                .iter()
                .any(|c| c.title.as_ref() == "Layout: Apply Preset My-Layout")
        })
    });

    // Switch away and back: the preset restores the docks.
    cx.dispatch_action(actions::ApplyPreset("focus".into()));
    assert!(!state_of(&view, cx).docks.left.visible);
    cx.dispatch_action(actions::ApplyPreset("My-Layout".into()));
    let state = state_of(&view, cx);
    assert!(state.docks.left.visible && !state.docks.right.visible);

    // Editing a preset definition on disk re-applies it when it is the current one.
    write(
        daemon.home(),
        "settings.toml",
        "[layout.presets.My-Layout]\nleft = { panels = [\"sessions\"], size = 333, visible = true }\nright = { panels = [], size = 300, visible = false }\nbottom = { panels = [], size = 200, visible = false }\nstatus_bar = { left = [\"connection\"], right = [] }\n",
    );
    wait_until(cx, "the preset definition", |cx| {
        state_of(&view, cx).docks.left.size == 333.0
    });
    let bar = view.read_with(cx, |wb, _| wb.status_bar());
    assert_eq!(
        (bar.left, bar.right),
        (vec!["connection".to_owned()], Vec::<String>::new())
    );
}

#[gpui::test]
fn workspace_settings_apply_while_their_session_is_focused(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".heeler")).unwrap();
    std::fs::write(
        root.path().join(".heeler/settings.toml"),
        "[terminal]\nfont_size = 20\n",
    )
    .unwrap();
    let ws = daemon.create_workspace("proj", &[root.path()]);
    let plain = daemon.sh("sleep 600");
    let project = daemon.create_session(&["sleep", "600"], Some(ws.id));
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 2);

    open_session(&view, cx, plain.id);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.workspace_root().cloned()),
        None
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        13.0
    );

    open_session(&view, cx, project.id);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.workspace_root().cloned()),
        Some(root.path().to_path_buf())
    );
    wait_until(cx, "the workspace override", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 20.0
    });

    open_session(&view, cx, plain.id);
    wait_until(cx, "the override to end", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 13.0
    });
}

#[gpui::test]
fn new_terminal_starts_in_the_focused_workspace_root_or_home(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let root = tempfile::tempdir().unwrap();
    let ws = daemon.create_workspace("proj", &[root.path()]);
    let member = daemon.create_session(&["sleep", "600"], Some(ws.id));
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);

    // Nothing focused: the default workspace and $HOME.
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the new session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()).is_some()
    });
    let first = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    let info = daemon.session(first);
    assert_eq!(info.cwd, std::env::home_dir());
    assert_ne!(info.workspace_id, ws.id);

    // From a session of `proj`: same workspace, in its root.
    open_session(&view, cx, member.id);
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the second new session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|s| s != member.id && s != first)
    });
    let second = daemon.session(view.read_with(cx, |wb, _| wb.focused_session()).unwrap());
    assert_eq!(second.workspace_id, ws.id);
    assert_eq!(second.cwd.as_deref(), Some(root.path()));

    // New agent / chat stay disabled with hints.
    cx.simulate_keystrokes("cmd-shift-t");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string)),
        Some("Agents arrive with M4".to_owned())
    );
    cx.simulate_keystrokes("cmd-alt-t");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string)),
        Some("Chats arrive with M5".to_owned())
    );
}

#[gpui::test]
fn closing_a_tab_detaches_and_the_layout_reattaches_after_a_restart(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let a = daemon.sh("echo session-a; sleep 600");
    let b = daemon.sh("echo session-b; sleep 600");
    let state_path = daemon.home().join("desktop-state.json");
    {
        let (view, cx) = open_with(
            cx,
            settings_for(&daemon),
            daemon.connection(),
            Some(state_path.clone()),
        );
        wait_synced(&view, cx, 2);
        open_session(&view, cx, a.id);
        cx.dispatch_action(actions::OpenSessionInSplit(actions::SessionSplit {
            session: b.id.to_string(),
            edge: heeler_desktop::workbench::model::Edge::Right,
        }));
        wait_screen(&view, cx, a.id, "session-a");
        wait_screen(&view, cx, b.id, "session-b");
        assert_eq!(
            view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
            2
        );

        // ⌘W detaches: the session keeps running, the attachment ends.
        cx.simulate_keystrokes("cmd-w");
        wait_until(cx, "the tab to close", |cx| {
            state_of(&view, cx).panes().len() == 1
        });
        assert!(!view.read_with(cx, |wb, _| wb.holds(b.id)));
        assert!(session_state(&view, cx, b.id).unwrap().is_live());
        view.update(cx, |wb, _| wb.flush_save());
        // Put it back so the saved layout holds both.
        cx.dispatch_action(actions::OpenSessionInSplit(actions::SessionSplit {
            session: b.id.to_string(),
            edge: heeler_desktop::workbench::model::Edge::Bottom,
        }));
        view.update(cx, |wb, _| wb.flush_save());
    }
    // A new app instance restores the layout and re-attaches every open terminal item.
    let (view, cx) = open_with(
        cx,
        settings_for(&daemon),
        daemon.connection(),
        Some(state_path),
    );
    assert_eq!(state_of(&view, cx).panes().len(), 2);
    wait_screen(&view, cx, a.id, "session-a");
    wait_screen(&view, cx, b.id, "session-b");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
        2
    );
}

#[gpui::test]
fn items_of_vanished_sessions_do_not_attach(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let live = daemon.sh("sleep 600");
    let ghost = SessionId::new();
    let state_path = daemon.home().join("desktop-state.json");
    let mut state = heeler_desktop::workbench::model::WorkbenchState::default();
    let pane = state.active_pane_id();
    for id in [live.id, ghost] {
        state
            .add_item(
                pane,
                ItemRef::Terminal {
                    session: id.to_string(),
                },
            )
            .unwrap();
    }
    std::fs::write(&state_path, state.to_json().unwrap()).unwrap();

    let (view, cx) = open_with(
        cx,
        settings_for(&daemon),
        daemon.connection(),
        Some(state_path),
    );
    wait_synced(&view, cx, 1);
    wait_until(cx, "the live session to attach", |cx| {
        view.read_with(cx, |wb, _| wb.holds(live.id))
    });
    assert!(
        !view.read_with(cx, |wb, _| wb.holds(ghost)),
        "a missing session shows a placeholder"
    );
    // Closing the placeholder tab is an ordinary tab close.
    cx.dispatch_action(actions::ClosePaneItem);
    draw(cx);
}

#[gpui::test]
fn status_items_follow_the_connection_through_loss_and_recovery(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let info = daemon.sh("echo status; sleep 600");
    let connection = daemon.proxied_connection();
    let (view, cx) = open_with(cx, settings_for(&daemon), connection, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "status");
    let conn = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection_state())
    };
    assert_eq!(conn(cx), ConnectionState::Connected);
    assert_eq!(
        connection_label(ConnectionState::Connected).0,
        "heelerd connected"
    );
    let epoch = view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);

    // Connection loss: the link drops and reconnecting fails for a while.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    wait_until(cx, "reconnecting", |cx| {
        matches!(conn(cx), ConnectionState::Reconnecting { .. })
    });
    assert!(connection_label(conn(cx)).0.starts_with("reconnecting"));
    wait_until(cx, "the terminal to show it", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == ViewStatus::Reconnecting)
        })
    });

    // Recovery in the same epoch: the terminal re-attaches with a fresh snapshot.
    daemon.proxy().block(false);
    wait_until(cx, "the connection to recover", |cx| {
        conn(cx) == ConnectionState::Connected
    });
    wait_until(cx, "the terminal to go live again", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == ViewStatus::Live)
        })
    });
    wait_screen(&view, cx, info.id, "status");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch),
        epoch
    );

    // The daemon itself goes away and comes back with a new epoch: sessions are gone and the
    // open item says so.
    daemon.stop();
    wait_until(cx, "reconnecting after the daemon stopped", |cx| {
        matches!(conn(cx), ConnectionState::Reconnecting { .. })
    });
    daemon.restart();
    wait_until(cx, "the new daemon", |cx| {
        conn(cx) == ConnectionState::Connected
            && view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch) != epoch
    });
    wait_until(cx, "the item to end", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| matches!(v.read(cx).status(), ViewStatus::Ended(_)))
        })
    });
    assert!(view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().is_empty()));
}

#[gpui::test]
fn the_inspector_describes_the_focused_session(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo inspected; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "inspected");
    draw(cx);
    let inspector = view
        .read_with(cx, |wb, _| wb.panel::<InspectorPanel>("inspector"))
        .expect("the inspector is drawn in the right dock");
    let current: SessionInfo = view
        .read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(info.id).cloned()
        })
        .unwrap();
    let rows = inspector.read_with(cx, |p, cx| p.info_rows(&current, cx));
    let get = |label: &str| {
        rows.iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(get("ID"), info.id.to_string());
    assert_eq!(get("Kind"), "Terminal");
    assert_eq!(get("Command"), "sh -c echo inspected; sleep 600");
    assert_eq!(get("State"), "idle");
    assert_eq!(get("Terminal"), info.terminal_id.unwrap().to_string());
    assert!(get("Size").contains('×'), "{}", get("Size"));
    assert!(get("Created").ends_with("ago)"), "{}", get("Created"));
}

#[gpui::test]
fn terminal_notifications_flag_attention_but_replays_after_a_reconnect_do_not(
    cx: &mut TestAppContext,
) {
    let mut daemon = TestDaemon::start();
    let first = daemon.home().join("first");
    let second = daemon.home().join("second");
    let info = daemon.sh_titled(
        "notifier",
        &format!(
            "while [ ! -e {0} ]; do sleep 0.05; done; printf '\\033]9;while you were away\\007'; while [ ! -e {1} ]; do sleep 0.05; done; printf '\\033]9;built\\007'; sleep 600",
            first.display(),
            second.display()
        ),
    );
    let connection = daemon.proxied_connection();
    let (view, cx) = open_with(cx, settings_for(&daemon), connection, None);
    wait_synced(&view, cx, 1);
    let needs = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).needs_you())
    };
    assert!(needs(cx).is_empty());

    // The notification happens while the desktop is disconnected; the daemon replays it on
    // reconnect, which must not count as news.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    wait_until(cx, "the connection to drop", |cx| {
        view.read_with(cx, |wb, cx| {
            matches!(
                wb.sessions().read(cx).connection_state(),
                ConnectionState::Reconnecting { .. }
            )
        })
    });
    std::fs::write(&first, "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    daemon.proxy().block(false);
    wait_until(cx, "the connection to recover", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection_state())
            == ConnectionState::Connected
    });
    // Let the replay window pass, then the replayed notification must have left no trace.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(700);
    while std::time::Instant::now() < deadline {
        draw(cx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        needs(cx).is_empty(),
        "a replayed notification is not unread"
    );

    // A live notification of an unfocused session is.
    std::fs::write(&second, "").unwrap();
    wait_until(cx, "the live notification", |cx| needs(cx) == [info.id]);
    let presentation = view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        store.presentation(store.session(info.id).unwrap())
    });
    assert_eq!(
        (presentation.label.as_str(), presentation.detail.as_str()),
        ("done", "notification")
    );
    assert!(presentation.needs_you);

    // Focusing the session reads it; the process keeps running.
    open_session(&view, cx, info.id);
    assert!(needs(cx).is_empty());
    assert!(session_state(&view, cx, info.id).unwrap().is_live());
}

#[gpui::test]
fn the_overview_preset_shows_the_board_from_every_path(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo boards; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "boards");
    let terminal = ItemRef::Terminal {
        session: info.id.to_string(),
    };
    let board_shown = |cx: &mut VisualTestContext| {
        let s = state_of(&view, cx);
        s.preset == LayoutPreset::OVERVIEW
            && s.panes().len() == 1
            && s.active_pane().unwrap().active_item() == Some(&ItemRef::panel("overview"))
            && matches!(
                s.focus,
                heeler_desktop::workbench::model::FocusTarget::Pane(_)
            )
    };

    // Settings file (hot reload).
    write(
        daemon.home(),
        "settings.toml",
        "[layout]\npreset = \"overview\"\n",
    );
    wait_until(cx, "the overview from settings.toml", |cx| board_shown(cx));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.panel::<OverviewPanel>("overview"))
            .is_some()
    );
    // Leaving through the settings file restores the stashed layout exactly.
    write(
        daemon.home(),
        "settings.toml",
        "[layout]\npreset = \"default\"\n",
    );
    wait_until(cx, "the default layout from settings.toml", |cx| {
        state_of(&view, cx).preset == LayoutPreset::DEFAULT
    });
    let s = state_of(&view, cx);
    assert_eq!(s.active_pane().unwrap().active_item(), Some(&terminal));
    assert!(s.find_panel(&"overview".into()).is_none());

    // Palette / menu path, then ⌘⇧O and a card click leave again.
    cx.dispatch_action(actions::ApplyPreset("overview".into()));
    assert!(board_shown(cx));
    cx.simulate_keystrokes("cmd-shift-o");
    assert_eq!(
        state_of(&view, cx).active_pane().unwrap().active_item(),
        Some(&terminal)
    );
    cx.dispatch_action(actions::ApplyPreset("overview".into()));
    assert!(board_shown(cx));
    cx.dispatch_action(actions::ApplyPreset("default".into()));
    let s = state_of(&view, cx);
    assert_eq!(s.preset, LayoutPreset::DEFAULT);
    assert_eq!(
        s.active_pane().unwrap().active_item(),
        Some(&terminal),
        "stash restored"
    );
    cx.dispatch_action(actions::ApplyPreset("overview".into()));
    cx.dispatch_action(actions::OpenSessionFromOverview(info.id.to_string()));
    assert_eq!(state_of(&view, cx).preset, LayoutPreset::DEFAULT);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(info.id)
    );
}

#[gpui::test]
fn the_overview_preset_applies_at_startup_from_settings(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "settings.toml",
        "[layout]\npreset = \"overview\"\n",
    );
    let (view, cx) = open(cx, &daemon);
    let s = state_of(&view, cx);
    assert_eq!(s.preset, LayoutPreset::OVERVIEW);
    assert_eq!(
        s.active_pane().unwrap().active_item(),
        Some(&ItemRef::panel("overview"))
    );
}

#[gpui::test]
fn splits_start_a_terminal_in_the_focused_sessions_directory(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let created: heeler_protocol::messages::SessionResult = daemon.call(Request::SessionCreate(
        heeler_protocol::messages::SessionCreate {
            workspace_id: None,
            spec: heeler_protocol::messages::SessionSpec::Terminal(
                heeler_protocol::messages::TerminalSpec {
                    command: vec!["sleep".into(), "600".into()],
                    cwd: Some(dir.path().to_path_buf()),
                    ..Default::default()
                },
            ),
        },
    ));
    let origin = created.session;
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, origin.id);

    // ⌘D: a new pane with a NEW session, started where the focused one is.
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "the new session in the split", |cx| {
        state_of(&view, cx).panes().len() == 2
            && view
                .read_with(cx, |wb, _| wb.focused_session())
                .is_some_and(|s| s != origin.id)
    });
    let right = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_same_dir(daemon.session(right).cwd.as_deref(), dir.path());
    assert_eq!(state_of(&view, cx).active_pane().unwrap().items.len(), 1);

    // ⌘⇧D from that one splits again, from its directory.
    cx.simulate_keystrokes("cmd-shift-d");
    wait_until(cx, "the third pane", |cx| {
        state_of(&view, cx).panes().len() == 3
    });
    wait_until(cx, "its session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|s| s != origin.id && s != right)
    });
    let below = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_same_dir(daemon.session(below).cwd.as_deref(), dir.path());

    // From a panel (not a terminal): the workspace root, else $HOME.
    cx.dispatch_action(actions::OpenPanel("sessions".into()));
    cx.dispatch_action(actions::OpenActivePanelAsTab);
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "the split from a panel", |cx| {
        state_of(&view, cx).panes().len() == 4
    });
    wait_until(cx, "its session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|s| ![origin.id, right, below].contains(&s))
    });
    let fourth = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_eq!(daemon.session(fourth).cwd, std::env::home_dir());
}

#[gpui::test]
fn every_overview_card_shows_its_session_even_unfocused_or_ended(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let omp = daemon.sh_titled("omp", "echo omp-screen; sleep 600");
    let tests = daemon.sh_titled("tests", "echo tests-screen; sleep 600");
    let failing = daemon.sh_titled("failing", "echo boom; exit 1");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 3);
    open_session(&view, cx, omp.id);
    wait_screen(&view, cx, omp.id, "omp-screen");
    // Two more panes; the last one opened has focus, the others are unfocused.
    for (session, edge) in [
        (&tests, heeler_desktop::workbench::model::Edge::Right),
        (&failing, heeler_desktop::workbench::model::Edge::Bottom),
    ] {
        cx.dispatch_action(actions::OpenSessionInSplit(actions::SessionSplit {
            session: session.id.to_string(),
            edge,
        }));
    }
    wait_screen(&view, cx, omp.id, "omp-screen");
    wait_screen(&view, cx, tests.id, "tests-screen");
    wait_screen(&view, cx, failing.id, "boom");
    assert_eq!(state_of(&view, cx).panes().len(), 3);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(failing.id)
    );

    cx.dispatch_action(actions::ApplyPreset("overview".into()));
    let s = state_of(&view, cx);
    assert_eq!(s.panes().len(), 1);
    assert_eq!(
        s.active_pane().unwrap().items,
        vec![ItemRef::panel("overview")],
        "only the board is visible; the tabs wait in the stash"
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
        3,
        "hidden sessions stay attached"
    );

    let overview = loop {
        draw(cx);
        if let Some(panel) = view.read_with(cx, |wb, _| wb.panel::<OverviewPanel>("overview")) {
            break panel;
        }
    };
    for (id, text) in [
        (omp.id, "omp-screen"),
        (tests.id, "tests-screen"),
        (failing.id, "boom"),
    ] {
        wait_until(cx, &format!("the preview of {text}"), |cx| {
            overview.read_with(cx, |p, _| p.preview_view(id).is_some())
        });
        let preview = overview.read_with(cx, |p, _| p.preview_view(id).cloned().unwrap());
        wait_until(cx, &format!("`{text}` drawn in its card"), |cx| {
            preview
                .read_with(cx, |v, _| v.visible_text())
                .iter()
                .any(|row| row.contains(text))
        });
        assert!(
            preview
                .read_with(cx, |v, _| screen_text(&v.model()))
                .contains(text),
            "the shared model of {text} is intact"
        );
    }
    // Leaving the overview brings the split layout back with live terminals.
    cx.simulate_keystrokes("cmd-shift-o");
    assert_eq!(state_of(&view, cx).panes().len(), 3);
    wait_screen(&view, cx, omp.id, "omp-screen");
    wait_screen(&view, cx, failing.id, "boom");
}

/// The new pane inherits the session's cwd, which follows OSC 7 from the user's login shell;
/// shells like fish report the resolved path (`/private/var/…` for `/var/…` on macOS).
fn assert_same_dir(actual: Option<&std::path::Path>, expected: &std::path::Path) {
    let actual = actual.expect("session has a cwd");
    assert_eq!(
        std::fs::canonicalize(actual).unwrap(),
        std::fs::canonicalize(expected).unwrap(),
        "{} is not {}",
        actual.display(),
        expected.display()
    );
}
