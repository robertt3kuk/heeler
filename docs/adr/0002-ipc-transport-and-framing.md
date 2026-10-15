# 0002. IPC transport, framing, sequencing

- Status: accepted
- Date: 2026-10-13
- Milestone: M1

## Context
Clients need: request/response commands and queries, a live event stream, high-volume terminal bytes, reconnect without losing or duplicating state, bounded memory with slow clients, version negotiation. Local first; remote later. Herdr's lesson: events invalidate caches and cannot be blindly replayed over an arbitrary snapshot — snapshot and stream must share one sequence watermark.

## Options
| Option | Pros | Cons |
|---|---|---|
| gRPC/tonic over UDS | Codegen, streaming | Protobuf toolchain, awkward for raw terminal bytes, heavier |
| Pure JSON-RPC lines | Debuggable, simple | Base64 bloat for terminal bytes, no binary path |
| **Length-prefixed frames: JSON control + binary data** | Debuggable control, zero-copy terminal bytes, one connection | Custom framing (small) |

## Decision
**Transport**: Unix domain socket `~/Library/Application Support/Hutan/hutand.sock` (dir `0700`, socket `0600`), override `HUTAN_SOCKET`. Daemon verifies peer UID with `getpeereid`. A single-instance lock file next to it prevents two daemons.

**Frame**: `u32 length (BE) | u8 kind | payload`. Max frame 4 MiB.
- `kind=0` control: UTF-8 JSON envelope.
- `kind=1` terminal render (daemon → client): `u64 terminal_id | postcard-encoded TerminalSnapshot | TerminalFrame` (ADR 0004).
- `kind=2` terminal input (client → daemon): `u64 terminal_id | bytes`.

**Envelope** (JSON, control frames):
- Request `{"id": 7, "method": "session.create", "params": {…}}` → response `{"id": 7, "result": {…}}` or `{"id": 7, "error": {"code": "…", "message": "…", "data": {…}}}`.
- Event `{"event": "session.updated", "epoch": "<daemon-boot-uuid>", "seq": 1042, "data": {…}}`.

**Handshake**: first frame `hello {protocol: {major, minor}, client: {kind, version}, capabilities: [...], resume: {epoch, seq}?}` → `welcome {protocol, daemon_version, epoch, capabilities, resume: "replayed"|"snapshot_required"}`. Major mismatch → error and close. Capabilities gate additive features.

**Sequencing and replay**: one daemon-wide event log with monotonically increasing `seq` per `epoch` (new epoch per daemon start). Retained in a bounded in-memory ring (size by count and bytes). On reconnect with a known epoch and `seq` still in the ring → replay, else client must fetch snapshots. Every snapshot response carries the `seq` it reflects; clients subscribe first, then snapshot, then apply only events with `seq > snapshot.seq`.

**Terminal stream**: separate from the event log. `terminal.attach` returns a render snapshot at frame `N`; render frames follow, coalesced and sent only after the client acks the previous frame (latest state wins). Slow clients skip intermediate frames instead of queueing them; the PTY reader is never blocked by a client. Scrollback is fetched with `terminal.history`. Details: ADR 0004.

**Backpressure**: one bounded outbound queue per connection (default 1 024 messages) carries control messages and render frames. Render frames cannot fill it: each attachment holds at most one unacked render (credit 1), so render traffic is bounded by the number of attachments. Overflow therefore comes only from control traffic (events, responses) and closes that connection (`slow_consumer` in the daemon log); the client reconnects through replay/snapshot.

*Amended 2026-10-13 (M1 debt):* the original text required separate control and data queues. Credit-1 pacing made the split unnecessary; the M1 flood bench showed `daemon.status` p99 0.11 ms during a 193 MiB flood with one acking and one silent client sharing the daemon. Revisit if a client type without ack pacing (e.g. raw output taps, M7) is added.

**Schema**: Rust types in `protocol` crate are the source; JSON Schema generated (`schemars`) and committed per version for external clients; golden fixtures guard compatibility.

## Consequences
- CLI and tests can speak the protocol with a few hundred lines; `hutan api` can expose raw calls for debugging.
- A remote transport later wraps the same frames in TLS/WebSocket.
- Terminal snapshot/frame format and flow control are defined by ADR 0004.

## Evidence
- Herdr socket API snapshot/subscription semantics: <https://raw.githubusercontent.com/herdrdev/herdr/v0.9.3/docs/next/website/src/content/docs/socket-api.mdx>
- macOS `sun_path` limit is 104 bytes; the chosen path is ~60 bytes for typical usernames.
