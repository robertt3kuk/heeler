# M1-A: `hutand` lifecycle, connection layer, `client` crate (roadmap M1.1 + M1.2)

Agent: implementer   Depends on: M0 (commit 6c99867)   Branch: slice/M1-A

## Context
Read `AGENTS.md`, then `docs/architecture.md` (§ Daemon internals, § Paths, § Security), `docs/protocol.md`, ADR 0001, ADR 0002, ADR 0009. Reuse `crates/core` and `crates/protocol` as they are: `FrameCodec`, `Frame`, `ControlMessage`, `Request::decode`, `Event`, `messages::*`, `PROTOCOL_VERSION`.

## Scope
Create `crates/daemon` (package `hutan-daemon`, binary `hutand`, plus a lib target for tests) and `crates/client` (package `hutan-client`). Add needed deps to root `[workspace.dependencies]` (tokio with `rt-multi-thread, net, io-util, sync, time, macros, signal, process`, tokio-util, futures, tracing, tracing-subscriber, anyhow in binaries only, `nix` or `libc` for `getpeereid`/flock). Do **not** edit `crates/protocol` or `crates/core`; if a protocol change seems necessary, stop and describe it in the report.

## Contract

### Paths (`daemon::paths`)
`HUTAN_HOME` overrides the base dir; default `~/Library/Application Support/Hutan/`. Base dir created `0700`. Socket `hutand.sock` (`0600`), lock `hutand.lock` (exclusive `flock`, second daemon exits with a clear error and code 2), logs `~/Library/Logs/Hutan/hutand.log` or `$HUTAN_HOME/logs/` when overridden. `HUTAN_SOCKET` overrides only the socket path. Stale socket file from a dead daemon is removed after the lock is acquired.

### Daemon behavior
- Accepts UDS connections; rejects peers whose UID ≠ daemon UID (`getpeereid`), closing the socket.
- First frame must be `hello`; major mismatch → `unsupported_version` error then close; otherwise `Welcome { epoch, capabilities: ["terminal.render.v1"], resume }`. `resume = replayed` iff `hello.resume.epoch == current epoch` and every event after `resume.seq` is still in the ring; replayed events are sent right after the welcome response and the connection is then subscribed (no separate `events.subscribe` needed).
- Event bus: one ring per epoch (bounded by count **and** bytes, configurable; defaults 10 000 events / 8 MiB) with strictly increasing `Seq`. `events.subscribe` → `Subscribed { seq: last }`, then the connection receives every subsequent event.
- Per-connection outbound queue bounded (default 1 024 control messages). Overflow → log `slow_consumer` with the client info and close that connection (the client reconnects via resume/snapshot later); the daemon and other clients are unaffected. Never block the event bus on one client.
- Methods in this slice: `hello`, `daemon.status`, `daemon.shutdown`, `events.subscribe`, `workspace.create|list|rename|close` (in-memory registry; a default workspace named `default` exists at start), `session.snapshot` (workspaces + empty sessions + current seq). Workspace mutations emit `workspace.updated` / `workspace.closed`. Every other method → `bad_request` for unknown methods, or `invalid_state` with message "not available in this build" for known-but-unimplemented session/terminal methods.
- `daemon.shutdown { stop_sessions }`: replies, then exits cleanly (removes socket, releases lock). SIGTERM/SIGINT do the same.
- Binary frames from clients (kind 1/2) are accepted and ignored with a debug log in this slice.
- Structure for later slices: a `Router` that dispatches typed `Request`s to services, a `SessionRegistry` actor stub that M1.5 will extend (keep the actor/channel shape from architecture.md; no unbounded channels).

### `hutan-client`
```rust
pub struct ConnectOptions { pub socket: Option<PathBuf>, pub autostart: Option<PathBuf /* hutand path */>, pub client: ClientInfo }
pub struct Client { /* cloneable handle */ }
impl Client {
    pub async fn connect(opts: ConnectOptions) -> Result<Self, ClientError>; // spawns hutand detached if socket absent and autostart is set, waits ≤ 3 s
    pub async fn call<T: DeserializeOwned>(&self, req: Request) -> Result<T, ClientError>; // maps error responses to ClientError::Remote(ErrorBody)
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<(Seq, Event)>;
    pub fn terminal_frames(&self, id: TerminalId) -> tokio::sync::mpsc::Receiver<TerminalRender>; // bounded; used from M1.4
    pub async fn send_input(&self, id: TerminalId, bytes: &[u8]) -> Result<(), ClientError>;
    pub fn epoch(&self) -> Epoch;
}
```
Request ids are per-connection counters; responses are matched by id; a dropped connection fails all pending calls with `ClientError::Disconnected`. Reconnect is **not** in this slice (M2.2), but keep `last_seq` tracked for it.

## Acceptance
- Integration tests in `crates/daemon/tests/` that start the daemon in-process (lib API) on a temp `HUTAN_HOME` and talk to it with `hutan-client`:
  1. handshake + `daemon.status` returns this epoch/pid;
  2. incompatible major → `unsupported_version`;
  3. workspace create/rename/close emits events in seq order to a subscriber;
  4. reconnect with `resume` in the same epoch replays exactly the missed events; with an unknown epoch → `snapshot_required`;
  5. a client that never reads is disconnected after its queue fills while another client keeps receiving events;
  6. second daemon on the same `HUTAN_HOME` fails to start;
  7. `daemon.shutdown` removes the socket.
- `just check` green. No `unwrap/expect` outside tests.
- Manual: `HUTAN_HOME=/tmp/hh cargo run -p hutan-daemon --bin hutand` starts and logs the socket path.

## Out of scope
Sessions, PTYs, terminals (M1-B), CLI (M1-C), persistence (M3), reconnect logic in client (M2.2), launchd.

## Report
Summary, files changed, commands run with results, open questions. Commit on `slice/M1-A` (conventional message, scope `daemon,client`). Do not edit `docs/status.md` or `docs/roadmap.md`.
