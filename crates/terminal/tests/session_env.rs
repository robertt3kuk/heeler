//! The environment of a real child process in both `env_inherit` modes: `/usr/bin/env` run in a
//! PTY while the test process carries a sentinel variable.
#![allow(unsafe_code)] // `set_var`, see below
#![allow(clippy::expect_used)] // tests assert by panicking

use std::collections::BTreeMap;
use std::time::Duration;

use hutan_protocol::messages::TerminalSize;
use hutan_terminal::{EnvInherit, RenderTracker, Terminal, TerminalConfig};
use tokio::time::timeout;

const SENTINEL: &str = "HUTAN_B1_SENTINEL";

/// Runs `/usr/bin/env` with `client_env` and returns what it printed as a map.
async fn child_env(
    env_inherit: EnvInherit,
    client_env: &[(&str, &str)],
) -> BTreeMap<String, String> {
    let term = Terminal::spawn(TerminalConfig {
        argv: vec!["/usr/bin/env".to_owned()],
        env_inherit,
        env: client_env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        // Wide enough that no line wraps.
        size: TerminalSize {
            cols: 4000,
            rows: 200,
            pixel_width: 0,
            pixel_height: 0,
        },
        ..TerminalConfig::default()
    })
    .expect("spawn env");
    timeout(Duration::from_secs(15), term.wait_exit())
        .await
        .expect("env exits");
    let snapshot = term.snapshot(&mut RenderTracker::new());
    snapshot
        .lines
        .iter()
        .map(|row| {
            row.runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        })
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.to_owned(), value.trim_end().to_owned()))
        })
        .collect()
}

// One test: the sentinel is process-wide, and `set_var` is unsafe in edition 2024 (no other
// thread of this test binary reads the environment concurrently: nextest runs one test per
// process, and the PTY threads start only after it is set).
#[tokio::test]
async fn login_mode_hides_the_daemons_environment_and_daemon_mode_keeps_it() {
    // SAFETY: see above; set before anything else runs.
    unsafe { std::env::set_var(SENTINEL, "from-the-daemon") };

    let login = child_env(EnvInherit::Login, &[("EDITOR", "ed")]).await;
    assert!(!login.contains_key(SENTINEL), "leaked: {login:?}");
    for key in [
        "HOME", "USER", "LOGNAME", "SHELL", "PATH", "LANG", "TERM", "EDITOR",
    ] {
        assert!(login.contains_key(key), "{key} missing: {login:?}");
    }
    assert_eq!(login["HOME"], std::env::var("HOME").expect("HOME is set"));
    assert_eq!(login["EDITOR"], "ed");
    assert_eq!(login["TERM_PROGRAM"], "hutan");
    assert!(
        login["PATH"].contains("/usr/bin"),
        "path_helper PATH: {}",
        login["PATH"]
    );
    assert!(
        login["TMPDIR"].starts_with("/var/folders/"),
        "TMPDIR: {}",
        login["TMPDIR"]
    );
    // Only what the base, the client and the terminal contribute: nothing else of the test
    // process (cargo's variables, `PWD`, …) arrives.
    assert!(
        !login.contains_key("CARGO_MANIFEST_DIR") && !login.contains_key("PWD"),
        "{login:?}"
    );

    let daemon = child_env(EnvInherit::Daemon, &[]).await;
    assert_eq!(daemon[SENTINEL], "from-the-daemon");
    assert_eq!(daemon["TERM"], "xterm-256color");
}
