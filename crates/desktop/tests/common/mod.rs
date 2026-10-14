//! Harness for the desktop's GPUI tests: an in-process daemon on a temporary `HEELER_HOME`
//! (driven from its own Tokio runtime, since GPUI has no reactor), a real `Connection`, and
//! helpers to pump GPUI while real I/O happens on other threads.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns
#![allow(dead_code)] // each test binary uses a different subset

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{Entity, TestAppContext, VisualTestContext};
use heeler_client::{Client, ConnectOptions, Connection};
use heeler_core::{SessionId, WorkspaceId};
use heeler_daemon::{DaemonConfig, Paths, RunningDaemon};
use heeler_desktop::workbench::{Workbench, WorkbenchOptions};
use heeler_protocol::messages::{
    ClientInfo, ClientKind, Request, SessionCreate, SessionInfo, SessionResult, SessionSpec,
    TerminalSpec, WorkspaceCreate, WorkspaceInfo,
};
use heeler_settings::{SettingsPaths, SettingsStore};
use serde::de::DeserializeOwned;
use tempfile::TempDir;

pub const WAIT: Duration = Duration::from_secs(20);

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Desktop,
        version: "desktop-test".into(),
    }
}

/// Forwards every connection on its own socket to the daemon's socket, so a test can cut the
/// link without restarting the daemon.
pub struct Proxy {
    pub path: PathBuf,
    sever: tokio::sync::broadcast::Sender<()>,
    blocked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    fn start(runtime: &tokio::runtime::Runtime, path: PathBuf, upstream: PathBuf) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::copy_bidirectional;
        use tokio::net::{UnixListener, UnixStream};
        let listener = {
            let _guard = runtime.enter();
            UnixListener::bind(&path).unwrap()
        };
        let blocked = std::sync::Arc::new(AtomicBool::new(false));
        let (sever, _) = tokio::sync::broadcast::channel(1);
        let task = runtime.spawn({
            let blocked = blocked.clone();
            let sever = sever.clone();
            async move {
                loop {
                    let Ok((mut client, _)) = listener.accept().await else {
                        return;
                    };
                    if blocked.load(Ordering::SeqCst) {
                        continue;
                    }
                    let upstream = upstream.clone();
                    let mut cut = sever.subscribe();
                    tokio::spawn(async move {
                        let Ok(mut daemon) = UnixStream::connect(&upstream).await else {
                            return;
                        };
                        tokio::select! {
                            _ = copy_bidirectional(&mut client, &mut daemon) => {}
                            _ = cut.recv() => {}
                        }
                    });
                }
            }
        });
        Self {
            path,
            sever,
            blocked,
            task,
        }
    }

    /// Closes every connection that passes through right now.
    pub fn sever(&self) {
        let _ = self.sever.send(());
    }

    /// While blocked, new connections are accepted and closed at once.
    pub fn block(&self, blocked: bool) {
        self.blocked
            .store(blocked, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A daemon in this process on a fresh `HEELER_HOME`.
pub struct TestDaemon {
    pub dir: TempDir,
    pub paths: Paths,
    runtime: Option<tokio::runtime::Runtime>,
    daemon: Option<RunningDaemon>,
    proxy: Option<Proxy>,
}

impl TestDaemon {
    pub fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("test-daemon")
            .enable_all()
            .build()
            .unwrap();
        let daemon = runtime
            .block_on(heeler_daemon::start(DaemonConfig::new(paths.clone())))
            .unwrap();
        Self {
            dir,
            paths,
            runtime: Some(runtime),
            daemon: Some(daemon),
            proxy: None,
        }
    }

    /// A connection that goes through a proxy whose link [`TestDaemon::sever`] can cut.
    pub fn proxied_connection(&mut self) -> Connection {
        let path = self.dir.path().join("proxy.sock");
        if self.proxy.is_none() {
            let proxy = Proxy::start(self.rt(), path.clone(), self.paths.socket.clone());
            self.proxy = Some(proxy);
        }
        let mut options = self.options();
        options.socket = Some(path);
        Connection::start(options)
    }

    pub fn proxy(&self) -> &Proxy {
        self.proxy.as_ref().expect("proxied_connection was called")
    }

    fn rt(&self) -> &tokio::runtime::Runtime {
        self.runtime.as_ref().unwrap()
    }

    pub fn home(&self) -> &Path {
        self.dir.path()
    }

    pub fn options(&self) -> ConnectOptions {
        ConnectOptions {
            socket: Some(self.paths.socket.clone()),
            autostart: None,
            client: client_info(),
        }
    }

    pub fn connection(&self) -> Connection {
        Connection::start(self.options())
    }

    /// One request on a fresh connection, answered synchronously.
    pub fn call<T: DeserializeOwned + Send + 'static>(&self, request: Request) -> T {
        let options = self.options();
        self.rt().block_on(async move {
            let client = Client::connect(options).await.unwrap();
            client.call(request).await.unwrap()
        })
    }

    pub fn create_workspace(&self, name: &str, roots: &[&Path]) -> WorkspaceInfo {
        self.call(Request::WorkspaceCreate(WorkspaceCreate {
            name: name.to_owned(),
            roots: roots.iter().map(|r| r.to_path_buf()).collect(),
        }))
    }

    /// A terminal session running `argv` with a plain `$ ` prompt.
    pub fn create_session(&self, argv: &[&str], workspace: Option<WorkspaceId>) -> SessionInfo {
        let created: SessionResult = self.call(Request::SessionCreate(SessionCreate {
            workspace_id: workspace,
            spec: SessionSpec::Terminal(TerminalSpec {
                command: argv.iter().map(|a| (*a).to_owned()).collect(),
                env: [("PS1".to_owned(), "$ ".to_owned())].into(),
                ..TerminalSpec::default()
            }),
        }));
        created.session
    }

    /// `sh -c script` as a session.
    pub fn sh(&self, script: &str) -> SessionInfo {
        self.create_session(&["sh", "-c", script], None)
    }

    /// `sh -c script` as a session named `title`.
    pub fn sh_titled(&self, title: &str, script: &str) -> SessionInfo {
        let created: SessionResult = self.call(Request::SessionCreate(SessionCreate {
            workspace_id: None,
            spec: SessionSpec::Terminal(TerminalSpec {
                command: vec!["sh".into(), "-c".into(), script.to_owned()],
                title: Some(title.to_owned()),
                ..TerminalSpec::default()
            }),
        }));
        created.session
    }

    pub fn session(&self, id: SessionId) -> SessionInfo {
        let list: heeler_protocol::messages::SessionList =
            self.call(Request::SessionList(Default::default()));
        list.sessions.into_iter().find(|s| s.id == id).unwrap()
    }

    pub fn stop(&mut self) {
        let daemon = self.daemon.take().expect("daemon is running");
        daemon.shutdown();
        self.rt()
            .block_on(async { tokio::time::timeout(WAIT, daemon.wait()).await })
            .unwrap()
            .unwrap();
    }

    /// A new daemon on the same home: new epoch, no sessions.
    pub fn restart(&mut self) {
        assert!(self.daemon.is_none(), "daemon is already running");
        let paths = self.paths.clone();
        self.daemon = Some(
            self.rt()
                .block_on(heeler_daemon::start(DaemonConfig::new(paths)))
                .unwrap(),
        );
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let (Some(daemon), Some(runtime)) = (self.daemon.take(), self.runtime.take()) {
            daemon.shutdown();
            let _ = runtime.block_on(async { tokio::time::timeout(WAIT, daemon.wait()).await });
            runtime.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

thread_local! {
    /// Temp dirs of tests that have no daemon; dropped when the test thread ends.
    static KEEP: RefCell<Vec<TempDir>> = const { RefCell::new(Vec::new()) };
}

/// A connection to a daemon that does not exist (layout-only tests).
pub fn offline_connection() -> (Connection, SettingsStore, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("none.sock");
    let settings = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
    let state = dir.path().join("desktop-state.json");
    KEEP.with(|keep| keep.borrow_mut().push(dir));
    let connection = Connection::start(ConnectOptions {
        socket: Some(socket),
        autostart: None,
        client: client_info(),
    });
    (connection, settings, state)
}

/// Opens a workbench over `settings` and `connection`.
pub fn open_with(
    cx: &mut TestAppContext,
    settings: SettingsStore,
    connection: Connection,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &mut VisualTestContext) {
    // Real sockets and threads wake GPUI tasks from other threads.
    cx.executor().allow_parking();
    let options = WorkbenchOptions::builtin(settings, connection, state_path);
    let (view, cx) = cx.add_window_view(|window, cx| Workbench::new(options, window, cx));
    // A real window is active; the test platform starts windows inactive.
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (view, cx)
}

/// One frame: draw, then the next-frame callbacks the platform runs before the following draw
/// (terminal views ack there).
pub fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
    cx.run_until_parked();
}

/// Pumps GPUI (and draws) until `condition` holds; panics with `what` after [`WAIT`].
pub fn wait_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        draw(cx);
        if condition(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Text of the terminal view's screen, rows joined by newlines.
pub fn screen_text(model: &heeler_terminal_view::model::TerminalModel) -> String {
    model
        .viewport()
        .iter()
        .map(heeler_terminal_view::model::TerminalModel::row_text)
        .collect::<Vec<_>>()
        .join("\n")
}
