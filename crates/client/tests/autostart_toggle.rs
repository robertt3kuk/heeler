//! `Connection::set_autostart`: a daemon stopped while autostart is off stays down, and turning
//! autostart on starts it at once, inside the 10 s autostart interval.
//!
//! Own test binary: it sets `HEELER_HOME` for the daemon process it spawns.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// `set_var` is unsafe in edition 2024.
#![allow(unsafe_code)]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::*;
use heeler_client::{ConnectOptions, Connection, ConnectionState};
use heeler_daemon::{DaemonConfig, Paths};
use heeler_protocol::messages::{
    ClientInfo, ClientKind, DaemonShutdown, DaemonStatusInfo, Empty, Request,
};
use tokio::time::{sleep, timeout};

/// `heelerd` next to the test binaries (same profile). Tests never build it: `just test`
/// builds the workspace binaries first.
fn heelerd() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(|deps| deps.parent()).unwrap();
    let binary = profile_dir.join("heelerd");
    assert!(
        binary.is_file(),
        "heelerd not built: run `cargo build -p heeler-daemon --bin heelerd` (or `just test`)"
    );
    binary
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_daemon_stays_down_until_autostart_is_turned_on_again() {
    let binary = heelerd();
    let home = tempfile::tempdir().unwrap();
    // SAFETY: this binary has a single test, so nothing else reads or writes the environment
    // while it is changed; the spawned daemon inherits it.
    unsafe { std::env::set_var("HEELER_HOME", home.path()) };
    let paths = Paths::in_home(home.path());
    // In this process, so the connection has not used its autostart interval yet.
    let first = heeler_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap();

    let connection = Connection::start(ConnectOptions {
        socket: Some(paths.socket.clone()),
        autostart: Some(binary),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "test".into(),
        },
    });
    assert!(connection.autostart());
    let mut state = connection.state();
    let first_epoch = wait_connected(&mut state).await.epoch;

    connection.set_autostart(false);
    assert!(!connection.autostart());
    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: true,
        }))
        .await
        .unwrap();
    timeout(WAIT, first.wait()).await.unwrap().unwrap();
    wait_reconnecting(&mut state).await;

    // With autostart on, the first reconnect attempt (100 ms) would have started a daemon.
    sleep(Duration::from_millis(1500)).await;
    assert!(
        !paths.socket.exists(),
        "heelerd was started although autostart is off"
    );
    assert!(matches!(
        state.borrow().connection,
        ConnectionState::Reconnecting { .. }
    ));

    // Turning it on does not wait for the backoff: the daemon is back well before 5 s.
    let asked = Instant::now();
    connection.set_autostart(true);
    let after = wait_connected(&mut state).await;
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "reconnected after {:?}",
        asked.elapsed()
    );
    assert_ne!(after.epoch, first_epoch, "a new daemon, not the old one");
    let status: DaemonStatusInfo = connection
        .call(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    assert_ne!(status.pid, std::process::id(), "spawned as its own process");

    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: true,
        }))
        .await
        .unwrap();
    connection.shutdown();
}
