//! Shared harness for the session and attach tests: the real `heeler` binary against a real
//! `heelerd` (in-process on a temporary `HEELER_HOME`, or a spawned process).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns
#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use heeler_daemon::{DaemonConfig, Paths, RunningDaemon};
use serde_json::Value;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

pub const HEELER: &str = env!("CARGO_BIN_EXE_heeler");
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
        // Sessions find this build of the CLI through `HEELER_BIN`, as in a real install.
        let mut config = DaemonConfig {
            heeler_bin: Some(PathBuf::from(HEELER)),
            ..DaemonConfig::new(paths.clone())
        };
        tweak(&mut config);
        let daemon = heeler_daemon::start(config).await.unwrap();
        Self {
            _dir: dir,
            paths,
            daemon,
        }
    }

    pub async fn heeler(&self, args: &[&str]) -> Out {
        heeler(&self.paths.socket, args).await
    }

    /// `heeler new --json` of `argv`; returns the session id.
    pub async fn new_session(&self, extra: &[&str], argv: &[&str]) -> String {
        let mut args = vec!["new", "--json"];
        args.extend_from_slice(extra);
        args.push("--");
        args.extend_from_slice(argv);
        let out = self.heeler(&args).await.ok();
        out.json()["session"]["id"].as_str().unwrap().to_owned()
    }

    /// The session objects of `heeler ls --json`.
    pub async fn sessions(&self) -> Vec<Value> {
        let out = self.heeler(&["ls", "--json"]).await.ok();
        out.json()["sessions"].as_array().unwrap().clone()
    }

    pub async fn session(&self, id: &str) -> Value {
        self.sessions()
            .await
            .into_iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("session {id} not listed"))
    }

    /// Polls `heeler ls` until the session's `state.state` is `state`.
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

/// Environment-free invocation: `HEELER_*` from the parent never leak in. `--socket` goes
/// first so it cannot end up inside a trailing `-- ARGV`.
pub fn command(binary: &Path, socket: Option<&Path>, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    command
        .args(args)
        .env_remove("HEELER_HOME")
        .env_remove("HEELER_SOCKET")
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

pub async fn heeler(socket: &Path, args: &[&str]) -> Out {
    finish(command(Path::new(HEELER), Some(socket), args)).await
}

/// Directory holding `heelerd`, next to the `heeler` test binary (same profile). Tests never
/// build it: `just test` builds the workspace binaries first.
pub fn heelerd_dir() -> PathBuf {
    let dir = Path::new(HEELER).parent().unwrap().to_owned();
    assert!(
        dir.join("heelerd").is_file(),
        "heelerd not built: run `cargo build -p heeler-daemon --bin heelerd` (or `just test`)"
    );
    dir
}

/// The fake codex app server, next to the `heeler` test binary; `just test` builds the
/// workspace binaries first.
pub fn codex_fixture_bin() -> PathBuf {
    let bin = Path::new(HEELER)
        .parent()
        .unwrap()
        .join("heeler-codex-fixture");
    assert!(
        bin.is_file(),
        "heeler-codex-fixture not built: run `cargo build -p heeler-agents --bins` (or `just test`)"
    );
    bin
}
