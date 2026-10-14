# M2-A: `client::Connection` — reconnecting client, state store, durable terminal handles (roadmap M2.2)

Agent: implementer   Depends on: master (M1 + debt)   Branch: slice/M2-A

## Context
Read `AGENTS.md`, ADR 0002 (handshake, resume, epoch/seq, snapshot rule: subscribe first, snapshot, apply events with `seq > snapshot.seq`), ADR 0009, `docs/protocol.md` (all semantics sections), `crates/client/src/*` (existing single-connection `Client`: `connect`, `connect_resuming`, `call`, `events`, `terminal_frames`, `send_input`, `epoch`, `last_seq`), `crates/daemon/tests/common/mod.rs` (test helpers you can copy patterns from).

## Scope
`crates/client` only (new modules + tests; `heeler-daemon` already usable as a dev-dependency). Do not edit `core`, `protocol`, `daemon`, `cli`. Keep `Client` working for the CLI.

## Contract
GPUI drives its own executor, not Tokio. `Connection` must therefore own a Tokio runtime (dedicated threads, named `heeler-client*`) and every public method must be callable / awaitable from **any** executor or plain thread.

```rust
pub struct Connection { /* Clone, Send + Sync */ }

impl Connection {
    /// Returns immediately; connects (autostarting heelerd per opts) and keeps reconnecting
    /// with exponential backoff (100 ms doubling to 5 s, ±20 % jitter) until dropped.
    pub fn start(opts: ConnectOptions) -> Connection;
    pub fn state(&self) -> tokio::sync::watch::Receiver<Arc<StoreState>>;   // latest full state, coalesced
    pub fn notices(&self) -> tokio::sync::broadcast::Receiver<Notice>;      // transient events
    pub async fn call<T: DeserializeOwned + Send + 'static>(&self, req: Request) -> Result<T, ClientError>; // Disconnected if not connected; never queued across reconnects
    pub fn attach(&self, terminal: TerminalId, size: Option<TerminalSize>) -> TerminalHandle;
    pub fn shutdown(self);   // stop reconnecting, close connection, join runtime threads
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoreState {
    pub connection: ConnectionState,
    pub epoch: Option<Epoch>,
    pub workspaces: Vec<WorkspaceInfo>,              // `default` first, then creation order
    pub sessions: Vec<SessionInfo>,                  // creation order (created_at_ms, then id)
}
pub enum ConnectionState { Connecting, Connected, Reconnecting { attempt: u32, retry_in: Duration }, Stopped }
pub enum Notice {
    DaemonRestarted { old: Epoch, new: Epoch },      // epoch changed: all terminals gone
    SessionExited { session: SessionId, code: Option<i32>, signal: Option<i32> },
    Bell { terminal: TerminalId },
    TerminalNotification { terminal: TerminalId, title: Option<String>, body: String },
}

pub struct TerminalHandle { /* Send; Drop = detach */ }
impl TerminalHandle {
    pub fn terminal(&self) -> TerminalId;
    /// Bounded (≤ 4). Every (re)attach starts with `Render(Snapshot)`.
    pub fn updates(&mut self) -> &mut tokio::sync::mpsc::Receiver<TerminalUpdate>;
    pub fn ack(&self, frame: u64);                   // fire-and-forget; ignored while disconnected
    pub fn input(&self, bytes: Vec<u8>);             // fire-and-forget; dropped while disconnected (log)
    pub fn resize(&self, size: TerminalSize);        // remembered; re-sent with the next re-attach
}
pub enum TerminalUpdate { Render(TerminalRender), Reconnecting, Gone { reason: GoneReason } }
pub enum GoneReason { EpochChanged, NotFound, SessionRestarted { new_terminal: TerminalId } }
```

Behavior:
- Handshake with `resume` = last applied `(epoch, seq)`. `replayed` → apply replayed events. `snapshot_required` → `events.subscribe`, then `session.snapshot`, buffer events that arrive meanwhile and apply only those with `seq > snapshot.seq`. Epoch change → reset state, `Notice::DaemonRestarted`, every handle gets `Gone(EpochChanged)`.
- Events fold into `StoreState`: workspace updated/closed, session updated (upsert), session exited (+ notice), terminal bell/notification (notices only). Unknown events ignored.
- Terminal handles: attach on creation when connected; on reconnect within the same epoch re-attach automatically (send `Reconnecting` first, then the new snapshot arrives as `Render(Snapshot)`); ack pacing is the caller's job (credit 1, see protocol.md). When the session restarts (`session.updated` shows a different `terminal_id` for the session that owned this terminal) emit `Gone(SessionRestarted { new_terminal })`.
- Autostart: on first connect and on every reconnect attempt when the socket is absent and `opts.autostart` is set — at most once per 10 s.
- No unbounded channels; no `unwrap/expect` outside tests.

## Acceptance
Tests over a real in-process daemon. Put a tiny UDS proxy between client and daemon so tests can sever connections without restarting the daemon:
1. `start` from a plain `std::thread` (no Tokio) and from `futures::executor::block_on`: state reaches `Connected`, `call(daemon.status)` works.
2. Sever via proxy → `Reconnecting` → `Connected`; events published while disconnected (create a workspace through a second client) appear in `state()` via replay; no duplicates.
3. Same as 2 but the ring has evicted the missed events (small ring in daemon config) → snapshot path; state equals a fresh snapshot.
4. Restart the daemon on the same `HEELER_HOME` → `DaemonRestarted`, state reset to the new daemon's view, terminal handles get `Gone(EpochChanged)`.
5. Terminal handle survives a sever: after reconnect a new snapshot arrives; `input` then reaches the PTY (verify through `terminal.history` or the next render).
6. Session restart → `Gone(SessionRestarted { new_terminal })`.
7. `shutdown` joins all threads (no leaked `heeler-client*` threads; check by name).
`just check` green.

## Out of scope
GPUI code, UI, CLI migration to `Connection`, persistence.

## Report
Summary, files changed, commands run with results, deviations, open questions. Commit on `slice/M2-A` (scope `client`).
