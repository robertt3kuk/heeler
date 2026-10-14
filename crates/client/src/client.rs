use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use heeler_core::{Epoch, Seq, TerminalId};
use heeler_protocol::messages::{
    ClientInfo, Event, Hello, Request, ResumePoint, Welcome, decode_result,
};
use heeler_protocol::render::TerminalRender;
use heeler_protocol::{
    ControlMessage, ErrorBody, Frame, FrameCodec, FrameKind, MAX_PAYLOAD, PROTOCOL_VERSION,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{sleep, timeout};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{debug, warn};

use crate::error::ClientError;
use crate::socket::default_socket_path;

/// Capability for `terminal.set_colors`; the client sends colors only to daemons advertising it.
pub(crate) const COLORS_CAPABILITY: &str = "terminal.colors";
/// Capabilities this client understands, offered in `hello`.
const CAPABILITIES: &[&str] = &["terminal.render.v1", COLORS_CAPABILITY, "session.restore"];
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const AUTOSTART_WAIT: Duration = Duration::from_secs(3);
const AUTOSTART_POLL: Duration = Duration::from_millis(25);
/// A missing socket triggers at most one autostart per this interval when dialing repeatedly.
const AUTOSTART_INTERVAL: Duration = Duration::from_secs(10);
/// Events buffered per [`Client::events`] receiver before it starts lagging.
const EVENT_BUFFER: usize = 1024;
/// Terminal render messages buffered per [`Client::terminal_frames`] receiver.
const TERMINAL_BUFFER: usize = 64;
/// Outgoing frames queued before `call`/`send_input` wait for the socket.
const OUTBOX: usize = 256;
/// Terminal input is split so every frame stays under the protocol's payload limit.
const MAX_INPUT_CHUNK: usize = MAX_PAYLOAD - 8;

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Socket to dial; default per `HEELER_SOCKET` / `HEELER_HOME` / the standard location.
    pub socket: Option<PathBuf>,
    /// `heelerd` binary to spawn (detached) when nothing accepts connections on the socket.
    pub autostart: Option<PathBuf>,
    pub client: ClientInfo,
}

/// Result of [`Client::connect_resuming`]. `events` was subscribed before the connection started
/// reading, so it also yields the events the daemon replays right after the welcome.
#[derive(Debug)]
pub struct Resumed {
    pub client: Client,
    pub events: broadcast::Receiver<(Seq, Event)>,
}

/// Handle to one daemon connection. Clones share it; the connection closes when the last
/// clone is dropped or the daemon goes away.
#[derive(Debug, Clone)]
pub struct Client {
    shared: Arc<Shared>,
    outbox: mpsc::Sender<Frame>,
    next_id: Arc<AtomicU64>,
    welcome: Arc<Welcome>,
}

#[derive(Debug)]
struct Shared {
    pending: Mutex<Pending>,
    /// `None` once the connection is gone, so receivers observe `Closed`.
    events: Mutex<Option<broadcast::Sender<(Seq, Event)>>>,
    terminals: Mutex<HashMap<TerminalId, mpsc::Sender<TerminalRender>>>,
    last_seq: AtomicU64,
}

type CallOutcome = Result<Value, ErrorBody>;

#[derive(Debug, Default)]
struct Pending {
    closed: bool,
    calls: HashMap<u64, oneshot::Sender<CallOutcome>>,
}

impl Client {
    /// Connects and handshakes. If the socket is absent (or stale) and `opts.autostart` is set,
    /// spawns `heelerd` and waits up to 3 s for it.
    pub async fn connect(opts: ConnectOptions) -> Result<Self, ClientError> {
        Ok(Self::connect_resuming(opts, None).await?.client)
    }

    /// Like [`connect`](Self::connect), asking the daemon to replay events after `resume`.
    /// Check [`welcome`](Self::welcome)`.resume`: on `Replayed` the connection is already
    /// subscribed; on `SnapshotRequired` fetch `session.snapshot` and call `events.subscribe`.
    pub async fn connect_resuming(
        opts: ConnectOptions,
        resume: Option<ResumePoint>,
    ) -> Result<Resumed, ClientError> {
        Self::connect_gated(opts, resume, || true).await
    }

    /// [`connect_resuming`](Self::connect_resuming) where `permit` decides whether a missing
    /// socket may trigger an autostart (consulted only when `opts.autostart` is set).
    pub(crate) async fn connect_gated(
        opts: ConnectOptions,
        resume: Option<ResumePoint>,
        permit: impl FnOnce() -> bool,
    ) -> Result<Resumed, ClientError> {
        let socket = opts
            .socket
            .clone()
            .or_else(default_socket_path)
            .ok_or(ClientError::NoSocketPath)?;
        let stream = dial(
            &socket,
            opts.autostart.as_deref(),
            opts.socket.is_some(),
            permit,
        )
        .await?;

        let (read_half, write_half) = stream.into_split();
        let mut reader = FramedRead::new(read_half, FrameCodec);
        let mut writer = FramedWrite::new(write_half, FrameCodec);
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client: opts.client,
            capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
            resume,
        };
        let welcome = timeout(
            HANDSHAKE_TIMEOUT,
            handshake(&mut reader, &mut writer, hello),
        )
        .await
        .map_err(|_| ClientError::Handshake("timed out waiting for welcome".into()))??;

        let replayed = resume
            .filter(|r| r.epoch == welcome.epoch)
            .filter(|_| welcome.resume == heeler_protocol::messages::ResumeOutcome::Replayed);
        let (events_tx, events_rx) = broadcast::channel(EVENT_BUFFER);
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending::default()),
            events: Mutex::new(Some(events_tx)),
            terminals: Mutex::new(HashMap::new()),
            last_seq: AtomicU64::new(replayed.map_or(0, |r| r.seq.0)),
        });
        let (outbox, outbox_rx) = mpsc::channel(OUTBOX);
        tokio::spawn(read_loop(reader, shared.clone()));
        tokio::spawn(write_loop(writer, outbox_rx));

        let client = Client {
            shared,
            outbox,
            // Id 0 was the hello.
            next_id: Arc::new(AtomicU64::new(1)),
            welcome: Arc::new(welcome),
        };
        Ok(Resumed {
            client,
            events: events_rx,
        })
    }

    /// Sends `req` and waits for its response. Error responses become
    /// [`ClientError::Remote`]; a dropped connection fails the call with
    /// [`ClientError::Disconnected`].
    pub async fn call<T: DeserializeOwned>(&self, req: Request) -> Result<T, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = ControlMessage::request(id, &req)?.to_frame()?;
        let (reply, response) = oneshot::channel();
        {
            let mut pending = self.shared.pending();
            if pending.closed {
                return Err(ClientError::Disconnected);
            }
            pending.calls.insert(id, reply);
        }
        // Removes the entry if this future is dropped before the response arrives.
        let _entry = PendingEntry {
            shared: &self.shared,
            id,
        };
        self.outbox
            .send(frame)
            .await
            .map_err(|_| ClientError::Disconnected)?;
        let outcome = response.await.map_err(|_| ClientError::Disconnected)?;
        match outcome {
            Ok(value) => Ok(decode_result(value)?),
            Err(error) => Err(ClientError::Remote(error)),
        }
    }

    /// Daemon events, each with its seq. A receiver sees events delivered after it was created
    /// and lags (`RecvError::Lagged`) if it falls more than 1024 events behind; on lag, resync
    /// through a snapshot. It yields `RecvError::Closed` once the connection is gone.
    pub fn events(&self) -> broadcast::Receiver<(Seq, Event)> {
        match &*self.shared.events() {
            Some(sender) => sender.subscribe(),
            // Already disconnected: hand out a receiver that is closed from the start.
            None => broadcast::channel(1).1,
        }
    }

    /// Terminal render messages (snapshots and frames) for `id`. Bounded; a consumer that does
    /// not keep up loses messages, which the render protocol's snapshot/ack flow recovers.
    /// Replaces any earlier receiver for the same terminal.
    pub fn terminal_frames(&self, id: TerminalId) -> mpsc::Receiver<TerminalRender> {
        let (tx, rx) = mpsc::channel(TERMINAL_BUFFER);
        // On a closed connection the sender is dropped at once: the receiver ends immediately.
        if !self.shared.pending().closed {
            self.shared.terminals().insert(id, tx);
        }
        rx
    }

    /// Sends raw input bytes to a terminal (binary frame, no response).
    pub async fn send_input(&self, id: TerminalId, bytes: &[u8]) -> Result<(), ClientError> {
        for chunk in bytes.chunks(MAX_INPUT_CHUNK) {
            self.outbox
                .send(Frame::terminal_input(id, chunk))
                .await
                .map_err(|_| ClientError::Disconnected)?;
        }
        Ok(())
    }

    /// Sends `req` without waiting for (or keeping) its response; the daemon's answer is
    /// discarded by the read loop. Never blocks: a full outbox drops the request.
    pub(crate) fn notify(&self, req: &Request) -> Result<(), ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = ControlMessage::request(id, req)?.to_frame()?;
        match self.outbox.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("outbox full, dropping a fire-and-forget request");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ClientError::Disconnected),
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.welcome.epoch
    }

    /// The daemon's `welcome`: capabilities and whether a resume was replayed.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Seq of the last event received on this connection (the resume point for a reconnect).
    pub fn last_seq(&self) -> Seq {
        Seq(self.shared.last_seq.load(Ordering::Acquire))
    }
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn events(&self) -> MutexGuard<'_, Option<broadcast::Sender<(Seq, Event)>>> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn terminals(&self) -> MutexGuard<'_, HashMap<TerminalId, mpsc::Sender<TerminalRender>>> {
        self.terminals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn complete(&self, id: u64, outcome: CallOutcome) {
        if let Some(reply) = self.pending().calls.remove(&id) {
            let _ = reply.send(outcome);
        } else {
            debug!(id, "response for an unknown or abandoned call");
        }
    }

    fn on_event(&self, seq: Seq, event: &str, data: Value) {
        // Recorded first so a consumer woken by the broadcast sees `last_seq >= seq`.
        self.last_seq.fetch_max(seq.0, Ordering::AcqRel);
        match Event::decode(event, data) {
            Ok(Some(event)) => {
                // No receivers is fine.
                if let Some(sender) = &*self.events() {
                    let _ = sender.send((seq, event));
                }
            }
            Ok(None) => debug!(event, "ignoring unknown event"),
            Err(e) => warn!(event, error = %e, "undecodable event"),
        }
    }

    fn on_terminal(&self, id: TerminalId, render: TerminalRender) {
        let mut terminals = self.terminals();
        let Some(sink) = terminals.get(&id) else {
            return;
        };
        match sink.try_send(render) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!(terminal = %id, "render queue full, dropping")
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                terminals.remove(&id);
            }
        }
    }

    /// Fails every pending and future call.
    fn close(&self) {
        let mut pending = self.pending();
        pending.closed = true;
        // Dropping the senders wakes the callers with `Disconnected`.
        pending.calls.clear();
        drop(pending);
        self.terminals().clear();
        // Dropping the sender closes every events receiver.
        self.events().take();
    }
}

struct PendingEntry<'a> {
    shared: &'a Shared,
    id: u64,
}

impl Drop for PendingEntry<'_> {
    fn drop(&mut self) {
        self.shared.pending().calls.remove(&self.id);
    }
}

async fn handshake(
    reader: &mut FramedRead<OwnedReadHalf, FrameCodec>,
    writer: &mut FramedWrite<OwnedWriteHalf, FrameCodec>,
    hello: Hello,
) -> Result<Welcome, ClientError> {
    let frame = ControlMessage::request(0, &Request::Hello(hello))?.to_frame()?;
    writer.send(frame).await?;
    let frame = match reader.next().await {
        Some(frame) => frame?,
        None => return Err(ClientError::Disconnected),
    };
    if frame.kind != FrameKind::Control {
        return Err(ClientError::Handshake(
            "first frame was not a control frame".into(),
        ));
    }
    match ControlMessage::from_frame(&frame)? {
        ControlMessage::Response { id: 0, outcome } => match outcome {
            Ok(value) => Ok(decode_result(value)?),
            Err(error) => Err(ClientError::Remote(error)),
        },
        _ => Err(ClientError::Handshake(
            "first message was not the hello response".into(),
        )),
    }
}

async fn read_loop(mut reader: FramedRead<OwnedReadHalf, FrameCodec>, shared: Arc<Shared>) {
    while let Some(item) = reader.next().await {
        let frame = match item {
            Ok(frame) => frame,
            Err(e) => {
                debug!(error = %e, "connection read failed");
                break;
            }
        };
        match frame.kind {
            FrameKind::Control => match ControlMessage::from_frame(&frame) {
                Ok(ControlMessage::Response { id, outcome }) => shared.complete(id, outcome),
                Ok(ControlMessage::Event {
                    seq, event, data, ..
                }) => shared.on_event(seq, &event, data),
                Ok(ControlMessage::Request { method, .. }) => {
                    debug!(method, "ignoring request from daemon");
                }
                Err(e) => warn!(error = %e, "malformed control frame from daemon"),
            },
            FrameKind::TerminalRender => match frame.decode_terminal_render() {
                Ok((id, render)) => shared.on_terminal(id, render),
                Err(e) => warn!(error = %e, "undecodable terminal render frame"),
            },
            FrameKind::TerminalInput => debug!("ignoring terminal input frame from daemon"),
        }
    }
    shared.close();
}

async fn write_loop(
    mut writer: FramedWrite<OwnedWriteHalf, FrameCodec>,
    mut outbox: mpsc::Receiver<Frame>,
) {
    while let Some(frame) = outbox.recv().await {
        if let Err(e) = writer.send(frame).await {
            debug!(error = %e, "connection write failed");
            break;
        }
    }
}

fn is_unreachable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Limits autostart attempts to one per [`AUTOSTART_INTERVAL`]; used by reconnecting callers
/// that dial repeatedly.
#[derive(Debug, Default)]
pub(crate) struct AutostartThrottle {
    last: Option<Instant>,
}

impl AutostartThrottle {
    /// `true` (and starts a new interval) if an autostart is allowed now.
    pub(crate) fn permit(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < AUTOSTART_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// Dials `socket`. When it is unreachable and `autostart` is set, spawns `heelerd` if `permit`
/// agrees and waits up to [`AUTOSTART_WAIT`] for it; otherwise returns the dial error.
async fn dial(
    socket: &Path,
    autostart: Option<&Path>,
    socket_explicit: bool,
    permit: impl FnOnce() -> bool,
) -> Result<UnixStream, ClientError> {
    let first = match UnixStream::connect(socket).await {
        Ok(stream) => return Ok(stream),
        Err(e) => e,
    };
    let Some(binary) = autostart.filter(|_| is_unreachable(&first) && permit()) else {
        return Err(first.into());
    };
    spawn_daemon(binary, socket_explicit.then_some(socket))?;
    let connected = timeout(AUTOSTART_WAIT, async {
        loop {
            sleep(AUTOSTART_POLL).await;
            match UnixStream::connect(socket).await {
                Ok(stream) => return Ok(stream),
                Err(e) if is_unreachable(&e) => {}
                Err(e) => return Err(e),
            }
        }
    })
    .await
    .map_err(|_| ClientError::AutostartTimeout {
        socket: socket.to_owned(),
    })?;
    Ok(connected?)
}

/// Starts `heelerd` in its own process group with no stdio, so it outlives this process and
/// terminal signals.
fn spawn_daemon(binary: &Path, socket: Option<&Path>) -> Result<(), ClientError> {
    let mut command = tokio::process::Command::new(binary);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(false);
    if let Some(socket) = socket {
        command.env("HEELER_SOCKET", socket);
    }
    // The child is intentionally not awaited; tokio reaps it in the background if it exits.
    drop(command.spawn()?);
    Ok(())
}
