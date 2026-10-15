#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The desktop's part of notification intents (protocol 0.13) against an in-process daemon: it
//! reports the session it shows (`client.focus`, cleared with the window's activation and
//! repeated after a reconnect), and it shows the session a notification's "Open" asks for
//! (`ui.open`). The daemon side is observable through `chat_turn_finished`, which a focused
//! session does not raise.

mod common;

use common::chat::ChatEnv;
use common::{TestDaemon, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use heeler_client::ConnectionState;
use heeler_core::SessionId;
use heeler_desktop::workbench::Workbench;
use heeler_protocol::chat::{ChatHistory, ChatHistoryRequest, ChatSend, ChatSent};
use heeler_protocol::messages::{Empty, Request};
use heeler_protocol::notify::{
    ACTION_OPEN, ActRoute, NotificationIntent, NotifyAct, NotifyActed, NotifyList,
};
use heeler_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    connection: heeler_client::Connection,
    sessions: usize,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        connection,
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
    (view, cx)
}

fn reported(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<Option<SessionId>> {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).focus_reported())
}

fn intents(daemon: &TestDaemon) -> Vec<NotificationIntent> {
    daemon
        .call::<NotifyList>(Request::NotifyList(Empty {}))
        .intents
}

fn send(daemon: &TestDaemon, session: SessionId) {
    daemon.call::<ChatSent>(Request::ChatSend(ChatSend {
        session_id: session,
        text: "Say hi".to_owned(),
        attachments: Vec::new(),
    }));
}

fn turn_ended(daemon: &TestDaemon, session: SessionId) -> bool {
    daemon
        .call::<ChatHistory>(Request::ChatHistory(ChatHistoryRequest {
            session_id: session,
            before_seq: None,
            limit: 100,
        }))
        .events
        .iter()
        .any(|event| event.kind.ends_turn())
}

/// Runs one turn in the session the desktop shows, then one in `other`; the intents the daemon
/// holds afterwards are only about `other`.
fn only_the_unwatched_turn_raises(
    daemon: &TestDaemon,
    cx: &mut VisualTestContext,
    watched: SessionId,
    other: SessionId,
) -> NotificationIntent {
    send(daemon, watched);
    wait_until(cx, "the watched turn to end", |_| {
        turn_ended(daemon, watched)
    });
    send(daemon, other);
    wait_until(cx, "the unwatched turn's intent", |_| {
        intents(daemon).iter().any(|i| i.session_id == other)
    });
    let pending = intents(daemon);
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].session_id, other);
    pending.into_iter().next().unwrap()
}

#[gpui::test]
fn the_desktop_reports_what_it_shows_and_opens_what_a_notification_asks_for(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let watched = daemon.create_chat(&env.cwd());
    let other = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon, daemon.connection(), 2);

    // Nothing is shown yet; the focus of the window is "no session".
    wait_until(cx, "the first focus report", |cx| {
        reported(&view, cx).is_some()
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(watched.id, window, cx));
    wait_until(cx, "the shown session to be reported", |cx| {
        reported(&view, cx) == Some(Some(watched.id))
    });

    // An inactive window shows nothing to the user; activating it again says so.
    cx.deactivate_window();
    wait_until(cx, "the deactivation to be reported", |cx| {
        reported(&view, cx) == Some(None)
    });
    cx.update(|window, _| window.activate_window());
    wait_until(cx, "the activation to be reported", |cx| {
        reported(&view, cx) == Some(Some(watched.id))
    });

    // The daemon raises no intent for the shown session, one for the other.
    let intent = only_the_unwatched_turn_raises(&daemon, cx, watched.id, other.id);

    // "Open" asks the desktop to show the other session.
    let acted: NotifyActed = daemon.call(Request::NotifyAct(NotifyAct {
        id: intent.id,
        action_id: ACTION_OPEN.to_owned(),
    }));
    assert_eq!(acted.route, ActRoute::UiOpen);
    assert_eq!(acted.desktop_clients, 1);
    wait_until(cx, "the session to be shown", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()) == Some(other.id)
    });
    wait_until(cx, "the new focus to be reported", |cx| {
        reported(&view, cx) == Some(Some(other.id))
    });

    // A session that has ended is not shown; the window says so and keeps what it had.
    let failing = daemon.sh("exit 2");
    wait_until(cx, "the failure intent", |_| {
        intents(&daemon).iter().any(|i| i.session_id == failing.id)
    });
    wait_until(cx, "the session to be seen as ended", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(failing.id)
                .is_some_and(|s| !s.state.is_live())
        })
    });
    let failure = intents(&daemon)
        .into_iter()
        .find(|i| i.session_id == failing.id)
        .unwrap();
    let acted: NotifyActed = daemon.call(Request::NotifyAct(NotifyAct {
        id: failure.id,
        action_id: ACTION_OPEN.to_owned(),
    }));
    assert_eq!(acted.desktop_clients, 1);
    wait_until(cx, "the notice", |cx| {
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            == Some("That session is no longer running".to_owned())
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(other.id)
    );
}

#[gpui::test]
fn the_focus_is_reported_again_on_the_connection_that_follows_a_loss(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let mut daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let watched = daemon.create_chat(&env.cwd());
    let other = daemon.create_chat(&env.cwd());
    let connection = daemon.proxied_connection();
    let (view, cx) = open(cx, &daemon, connection, 2);
    view.update_in(cx, |wb, window, cx| wb.open_session(watched.id, window, cx));
    wait_until(cx, "the shown session to be reported", |cx| {
        reported(&view, cx) == Some(Some(watched.id))
    });

    // The link drops; the daemon forgets the old connection's focus, and so does the store's
    // record of what was acknowledged.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    wait_until(cx, "the loss to be seen", |cx| {
        view.read_with(cx, |wb, cx| {
            matches!(
                wb.sessions().read(cx).connection_state(),
                ConnectionState::Reconnecting { .. }
            )
        }) && reported(&view, cx).is_none()
    });
    daemon.proxy().block(false);
    wait_until(cx, "the focus to be reported on the new connection", |cx| {
        reported(&view, cx) == Some(Some(watched.id))
    });

    // Had it not been, the finished turn of the shown session would raise an intent.
    only_the_unwatched_turn_raises(&daemon, cx, watched.id, other.id);
}
