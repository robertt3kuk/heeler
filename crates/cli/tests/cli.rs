//! Runs the real `heeler` binary against a real `heelerd`: in-process on a temporary
//! `HEELER_HOME` for most commands, a spawned `heelerd` process for `daemon start`/`stop`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Once;
use std::time::Duration;

use heeler_daemon::{DaemonConfig, Paths, RunningDaemon};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

const HEELER: &str = env!("CARGO_BIN_EXE_heeler");
const WAIT: Duration = Duration::from_secs(20);

#[derive(Debug)]
struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {:?}", self.stdout))
    }
}

/// Environment-free invocation: `HEELER_*` from the parent never leak in.
fn command(binary: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_remove("HEELER_HOME")
        .env_remove("HEELER_SOCKET")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn finish(mut command: Command) -> Out {
    let output = timeout(WAIT, command.output()).await.unwrap().unwrap();
    Out {
        code: output.status.code().expect("killed by a signal"),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

async fn heeler(socket: &Path, args: &[&str]) -> Out {
    let mut cmd = command(Path::new(HEELER), args);
    cmd.arg("--socket").arg(socket);
    finish(cmd).await
}

struct TestDaemon {
    _dir: TempDir,
    paths: Paths,
    daemon: RunningDaemon,
}

impl TestDaemon {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        let daemon = heeler_daemon::start(DaemonConfig::new(paths.clone()))
            .await
            .unwrap();
        Self {
            _dir: dir,
            paths,
            daemon,
        }
    }

    async fn heeler(&self, args: &[&str]) -> Out {
        heeler(&self.paths.socket, args).await
    }

    async fn stop(self) {
        self.daemon.shutdown();
        self.daemon.wait().await.unwrap();
    }
}

fn missing_socket() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("nobody.sock");
    (dir, socket)
}

// daemon status ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_json_reports_the_running_daemon() {
    let t = TestDaemon::start().await;
    let out = t.heeler(&["daemon", "status", "--json"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    let status = out.json();
    assert_eq!(status["running"], true);
    assert_eq!(status["pid"], std::process::id());
    assert_eq!(status["epoch"], t.daemon.epoch().to_string());
    assert_eq!(status["socket"], t.paths.socket.to_str().unwrap());
    assert_eq!(status["sessions"], 0);
    assert_eq!(status["protocol"]["major"], 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_human_output() {
    let t = TestDaemon::start().await;
    let out = t.heeler(&["daemon", "status"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout.contains(&format!("pid {}", std::process::id())),
        "{out:?}"
    );
    assert!(out.stdout.contains("0 session(s)"), "{out:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn socket_is_found_through_heeler_home_and_heeler_socket() {
    let t = TestDaemon::start().await;

    let mut by_home = command(Path::new(HEELER), &["daemon", "status", "--json"]);
    by_home.env("HEELER_HOME", &t.paths.base);
    let out = finish(by_home).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.json()["pid"], std::process::id());

    let (_dir, nowhere) = missing_socket();
    let mut by_socket = command(Path::new(HEELER), &["daemon", "status", "--json"]);
    by_socket
        .env("HEELER_HOME", nowhere.parent().unwrap())
        .env("HEELER_SOCKET", &t.paths.socket);
    let out = finish(by_socket).await;
    assert_eq!(out.code, 0, "{out:?}");

    // --socket beats the environment.
    let mut flag = command(Path::new(HEELER), &["daemon", "status"]);
    flag.env("HEELER_SOCKET", &nowhere)
        .arg("--socket")
        .arg(&t.paths.socket);
    assert_eq!(finish(flag).await.code, 0);
    t.stop().await;
}

// workspaces -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ws_new_then_ls_json() {
    let t = TestDaemon::start().await;
    let created = t
        .heeler(&[
            "ws", "new", "alpha", "--root", "/srv/a", "--root", "rel/b", "--json",
        ])
        .await;
    assert_eq!(created.code, 0, "{created:?}");
    let created = created.json();
    assert_eq!(created["name"], "alpha");
    let id = created["id"].as_str().unwrap().to_owned();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        created["roots"],
        json!(["/srv/a", cwd.join("rel/b").to_str().unwrap()]),
        "relative roots are resolved against the caller's directory"
    );

    t.heeler(&["ws", "new", "beta"]).await;
    let list = t.heeler(&["ws", "ls", "--json"]).await;
    assert_eq!(list.code, 0, "{list:?}");
    let workspaces = list.json()["workspaces"].clone();
    let names: Vec<_> = workspaces
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap().to_owned())
        .collect();
    // The daemon always has its "default" workspace.
    assert_eq!(names, ["default", "alpha", "beta"]);
    assert_eq!(workspaces[1]["id"], id);

    let human = t.heeler(&["ws", "ls"]).await;
    assert!(human.stdout.contains(&id), "{human:?}");
    assert!(human.stdout.contains("alpha"), "{human:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_ls_on_a_fresh_daemon_shows_the_default_workspace() {
    let t = TestDaemon::start().await;
    let out = t.heeler(&["ws", "ls"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout.lines().count(), 2, "header and one row: {out:?}");
    assert!(out.stdout.lines().nth(1).unwrap().contains("default"));
    let out = t.heeler(&["ws", "ls", "--json"]).await.json();
    assert_eq!(out["workspaces"].as_array().unwrap().len(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_rename_and_close() {
    let t = TestDaemon::start().await;
    let created = t.heeler(&["ws", "new", "old", "--json"]).await.json();
    let id = created["id"].as_str().unwrap();

    let renamed = t.heeler(&["ws", "rename", id, "fresh", "--json"]).await;
    assert_eq!(renamed.code, 0, "{renamed:?}");
    assert_eq!(renamed.json()["name"], "fresh");
    let human = t.heeler(&["ws", "rename", id, "again"]).await;
    assert!(human.stdout.contains("again"), "{human:?}");

    let closed = t.heeler(&["ws", "close", id, "--json"]).await;
    assert_eq!(closed.code, 0, "{closed:?}");
    assert_eq!(closed.json(), json!({"workspace_id": id, "closed": true}));
    let list = t.heeler(&["ws", "ls", "--json"]).await.json();
    assert_eq!(list["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(list["workspaces"][0]["name"], "default");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_errors_exit_1_with_the_code_on_stderr() {
    let t = TestDaemon::start().await;
    let unknown = "01J00000000000000000000000";
    let out = t.heeler(&["ws", "close", unknown]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(out.stderr.starts_with("heeler: not_found:"), "{out:?}");
    let out = t.heeler(&["ws", "rename", unknown, "x"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    t.stop().await;
}

// api -----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn api_daemon_status_prints_the_result() {
    let t = TestDaemon::start().await;
    let out = t.heeler(&["api", "daemon.status"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    let result = out.json();
    assert_eq!(result["pid"], std::process::id());
    assert_eq!(result["epoch"], t.daemon.epoch().to_string());
    assert!(
        out.stdout.lines().count() > 1,
        "human output is pretty-printed"
    );

    let compact = t.heeler(&["api", "daemon.status", "--json"]).await;
    assert_eq!(compact.stdout.lines().count(), 1, "{compact:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_passes_params_through() {
    let t = TestDaemon::start().await;
    let out = t
        .heeler(&[
            "api",
            "workspace.create",
            r#"{"name": "via-api", "roots": ["/x"]}"#,
            "--json",
        ])
        .await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.json()["name"], "via-api");
    let list = t.heeler(&["api", "workspace.list", "{}", "--json"]).await;
    assert_eq!(list.json()["workspaces"][1]["roots"], json!(["/x"]));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_error_bodies_go_to_stdout_with_exit_1() {
    let t = TestDaemon::start().await;
    let unknown_method = t.heeler(&["api", "no.such.method", "--json"]).await;
    assert_eq!(unknown_method.code, 1, "{unknown_method:?}");
    assert_eq!(unknown_method.json()["code"], "bad_request");
    assert!(unknown_method.stderr.is_empty(), "{unknown_method:?}");

    let bad_params = t
        .heeler(&["api", "workspace.create", r#"{"nope": 1}"#, "--json"])
        .await;
    assert_eq!(bad_params.code, 1, "{bad_params:?}");
    assert_eq!(bad_params.json()["code"], "bad_request");

    let remote = t
        .heeler(&[
            "api",
            "workspace.close",
            r#"{"workspace_id": "01J00000000000000000000000"}"#,
            "--json",
        ])
        .await;
    assert_eq!(remote.code, 1, "{remote:?}");
    assert_eq!(remote.json()["code"], "not_found");
    assert!(remote.json()["message"].is_string());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_params_that_are_not_json_are_a_usage_error() {
    let t = TestDaemon::start().await;
    let out = t.heeler(&["api", "daemon.status", "{not json"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("not valid JSON"), "{out:?}");
    t.stop().await;
}

// events --------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn events_without_follow_prints_current_state_and_exits() {
    let t = TestDaemon::start().await;
    t.heeler(&["ws", "new", "one"]).await;
    let out = t.heeler(&["events"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    let lines: Vec<Value> = out
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{out:?}");
    assert!(lines.iter().all(|l| l["event"] == "workspace.updated"));
    assert_eq!(lines[0]["data"]["workspace"]["name"], "default");
    assert_eq!(lines[1]["data"]["workspace"]["name"], "one");
    assert_eq!(lines[1]["epoch"], t.daemon.epoch().to_string());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn events_follow_streams_new_events_as_json_lines() {
    let t = TestDaemon::start().await;
    t.heeler(&["ws", "new", "before"]).await;

    let mut child = command(Path::new(HEELER), &["events", "--follow"])
        .arg("--socket")
        .arg(&t.paths.socket)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next = async || -> Value {
        let line = timeout(WAIT, lines.next_line()).await.unwrap().unwrap();
        serde_json::from_str(&line.expect("stdout closed")).unwrap()
    };

    assert_eq!(next().await["data"]["workspace"]["name"], "default");
    let first = next().await;
    assert_eq!(first["data"]["workspace"]["name"], "before");
    t.heeler(&["ws", "new", "after"]).await;
    let second = next().await;
    assert_eq!(second["event"], "workspace.updated");
    assert_eq!(second["data"]["workspace"]["name"], "after");
    assert!(second["seq"].as_u64().unwrap() > first["seq"].as_u64().unwrap());

    child.kill().await.unwrap();
    t.stop().await;
}

// daemon stop ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn daemon_stop_shuts_the_daemon_down() {
    let t = TestDaemon::start().await;
    let out = t.heeler(&["daemon", "stop", "--json"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.json(), json!({"stopped": true}));
    assert!(!t.paths.socket.exists());
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();
}

// exit codes ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_daemon_exits_3() {
    let (_dir, socket) = missing_socket();
    for args in [
        &["daemon", "status"][..],
        &["daemon", "stop"],
        &["ws", "ls"],
        &["ws", "new", "x"],
        &["api", "daemon.status"],
        &["events"],
    ] {
        let out = heeler(&socket, args).await;
        assert_eq!(out.code, 3, "{args:?}: {out:?}");
        assert!(out.stderr.contains("heeler daemon start"), "{out:?}");
    }
    let json = heeler(&socket, &["daemon", "status", "--json"]).await;
    assert_eq!(json.code, 3);
    assert_eq!(json.json()["running"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_socket_file_counts_as_unreachable() {
    let t = TestDaemon::start().await;
    let stale = t.paths.base.join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    assert!(stale.exists());
    let out = heeler(&stale, &["daemon", "status"]).await;
    assert_eq!(out.code, 3, "{out:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_errors_exit_2() {
    let (_dir, socket) = missing_socket();
    for args in [
        &[][..],
        &["bogus"],
        &["ws"],
        &["ws", "new"],
        &["ws", "rename", "not-an-id", "x"],
        &["ws", "close", "not-an-id"],
        &["daemon", "status", "--bogus"],
        &["api"],
    ] {
        let out = heeler(&socket, args).await;
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
    }
    let help = heeler(&socket, &["--help"]).await;
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("daemon"));
}

// daemon start (real heelerd process) -------------------------------------------------------

/// Makes sure `heelerd` sits next to the `heeler` test binary; plain `cargo test` only builds
/// the binaries of the package under test.
fn heelerd_dir() -> PathBuf {
    static BUILD: Once = Once::new();
    let dir = Path::new(HEELER).parent().unwrap().to_owned();
    BUILD.call_once(|| {
        let status = std::process::Command::new(env!("CARGO"))
            .args([
                "build",
                "--locked",
                "-p",
                "heeler-daemon",
                "--bin",
                "heelerd",
            ])
            .status()
            .unwrap();
        assert!(status.success(), "building heelerd failed");
    });
    assert!(dir.join("heelerd").is_file(), "no heelerd in {dir:?}");
    dir
}

/// Stops a spawned daemon even when the test fails.
struct DaemonGuard {
    home: PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new(HEELER)
            .args(["daemon", "stop", "--stop-sessions"])
            .env("HEELER_HOME", &self.home)
            .env_remove("HEELER_SOCKET")
            .output();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_start_status_stop_with_heelerd_beside_heeler() {
    heelerd_dir();
    let home = tempfile::tempdir().unwrap();
    let _guard = DaemonGuard {
        home: home.path().to_owned(),
    };
    let run = |args: &'static [&'static str]| {
        let mut cmd = command(Path::new(HEELER), args);
        cmd.env("HEELER_HOME", home.path());
        finish(cmd)
    };

    let started = run(&["daemon", "start", "--json"]).await;
    assert_eq!(started.code, 0, "{started:?}");
    let started = started.json();
    assert_eq!(started["started"], true);
    let pid = started["pid"].as_u64().unwrap();
    assert_ne!(pid, u64::from(std::process::id()));

    let again = run(&["daemon", "start", "--json"]).await.json();
    assert_eq!(again["started"], false, "start is idempotent");
    assert_eq!(again["pid"], pid);
    assert_eq!(again["epoch"], started["epoch"]);

    let status = run(&["daemon", "status", "--json"]).await;
    assert_eq!(status.json()["pid"], pid);
    assert!(home.path().join("heelerd.sock").exists());

    let ws = run(&["ws", "new", "real", "--json"]).await;
    assert_eq!(ws.code, 0, "{ws:?}");

    let stopped = run(&["daemon", "stop"]).await;
    assert_eq!(stopped.code, 0, "{stopped:?}");
    assert!(!home.path().join("heelerd.sock").exists());
    assert_eq!(run(&["daemon", "status"]).await.code, 3);

    // A fresh start after a stop works (the lock was released).
    let restarted = run(&["daemon", "start", "--json"]).await.json();
    assert_eq!(restarted["started"], true);
    assert_ne!(restarted["epoch"], started["epoch"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_start_finds_heelerd_on_path() {
    let bin_dir = heelerd_dir();
    let home = tempfile::tempdir().unwrap();
    let _guard = DaemonGuard {
        home: home.path().to_owned(),
    };
    // A copy of `heeler` somewhere without a sibling heelerd.
    let lonely = tempfile::tempdir().unwrap();
    let copy = lonely.path().join("heeler");
    std::fs::copy(HEELER, &copy).unwrap();

    let mut missing = command(&copy, &["daemon", "start"]);
    missing
        .env("HEELER_HOME", home.path())
        .env("PATH", lonely.path());
    let out = finish(missing).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("heelerd not found") && out.stderr.contains("PATH"),
        "{out:?}"
    );

    let mut found = command(&copy, &["daemon", "start", "--json"]);
    found.env("HEELER_HOME", home.path()).env("PATH", &bin_dir);
    let out = finish(found).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.json()["started"], true);
}
