//! Daemon lifecycle: single-instance lock, socket binding, accept loop, orderly shutdown.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use hutan_agents::CodexConfig;
use hutan_core::Epoch;
use hutan_storage::Storage;
use tokio::net::UnixListener;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::accounts::AccountService;
use crate::agent;
use crate::assistant::AssistantService;
use crate::bus::{EventBus, RingLimits};
use crate::chat;
use crate::config::DaemonSettings;
use crate::conn::{self, ConnContext};
use crate::error::{DaemonError, MAX_SOCKET_PATH_BYTES};
use crate::harness::Harnesses;
use crate::history::HistoryService;
use crate::notify::NotifyService;
use crate::notify_native::NativePoster;
use crate::paths::Paths;
use crate::project_migration;
use crate::projects;
use crate::providers::{GatewayConfig, ProviderService};
use crate::registry::{self, RegistryConfig};
use crate::resources::{ResourceService, SystemProcesses};
use crate::restore::{self, Stored};
use crate::router::{Router, Services};
use crate::tasks::TaskService;
use crate::terminals::TerminalHub;
use crate::worktrees::WorktreeService;

pub const DEFAULT_OUTBOUND_QUEUE: usize = 1024;

/// A connection that has not sent `hello` after this long is closed.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long shutdown waits for connections to flush before abandoning them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How long shutdown waits for session processes to end: the SIGHUP grace plus margin for the
/// SIGKILL and reaping that follow it.
const SESSION_STOP_WAIT: Duration = registry::STOP_GRACE.saturating_add(Duration::from_secs(5));

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub paths: Paths,
    pub ring: RingLimits,
    /// Per-connection outbound queue length in control messages.
    pub outbound_queue: usize,
    /// How long a new connection may stay silent before `hello` arrives.
    pub handshake_timeout: Duration,
    /// The `hutan` CLI exported to sessions as `HUTAN_BIN`; `None` looks for it next to this
    /// executable, then on `PATH`.
    pub hutan_bin: Option<PathBuf>,
    /// Shortest time between two `last_active_ms` updates of one workspace.
    pub activity_interval: Duration,
    /// How to start `codex app-server`; `None` follows `[codex]` of `daemon.toml`.
    pub codex: Option<CodexConfig>,
    /// Credential store, HTTP policy and discovery targets of the model gateway; the default is
    /// the Keychain and the well-known local ports.
    pub gateway: GatewayConfig,
    /// The shell of sessions whose spec names none (as `SHELL` in the session environment),
    /// instead of the account's login shell; `None` keeps the account's. Also the shell whose
    /// login environment explicit-command sessions start from. For tests, which need a
    /// deterministic one.
    pub login_shell: Option<PathBuf>,
    /// Where intents are shown as system notifications: the real poster of a daemon that runs
    /// from `Hutan.app` (`notify_native::system_host`), a fake in tests; `None` posts nothing.
    /// `[notify] native = false` ignores it.
    pub native: Option<Arc<dyn NativePoster>>,
}

impl DaemonConfig {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            ring: RingLimits::default(),
            outbound_queue: DEFAULT_OUTBOUND_QUEUE,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            hutan_bin: None,
            activity_interval: registry::ACTIVITY_INTERVAL,
            codex: None,
            gateway: GatewayConfig::default(),
            login_shell: None,
            native: None,
        }
    }
}

/// A started daemon. Dropping it does not stop the daemon; call [`shutdown`](Self::shutdown).
#[derive(Debug)]
pub struct RunningDaemon {
    socket: PathBuf,
    epoch: Epoch,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl RunningDaemon {
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// Token that stops the daemon when cancelled (what signal handlers trigger).
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Requests an orderly stop; returns immediately. Pair with [`wait`](Self::wait).
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Resolves once the daemon has stopped: connections closed, socket removed, lock released.
    pub async fn wait(self) -> Result<(), DaemonError> {
        Ok(self.task.await?)
    }
}

/// Acquires the single-instance lock, binds the socket, and starts serving.
pub async fn start(config: DaemonConfig) -> Result<RunningDaemon, DaemonError> {
    let paths = &config.paths;
    let socket_len = paths.socket.as_os_str().as_bytes().len();
    if socket_len > MAX_SOCKET_PATH_BYTES {
        return Err(DaemonError::SocketPathTooLong {
            path: paths.socket.clone(),
            len: socket_len,
        });
    }

    paths.prepare().map_err(DaemonError::io(format!(
        "preparing {}",
        paths.base.display()
    )))?;
    let lock = acquire_lock(&paths.lock)?;
    // Under the lock nobody else touches the database. A daemon that cannot read its config or
    // open its database must not come up: it would serve an empty state and then overwrite
    // the real one.
    let settings = DaemonSettings::load(&paths.config_file, paths.user_home.as_deref())?;
    let home_folder = settings
        .home_folder
        .clone()
        .unwrap_or_else(|| paths.default_home_folder.clone());
    let home_folder = prepare_home_folder(home_folder).await?;
    let (storage, mut stored) = open_storage(&paths.database).await?;
    stored = convert_workspaces(&storage, stored, &paths.database).await?;
    remove_stale_socket(&paths.socket)?;
    let listener = UnixListener::bind(&paths.socket).map_err(DaemonError::io(format!(
        "binding {}",
        paths.socket.display()
    )))?;
    // From here on the guard removes the socket on every exit path.
    let socket_guard = SocketGuard(paths.socket.clone());
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600)).map_err(
        DaemonError::io(format!("securing {}", paths.socket.display())),
    )?;
    let daemon_uid = fs::metadata(&paths.socket)
        .map_err(DaemonError::io(format!(
            "inspecting {}",
            paths.socket.display()
        )))?
        .uid();

    let epoch = Epoch::new();
    if let Err(error) = storage.app_meta().record_epoch(epoch, registry::now_ms()) {
        warn!(%error, "could not record the epoch");
    }
    let shutdown = CancellationToken::new();
    let clients = Arc::new(AtomicU32::new(0));
    let bus = Arc::new(EventBus::new(epoch, config.ring));
    let hub = Arc::new(TerminalHub::new());
    // Best effort: without the scripts terminals still work, they just carry no prompt marks.
    let shell_integration_dir = match hutan_terminal::shell_integration::install(&paths.base) {
        Ok(dir) => Some(dir),
        Err(error) => {
            warn!(%error, "shell integration unavailable");
            None
        }
    };
    if settings.terminal.env_inherit == hutan_terminal::EnvInherit::Login {
        // The facts of the login environment come from a few subprocesses; do not make the
        // first session (or a restored one) wait for them.
        hutan_terminal::env::warm_up(
            config
                .login_shell
                .as_ref()
                .map(|shell| shell.to_string_lossy().into_owned()),
        );
    }
    let restoring = stored.sessions.len();
    let stored_tasks = std::mem::take(&mut stored.tasks);
    let providers = ProviderService::new(
        storage.clone(),
        config
            .gateway
            .credentials
            .unwrap_or_else(hutan_gateway::system_store),
        hutan_gateway::Http::new(config.gateway.http)
            .map_err(|e| DaemonError::Gateway(e.to_string()))?,
        config
            .gateway
            .discovery
            .unwrap_or_else(hutan_gateway::discovery::default_targets),
        std::mem::take(&mut stored.providers),
    );
    let login_shell = config
        .login_shell
        .as_ref()
        .map(|shell| shell.to_string_lossy().into_owned());
    let harnesses = Arc::new(Harnesses::new(
        settings.harness.clone(),
        &settings.mcp,
        settings.terminal.env_inherit,
        login_shell,
    ));
    let codex_config = config.codex.unwrap_or_else(|| settings.codex.to_config());
    let registry = registry::spawn(
        bus.clone(),
        hub.clone(),
        RegistryConfig {
            socket: paths.socket.clone(),
            shell_integration_dir,
            env_inherit: settings.terminal.env_inherit,
            login_shell: config
                .login_shell
                .as_ref()
                .map(|shell| shell.to_string_lossy().into_owned()),
            hutan_bin: config.hutan_bin.unwrap_or_else(agent::resolve_hutan_bin),
            storage: storage.clone(),
            restore: settings.restore_sessions,
            home_folder,
            user_home: paths.user_home.clone(),
            auto_projects: settings.projects.auto,
            activity_interval: config.activity_interval,
            codex: codex_config.clone(),
            harnesses: harnesses.clone(),
            providers,
            max_context_chars: settings.gateway.max_context_chars,
        },
        stored,
    );
    let history = HistoryService::new(
        paths.clone(),
        settings.history.clone(),
        bus.clone(),
        hub.clone(),
        registry.clone(),
    );
    history.start().await;
    let assistant = AssistantService::new(
        settings.assistant.clone(),
        registry.clone(),
        hub.clone(),
        history.clone(),
        storage.clone(),
    );
    let notify = NotifyService::new(
        settings.notify.clone(),
        bus.clone(),
        registry.clone(),
        config.native,
    );
    // Observes the bus until shutdown; nothing needs its handle.
    drop(notify.start(shutdown.clone()));
    let worktrees = WorktreeService::new(registry.clone(), storage.clone(), bus.clone());
    // After the registry restored its sessions and the notifier listens: a task whose session
    // did not survive the last daemon ends here, and says so.
    let tasks = TaskService::open(
        registry.clone(),
        worktrees.clone(),
        storage.clone(),
        bus.clone(),
        &settings.tasks,
        stored_tasks,
    )
    .await
    .map_err(|error| DaemonError::Tasks(error.message))?;
    drop(tasks.run(shutdown.clone()));
    let accounts = AccountService::new(
        registry.clone(),
        harnesses,
        codex_config,
        bus.clone(),
        shutdown.clone(),
    );
    let resources = ResourceService::new(
        registry.clone(),
        hub.clone(),
        bus.clone(),
        Arc::new(SystemProcesses),
    );
    let router = Router::new(
        registry,
        hub,
        Services {
            history,
            assistant,
            notify,
            worktrees,
            tasks,
            accounts,
            resources,
        },
        bus.clone(),
        shutdown.clone(),
        clients.clone(),
    );
    let ctx = Arc::new(ConnContext::new(
        router,
        bus,
        shutdown.clone(),
        clients,
        config.outbound_queue.max(2),
        daemon_uid,
        config.handshake_timeout,
    ));

    info!(
        socket = %paths.socket.display(),
        database = %paths.database.display(),
        restored_sessions = restoring,
        restore_mode = ?settings.restore_sessions,
        pid = std::process::id(),
        %epoch,
        protocol = ?hutan_protocol::PROTOCOL_VERSION,
        "hutand listening"
    );

    let held = Held {
        _socket: socket_guard,
        _lock: lock,
    };
    let task = tokio::spawn(accept_loop(listener, ctx, shutdown.clone(), held, storage));
    Ok(RunningDaemon {
        socket: paths.socket.clone(),
        epoch,
        shutdown,
        task,
    })
}

/// Resources released at the end of the daemon's life. Field order matters: the socket is
/// removed before the lock is released, so a successor never sees our socket.
#[derive(Debug)]
struct Held {
    _socket: SocketGuard,
    _lock: File,
}

#[derive(Debug)]
struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        match fs::remove_file(&self.0) {
            Ok(()) => debug!(socket = %self.0.display(), "socket removed"),
            Err(e) => warn!(socket = %self.0.display(), error = %e, "could not remove socket"),
        }
    }
}

async fn accept_loop(
    listener: UnixListener,
    ctx: Arc<ConnContext>,
    shutdown: CancellationToken,
    held: Held,
    storage: Storage,
) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    conns.spawn(conn::serve(stream, ctx.clone()));
                }
                Err(e) => {
                    // Typically fd exhaustion; back off instead of spinning.
                    error!(error = %e, "accept failed");
                    sleep(Duration::from_millis(100)).await;
                }
            },
            Some(finished) = conns.join_next(), if !conns.is_empty() => {
                log_connection_exit(finished);
            }
        }
    }
    info!("shutting down");
    drop(listener);

    // Session processes die with the daemon (ADR 0009): hang them up and reap them while the
    // connections (which watch the same token) flush their queues and close.
    let stop_sessions = async {
        match timeout(SESSION_STOP_WAIT, ctx.router.stop_sessions()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => error!(message = %error.message, "stopping sessions failed"),
            Err(_) => warn!("sessions did not end in time; leaving them to the kernel"),
        }
    };
    let drain_connections = async {
        let drained = timeout(SHUTDOWN_GRACE, async {
            while let Some(finished) = conns.join_next().await {
                log_connection_exit(finished);
            }
        })
        .await;
        if drained.is_err() {
            warn!(
                remaining = conns.len(),
                "connections did not close in time; abandoning them"
            );
            conns.abort_all();
        }
    };
    tokio::join!(stop_sessions, drain_connections);
    // Records from the sessions that just ended are applied before the indexer stops.
    ctx.router.stop_history().await;
    // Everything the registry queued while stopping the sessions reaches the disk before the
    // lock is released and a successor opens the database.
    if let Err(error) = tokio::task::spawn_blocking(move || storage.close()).await {
        error!(%error, "closing the database failed");
    }
    drop(held);
    info!("hutand stopped");
}

/// Creates the Home folder when it is missing (and only then) and returns its canonical path.
async fn prepare_home_folder(folder: PathBuf) -> Result<PathBuf, DaemonError> {
    let context = format!("preparing the Home folder {}", folder.display());
    tokio::task::spawn_blocking(move || projects::ensure_home_folder(&folder))
        .await?
        .map_err(DaemonError::io(context))
}

/// Opens (and migrates) the database and reads its content, off the async threads.
async fn open_storage(database: &Path) -> Result<(Storage, Stored), DaemonError> {
    let path = database.to_owned();
    tokio::task::spawn_blocking(move || {
        let storage = Storage::open(&path)?;
        let stored = restore::load(&storage)?;
        // Agents died with the previous daemon; whatever they asked can no longer be answered.
        chat::expire_dangling_approvals(&storage)?;
        Ok((storage, stored))
    })
    .await?
    .map_err(DaemonError::storage(database))
}

/// Turns the workspaces of earlier versions into workspaces with member projects, once
/// (`docs/orchestration.md` § Projects), off the async threads.
async fn convert_workspaces(
    storage: &Storage,
    mut stored: Stored,
    database: &Path,
) -> Result<Stored, DaemonError> {
    let storage = storage.clone();
    tokio::task::spawn_blocking(move || {
        project_migration::run(&storage, &mut stored)?;
        Ok(stored)
    })
    .await?
    .map_err(DaemonError::storage(database))
}

fn log_connection_exit(finished: Result<(), tokio::task::JoinError>) {
    if let Err(e) = finished
        && e.is_panic()
    {
        error!(error = %e, "connection task panicked");
    }
}

fn acquire_lock(path: &Path) -> Result<File, DaemonError> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(DaemonError::io(format!("opening lock {}", path.display())))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(DaemonError::AlreadyRunning {
                lock: path.to_owned(),
            });
        }
        Err(TryLockError::Error(e)) => {
            return Err(DaemonError::io(format!("locking {}", path.display()))(e));
        }
    }
    // Informational only: lets a human see who holds the lock.
    file.set_len(0)
        .and_then(|()| writeln!(file, "{}", std::process::id()))
        .map_err(DaemonError::io(format!("writing {}", path.display())))?;
    Ok(file)
}

/// Removes a socket file left by a dead daemon. Safe only while holding the lock.
fn remove_stale_socket(path: &Path) -> Result<(), DaemonError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            info!(socket = %path.display(), "removing stale socket");
            fs::remove_file(path).map_err(DaemonError::io(format!(
                "removing stale {}",
                path.display()
            )))
        }
        Ok(_) => Err(DaemonError::Io {
            context: format!("{} exists and is not a socket", path.display()),
            source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::io(format!("inspecting {}", path.display()))(e)),
    }
}
