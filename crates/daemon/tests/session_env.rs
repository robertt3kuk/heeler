//! The environment of a session (`daemon.toml [terminal] env_inherit`): a session running
//! `/usr/bin/env` against an in-process daemon whose own process carries a sentinel variable.
#![allow(unsafe_code)] // `set_var`, see below
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

mod common;

use std::collections::BTreeMap;

use common::*;
use hutan_protocol::messages::{Event, TerminalSpec};

const SENTINEL: &str = "HUTAN_B1_DAEMON_SENTINEL";

/// Runs `/usr/bin/env` as a session of a daemon configured with `daemon_toml` and returns what
/// it printed.
async fn session_env(daemon_toml: &str, client_env: &[(&str, &str)]) -> BTreeMap<String, String> {
    let t = TestDaemon::start_with(daemon_toml, |_| {}).await;
    let (client, mut events) = t.connect_subscribed().await;
    let session = create(
        &client,
        TerminalSpec {
            command: vec!["/usr/bin/env".to_owned()],
            env: client_env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            // Wide enough that no variable wraps.
            size: Some(size(4000, 200)),
            ..TerminalSpec::default()
        },
    )
    .await;
    wait_event(&mut events, "session.exited", |e| match e {
        Event::SessionExited { session_id, .. } if *session_id == session.id => Some(()),
        _ => None,
    })
    .await;
    // The exited session keeps its screen for late readers.
    let view = View::attach(&client, terminal_of(&session), None).await;
    let printed = view
        .screen
        .lines
        .iter()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.to_owned(), value.trim_end().to_owned()))
        })
        .collect();
    drop(view);
    drop(client);
    t.stop().await;
    printed
}

// One test: the sentinel is process-wide and `set_var` is unsafe in edition 2024; nextest runs
// each test in its own process, and nothing reads the environment concurrently before it is set.
#[tokio::test(flavor = "multi_thread")]
async fn sessions_get_a_login_environment_unless_the_daemon_environment_is_asked_for() {
    // SAFETY: see above; set before the first daemon starts.
    unsafe { std::env::set_var(SENTINEL, "from-the-daemon") };
    let client_env = [
        ("HUTAN_SESSION_ID", "forged"),
        ("HUTAN_ENV", "forged"),
        ("CLIENT_VARIABLE", "kept"),
        ("TERM", "dumb"),
    ];

    // The default is `login`.
    for toml in ["", "[terminal]\nenv_inherit = \"login\"\n"] {
        let env = session_env(toml, &client_env).await;
        assert!(!env.contains_key(SENTINEL), "leaked ({toml:?}): {env:?}");
        assert!(
            !env.contains_key("CARGO_MANIFEST_DIR"),
            "the daemon's environment leaked ({toml:?}): {env:?}"
        );
        for key in [
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "TMPDIR",
            "LANG",
            "PATH",
            "TERM",
            "COLORTERM",
            "TERM_PROGRAM",
            "HUTAN_SOCKET",
            "HUTAN_SESSION_ID",
            "HUTAN_ENV",
            "HUTAN_BIN",
            "HUTAN_WORKSPACE_ID",
            "CLIENT_VARIABLE",
        ] {
            assert!(env.contains_key(key), "{key} missing ({toml:?}): {env:?}");
        }
        assert_eq!(env["CLIENT_VARIABLE"], "kept");
        // Daemon-managed variables beat the client's; terminal variables beat the client's.
        assert_ne!(env["HUTAN_SESSION_ID"], "forged");
        assert_eq!(env["HUTAN_ENV"], "1");
        assert_eq!(env["TERM"], "xterm-256color");
        assert_eq!(env["TERM_PROGRAM"], "hutan");
    }

    let env = session_env("[terminal]\nenv_inherit = \"daemon\"\n", &client_env).await;
    assert_eq!(env[SENTINEL], "from-the-daemon", "{env:?}");
    assert!(env.contains_key("CARGO_MANIFEST_DIR"), "{env:?}");
    assert_ne!(env["HUTAN_SESSION_ID"], "forged");
    assert_eq!(env["CLIENT_VARIABLE"], "kept");
}
