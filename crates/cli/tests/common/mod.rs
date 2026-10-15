//! Shared harness for the session and attach tests: the real `staffy` binary against a real
//! `staffyd` (in-process on a temporary `STAFFY_HOME`, or a spawned process).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns
#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use staffy_daemon::{DaemonConfig, Paths, RunningDaemon};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

pub const STAFFY: &str = env!("CARGO_BIN_EXE_staffy");
pub const WAIT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {:?}", self.stdout))
    }

    /// Asserts success and returns `self`.
    pub fn ok(self) -> Self {
        assert_eq!(self.code, 0, "{self:?}");
        self
    }
}

pub struct TestDaemon {
    pub _dir: TempDir,
    pub paths: Paths,
    pub daemon: RunningDaemon,
}

impl TestDaemon {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// [`start`](Self::start) with `tweak` applied to the daemon's config.
    pub async fn start_with(tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        // Sessions find this build of the CLI through `STAFFY_BIN`, as in a real install.
        let mut config = DaemonConfig {
            staffy_bin: Some(PathBuf::from(STAFFY)),
            ..DaemonConfig::new(paths.clone())
        };
        tweak(&mut config);
        let daemon = staffy_daemon::start(config).await.unwrap();
        Self {
            _dir: dir,
            paths,
            daemon,
        }
    }

    pub async fn staffy(&self, args: &[&str]) -> Out {
        staffy(&self.paths.socket, args).await
    }

    /// `staffy new --json` of `argv`; returns the session id.
    pub async fn new_session(&self, extra: &[&str], argv: &[&str]) -> String {
        let mut args = vec!["new", "--json"];
        args.extend_from_slice(extra);
        args.push("--");
        args.extend_from_slice(argv);
        let out = self.staffy(&args).await.ok();
        out.json()["session"]["id"].as_str().unwrap().to_owned()
    }

    /// The session objects of `staffy ls --json`.
    pub async fn sessions(&self) -> Vec<Value> {
        let out = self.staffy(&["ls", "--json"]).await.ok();
        out.json()["sessions"].as_array().unwrap().clone()
    }

    pub async fn session(&self, id: &str) -> Value {
        self.sessions()
            .await
            .into_iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("session {id} not listed"))
    }

    /// Polls `staffy ls` until the session's `state.state` is `state`.
    pub async fn wait_state(&self, id: &str, state: &str) -> Value {
        let result = timeout(WAIT, async {
            loop {
                let session = self.session(id).await;
                if session["state"]["state"] == state {
                    return session;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        result.unwrap_or_else(|_| panic!("session {id} never reached state {state}"))
    }

    pub async fn stop(self) {
        self.daemon.shutdown();
        timeout(WAIT, self.daemon.wait()).await.unwrap().unwrap();
    }
}

/// Environment-free invocation: `STAFFY_*` from the parent never leak in. `--socket` goes
/// first so it cannot end up inside a trailing `-- ARGV`.
pub fn command(binary: &Path, socket: Option<&Path>, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    command
        .args(args)
        .env_remove("STAFFY_HOME")
        .env_remove("STAFFY_SOCKET")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}

pub async fn finish(mut command: Command) -> Out {
    let output = timeout(WAIT, command.output()).await.unwrap().unwrap();
    Out {
        code: output.status.code().expect("killed by a signal"),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

pub async fn staffy(socket: &Path, args: &[&str]) -> Out {
    finish(command(Path::new(STAFFY), Some(socket), args)).await
}

/// Directory holding `staffyd`, next to the `staffy` test binary (same profile). Tests never
/// build it: `just test` builds the workspace binaries first.
pub fn staffyd_dir() -> PathBuf {
    let dir = Path::new(STAFFY).parent().unwrap().to_owned();
    assert!(
        dir.join("staffyd").is_file(),
        "staffyd not built: run `cargo build -p staffy-daemon --bin staffyd` (or `just test`)"
    );
    dir
}

/// The fake codex app server, next to the `staffy` test binary; `just test` builds the
/// workspace binaries first.
pub fn codex_fixture_bin() -> PathBuf {
    let bin = Path::new(STAFFY)
        .parent()
        .unwrap()
        .join("staffy-codex-fixture");
    assert!(
        bin.is_file(),
        "staffy-codex-fixture not built: run `cargo build -p staffy-agents --bins` (or `just test`)"
    );
    bin
}
