//! Daemon lifecycle: single-instance lock, socket binding, accept loop, orderly shutdown.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use heeler_core::Epoch;
use heeler_storage::Storage;
use tokio::net::UnixListener;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::bus::{EventBus, RingLimits};
use crate::config::DaemonSettings;
use crate::conn::{self, ConnContext};
use crate::error::{DaemonError, MAX_SOCKET_PATH_BYTES};
use crate::paths::Paths;
use crate::registry::{self, RegistryConfig};
use crate::restore::{self, Stored};
use crate::router::Router;
use crate::terminals::TerminalHub;

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
}

impl DaemonConfig {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            ring: RingLimits::default(),
            outbound_queue: DEFAULT_OUTBOUND_QUEUE,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
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
    let settings = DaemonSettings::load(&paths.config_file)?;
    let (storage, stored) = open_storage(&paths.database).await?;
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
    let shell_integration_dir = match heeler_terminal::shell_integration::install(&paths.base) {
        Ok(dir) => Some(dir),
        Err(error) => {
            warn!(%error, "shell integration unavailable");
            None
        }
    };
    let restoring = stored.sessions.len();
    let registry = registry::spawn(
        bus.clone(),
        hub.clone(),
        RegistryConfig {
            socket: paths.socket.clone(),
            shell_integration_dir,
            storage: storage.clone(),
            restore: settings.restore_sessions,
        },
        stored,
    );
    let router = Router::new(
        registry,
        hub,
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
        protocol = ?heeler_protocol::PROTOCOL_VERSION,
        "heelerd listening"
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
    // Everything the registry queued while stopping the sessions reaches the disk before the
    // lock is released and a successor opens the database.
    if let Err(error) = tokio::task::spawn_blocking(move || storage.close()).await {
        error!(%error, "closing the database failed");
    }
    drop(held);
    info!("heelerd stopped");
}

/// Opens (and migrates) the database and reads its content, off the async threads.
async fn open_storage(database: &Path) -> Result<(Storage, Stored), DaemonError> {
    let path = database.to_owned();
    tokio::task::spawn_blocking(move || {
        let storage = Storage::open(&path)?;
        let stored = restore::load(&storage)?;
        Ok((storage, stored))
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
