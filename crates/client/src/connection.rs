//! [`Connection`]: the reconnecting, executor-independent client that UIs build on.
//!
//! GPUI drives its own executor, so a `Connection` owns one OS thread (`heeler-client`) running
//! a current-thread Tokio runtime with the connection task. Everything public is a channel, a
//! `watch` or a `broadcast` handle; none of them needs a Tokio context, so they can be used from
//! any executor or plain thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use heeler_core::TerminalId;
use heeler_protocol::blocks::{
    BlockPart, TerminalBlockText, TerminalBlockTextResult, TerminalBlocks, TerminalBlocksResult,
};
use heeler_protocol::history::{HistoryHits, HistorySearch, HistorySetEnabled, HistoryStatusInfo};
use heeler_protocol::messages::{Empty, Request, TerminalSize};
use serde::de::DeserializeOwned;
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::blocks::BlockList;
use crate::client::{Client, ConnectOptions};
use crate::error::ClientError;
use crate::socket::default_socket_path;
use crate::store::{ConnectionState, Notice, StoreState};
use crate::supervisor::Supervisor;
use crate::terminal::{Command, TerminalHandle, UPDATE_BUFFER, send_command};

/// Name of the connection's thread; tests look for it by this prefix.
const THREAD_NAME: &str = "heeler-client";
/// Notices buffered per receiver before it starts lagging.
const NOTICE_BUFFER: usize = 256;
/// Terminal commands queued for the connection task.
const COMMAND_BUFFER: usize = 1024;

/// State shared between the public handles and the connection task.
#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) state: watch::Sender<Arc<StoreState>>,
    pub(crate) notices: broadcast::Sender<Notice>,
    /// The live, synchronised connection; `None` whenever disconnected.
    client: Mutex<Option<Client>>,
    commands: mpsc::Sender<Command>,
    next_handle: AtomicU64,
    /// Whether a missing daemon may be started (see [`Connection::set_autostart`]).
    pub(crate) autostart: AtomicBool,
    /// Wakes a backoff wait so a reconnect happens now.
    pub(crate) kick: Notify,
}

impl Shared {
    fn client(&self) -> Option<Client> {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set_client(&self, client: Option<Client>) {
        *self.client.lock().unwrap_or_else(PoisonError::into_inner) = client;
    }
}

/// Stops the connection thread when the last [`Connection`] clone goes away (or on shutdown).
#[derive(Debug)]
struct Owner {
    cancel: CancellationToken,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Owner {
    /// Cancels the task and waits for the thread to end. Idempotent.
    fn stop(&self) {
        self.cancel.cancel();
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread
            && thread.thread().id() != std::thread::current().id()
            && thread.join().is_err()
        {
            error!("the heeler-client thread panicked");
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A connection to `heelerd` that reconnects by itself and keeps a [`StoreState`] current.
///
/// Clones share the connection; it ends when the last clone is dropped or
/// [`shutdown`](Self::shutdown) is called on any clone.
#[derive(Debug, Clone)]
pub struct Connection {
    shared: Arc<Shared>,
    owner: Arc<Owner>,
    socket: Option<PathBuf>,
}

impl Connection {
    /// Returns immediately. Connects (starting `heelerd` per `opts.autostart`, at most once per
    /// 10 s) and reconnects with exponential backoff (100 ms doubling to 5 s, ±20 % jitter)
    /// until dropped or shut down.
    pub fn start(opts: ConnectOptions) -> Connection {
        let socket = opts.socket.clone().or_else(default_socket_path);
        let (state, _) = watch::channel(Arc::new(StoreState::initial()));
        let (notices, _) = broadcast::channel(NOTICE_BUFFER);
        let (commands, command_rx) = mpsc::channel(COMMAND_BUFFER);
        let shared = Arc::new(Shared {
            state,
            notices,
            client: Mutex::new(None),
            commands,
            next_handle: AtomicU64::new(1),
            autostart: AtomicBool::new(opts.autostart.is_some()),
            kick: Notify::new(),
        });
        let cancel = CancellationToken::new();
        let supervisor = Supervisor::new(opts, shared.clone(), command_rx, cancel.clone());
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || run_thread(supervisor));
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(e) => {
                error!(error = %e, "cannot start the heeler-client thread");
                shared.state.send_modify(|state| {
                    Arc::make_mut(state).connection = ConnectionState::Stopped;
                });
                None
            }
        };
        Connection {
            shared,
            owner: Arc::new(Owner {
                cancel,
                thread: Mutex::new(thread),
            }),
            socket,
        }
    }

    /// The latest full state. Coalesced: a receiver that is slow sees the newest state, not
    /// every intermediate one.
    pub fn state(&self) -> watch::Receiver<Arc<StoreState>> {
        self.shared.state.subscribe()
    }

    /// Transient events (bell, exit, restart). A receiver that falls behind misses the oldest.
    pub fn notices(&self) -> broadcast::Receiver<Notice> {
        self.shared.notices.subscribe()
    }

    /// The socket this connection dials (`ConnectOptions::socket`, else the default location).
    pub fn socket_path(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// Allows or forbids starting `heelerd` when nothing listens (it has no effect without
    /// [`ConnectOptions::autostart`]). Forbidding keeps the connection retrying quietly, so a
    /// daemon the user stopped is not brought back; allowing it again reconnects at once and
    /// starts the daemon without waiting for the autostart interval.
    pub fn set_autostart(&self, enabled: bool) {
        let was = self.shared.autostart.swap(enabled, Ordering::AcqRel);
        if enabled && !was {
            self.shared.kick.notify_one();
        }
    }

    /// Whether a missing daemon is started ([`set_autostart`](Self::set_autostart)).
    pub fn autostart(&self) -> bool {
        self.shared.autostart.load(Ordering::Acquire)
    }

    /// Sends `req` on the current connection. Fails with [`ClientError::Disconnected`] when
    /// not connected, and when the connection drops mid-call; a call is never queued across a
    /// reconnect.
    pub async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        req: Request,
    ) -> Result<T, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.call(req).await
    }

    /// True while connected to a daemon that advertised capability `terminal.blocks`
    /// ([`Client::supports_blocks`]); false when disconnected.
    pub fn supports_blocks(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_blocks())
    }

    /// Every block the daemon keeps for `terminal` ([`BlockList::fetch`] on the current
    /// connection).
    pub async fn fetch_blocks(&self, terminal: TerminalId) -> Result<BlockList, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        BlockList::fetch(&client, terminal).await
    }

    /// One page of `terminal`'s blocks, newest first (see [`Client::terminal_blocks`]).
    pub async fn terminal_blocks(
        &self,
        terminal: TerminalId,
        before_id: Option<u64>,
        limit: u32,
    ) -> Result<TerminalBlocksResult, ClientError> {
        self.call(Request::TerminalBlocks(TerminalBlocks {
            terminal_id: terminal,
            before_id,
            limit,
        }))
        .await
    }

    /// Plain text of a block's command or output (see [`Client::terminal_block_text`]).
    pub async fn terminal_block_text(
        &self,
        terminal: TerminalId,
        block_id: u64,
        part: BlockPart,
        max_bytes: u32,
    ) -> Result<TerminalBlockTextResult, ClientError> {
        self.call(Request::TerminalBlockText(TerminalBlockText {
            terminal_id: terminal,
            block_id,
            part,
            max_bytes,
        }))
        .await
    }

    /// True while connected to a daemon that advertised capability `history.search`
    /// ([`Client::supports_history`]); false when disconnected.
    pub fn supports_history(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_history())
    }

    /// `history.search`: one hit per matching session, best first. Fails with the daemon's
    /// `invalid_state` while indexing is off.
    pub async fn history_search(&self, search: HistorySearch) -> Result<HistoryHits, ClientError> {
        self.call(Request::HistorySearch(search)).await
    }

    /// `history.status`: whether indexing is on, per-source counters, database size.
    pub async fn history_status(&self) -> Result<HistoryStatusInfo, ClientError> {
        self.call(Request::HistoryStatus(Empty {})).await
    }

    /// `history.set_enabled`: turns indexing on or off (recorded in `daemon.toml`); answers with
    /// the new status.
    pub async fn history_set_enabled(
        &self,
        enabled: bool,
    ) -> Result<HistoryStatusInfo, ClientError> {
        self.call(Request::HistorySetEnabled(HistorySetEnabled { enabled }))
            .await
    }

    /// `history.clear`: deletes the index and, while indexing is on, starts an empty one.
    pub async fn history_clear(&self) -> Result<(), ClientError> {
        self.call::<serde_json::Value>(Request::HistoryClear(Empty {}))
            .await
            .map(drop)
    }

    /// `history.rebuild`: reads every transcript again in the background. Fails with the
    /// daemon's `invalid_state` while indexing is off.
    pub async fn history_rebuild(&self) -> Result<(), ClientError> {
        self.call::<serde_json::Value>(Request::HistoryRebuild(Empty {}))
            .await
            .map(drop)
    }

    /// A durable view of `terminal` that re-attaches across reconnects. Returns at once; the
    /// attach happens when (and whenever) the connection is up. At most one handle per
    /// terminal is active: attaching again ends the earlier handle's update stream.
    pub fn attach(&self, terminal: TerminalId, size: Option<TerminalSize>) -> TerminalHandle {
        let handle = self.shared.next_handle.fetch_add(1, Ordering::Relaxed);
        let (updates, updates_rx) = mpsc::channel(UPDATE_BUFFER);
        send_command(
            &self.shared.commands,
            Command::Attach {
                handle,
                terminal,
                size,
                updates,
            },
        );
        TerminalHandle::new(terminal, handle, updates_rx, self.shared.commands.clone())
    }

    /// Stops reconnecting, closes the connection and waits for the thread to finish. Affects
    /// every clone: their state becomes [`ConnectionState::Stopped`] and calls fail.
    pub fn shutdown(self) {
        self.owner.stop();
    }
}

/// Body of the `heeler-client` thread.
fn run_thread(supervisor: Supervisor) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    match runtime {
        Ok(runtime) => runtime.block_on(supervisor.run()),
        Err(e) => {
            error!(error = %e, "cannot build the heeler-client runtime");
            supervisor.finish();
        }
    }
}
