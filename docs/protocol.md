# Heeler IPC Protocol — v0.3

Normative decisions: ADR 0002 (transport, framing, sequencing) and ADR 0004 (terminal render stream). This file is the message catalog; the `protocol` crate (`PROTOCOL_VERSION`) is the source of truth and this document must match it.

Version history: 0.1 — M1 (sessions, terminals, attach). 0.2 — `terminal.set_colors` (capability `terminal.colors`). 0.3 — durable sessions (capability `session.restore`): `SessionInfo.restored`, `daemon.shutdown { forget }`.

## Frames

| kind | Direction | Payload |
|---|---|---|
| 0 | both | UTF-8 JSON envelope |
| 1 | daemon → client | `u64 terminal_id` + postcard `TerminalRender` (`Snapshot` or `Frame`) |
| 2 | client → daemon | `u64 terminal_id` + raw input bytes |

Header: `u32 length (big-endian, excludes header) | u8 kind`. Max payload 4 MiB.

## Envelope

```json
{"id": 1, "method": "session.create", "params": {"workspace_id": "01J…", "kind": "terminal", "command": ["omp"], "cwd": "/repo"}}
{"id": 1, "result": {"session_id": "01J…", "terminal_id": 7}}
{"id": 2, "error": {"code": "not_found", "message": "session 01J… not found"}}
{"event": "session.updated", "epoch": "6f1c…", "seq": 1042, "data": {"session": {…}}}
```

## Handshake

```json
{"id": 0, "method": "hello", "params": {"protocol": {"major": 0, "minor": 1}, "client": {"kind": "desktop", "version": "0.1.0"}, "capabilities": ["terminal.render.v1"], "resume": {"epoch": "6f1c…", "seq": 1042}}}
{"id": 0, "result": {"protocol": {"major": 0, "minor": 1}, "daemon_version": "0.1.0", "epoch": "6f1c…", "capabilities": ["terminal.render.v1"], "resume": "replayed"}}
```

`resume` is `replayed` (events after `seq` follow immediately) or `snapshot_required`.

## Methods

Milestone tag = when the method lands.

| Method | Kind | Params → Result | M |
|---|---|---|---|
| `hello` | handshake | see above | M1 |
| `daemon.status` | query | → version, uptime, epoch, counts | M1 |
| `daemon.shutdown` | command | `{stop_sessions: bool, forget?: bool}`; `forget` (capability `session.restore`) drops the stopped sessions from the restore set | M1 |
| `events.subscribe` | command | `{topics?: [...]}` → current `seq` | M1 |
| `workspace.create` / `.list` / `.rename` / `.close` | cmd/query | name, root paths | M1 |
| `session.create` | command | workspace, kind, command/cwd/env or agent/provider config → ids | M1 |
| `session.list` | query | filters → sessions + `seq` | M1 |
| `session.snapshot` | query | → workspaces, sessions, pending approvals + `seq` | M1 |
| `session.stop` / `.restart` / `.rename` | command | session id | M1 |
| `terminal.attach` | command | terminal id, size → `TerminalSnapshot` via kind=1 | M1 |
| `terminal.detach` | command | terminal id | M1 |
| `terminal.resize` | command | terminal id, cols, rows, px size | M1 |
| `terminal.ack` | command | terminal id, frame | M1 |
| `terminal.history` | query | terminal id, from_line, count → styled rows | M1 |
| `terminal.set_colors` | command | terminal id, `{foreground, background, cursor}` RGB → `{}`; daemon answers OSC 10/11/12 with them; last writer wins; capability `terminal.colors` | M2 |
| `terminal.tap_output` | command | raw byte subscription (lossy, gap markers) | M7 |
| `chat.send` | command | session id, content blocks | M4 |
| `chat.cancel` | command | session id, turn id | M4 |
| `conversation.read` | query | session id, range → turns/items | M4 |
| `approval.decide` | command | approval id, option id (must be one offered) | M4 |
| `provider.list` / `.configure` / `.set_credential` / `.models` | cmd/query | provider config; secret write-only | M5 |
| `notification.ack` | command | intent id, action | M6 |
| `task.create` / `.list` / `.update` | cmd/query | task graph ops | M7 |
| `worktree.create` / `.list` / `.remove` | cmd/query | repo, branch, base | M7 |

## Events

| Event | Data | M |
|---|---|---|
| `workspace.updated` / `workspace.closed` | workspace | M1 |
| `session.updated` | session (state, title, cwd, attention, `restored`) | M1 |
| `session.exited` | session id, exit code/signal | M1 |
| `terminal.bell` / `terminal.title` / `terminal.notification` | from OSC sniffer | M1 |
| `chat.delta` | session, turn, item, text/blocks | M4 |
| `agent.item` | `AgentEvent` (tool call, diff, plan, usage, …) | M4 |
| `agent.state_changed` | attention state | M4 |
| `turn.completed` | session, turn, status, usage | M4 |
| `approval.required` / `approval.resolved` | approval with offered options / decision | M4 |
| `notification.intent` | id, kind, title, body, actions, target | M6 |
| `task.updated` / `task.completed` | task | M7 |

## Terminal render types (ADR 0004)

```text
TerminalSnapshot { frame, cols, rows, cursor, modes, palette, title?, lines: [Row; rows], links: [Link], history_len }
TerminalFrame    { frame, scrolled: i32, dirty: [(row, Row)], cursor, modes?, title??, links: [Link], history_len }
Row              { runs: [Run], wrapped }
Run              { text, fg: Color, bg: Color, flags: CellFlags, underline_color: Color?, link: u32? }
Color            default | indexed(u8) | rgb(u8, u8, u8)
CellFlags        bold, dim, italic, underline, double_underline, undercurl, dotted_underline, dashed_underline, inverse, hidden, strikethrough, wide
Cursor           { col, row, shape: block|underline|bar|hollow_block, visible, blinking }
Modes            { alt_screen, app_cursor, app_keypad, bracketed_paste, mouse: none|click|drag|motion, mouse_sgr, focus_events, alternate_scroll, kitty_keyboard: u8 }
Palette          { indexed: [(u8, rgb)], foreground?, background?, cursor? }   # OSC 4/10/11/12 overrides only
```

Semantics produced by the `terminal` crate (what clients can rely on):
- Rows list runs left to right; a wide character is one run character and spans two columns (spacer cells omitted), combining characters are appended to their base. Trailing default-style blanks are trimmed, so a row's runs may cover fewer columns than `cols`; the rest is blank.
- `Run.link` is an index into the terminal's link table (`Link.id == index`, stable for the terminal's lifetime, capped at 16 384 entries; later links render unlinked). A snapshot carries the whole table, a frame the entries added since the last snapshot/frame sent to that client.
- `scrolled` is always 0: a scrolling viewport is reported as the rows that differ.
- Frames carry neither dimensions nor palette. After a resize or a palette change (OSC 4/10/11/12) the daemon sends a fresh `TerminalSnapshot` instead of a frame (`Terminal::render`).
- `terminal.history`: `from_line` 0 is the line directly above the viewport, rows are newest first; the alternate screen has no history (`history_len` = 0 while it is active).
- Scrollback storage (ADR 0013, no wire change): the daemon keeps the newest ≤ 256 history rows in the engine and older ones in a compact store; `terminal.history` reads across both with the semantics above, and the retained history is exactly `scrollback_lines` rows. Consequences clients may observe: after a resize only rows within the newest ~256 history rows are reflowed to the new width — older history rows keep the width they had when they scrolled out (a row of any width is valid; paint it as is, `wrapped` still marks soft wraps); `ESC[3J` and `ESC c` erase all of it; link ids of history rows are assigned when a row leaves that window, not when it is first read (ids stay stable, and the link table in a snapshot may therefore list links of history rows no client has fetched).
- Terminal queries answered by the daemon (through the terminal's single PTY writer, in query order, echoing the terminator the query used): OSC 10/11/12 `?` → `OSC 1x ; rgb:RRRR/GGGG/BBBB` (the application's override, else the configured default colours: foreground `#d4d4d4`, background `#1e1e1e`, cursor `#d4d4d4`); OSC 4 `;N;?` → override, else the standard xterm 256-colour table; CSI 18 t → `CSI 8 ; rows ; cols t`; CSI 14 t → `CSI 4 ; height_px ; width_px t`; CSI 16 t → `CSI 6 ; cell_h ; cell_w t` (pixel values from the last `TerminalSize`, `0` when unknown). Clients do not answer these queries.

Semantics implemented by the daemon (M1-D1; `crates/daemon/src/{registry,terminals}.rs`):
- `session.create`, `session.stop`, `.restart` and `.rename` answer `SessionResult { session }`; `create` does so even when the spawn failed (`state: failed`, reason inside). `stop` answers once the session is `stopped` (SIGHUP, SIGKILL after 3 s); `restart` is only valid from `exited`/`failed`/`stopped` and yields a new `terminal_id`. Process exit publishes `session.updated` and then `session.exited`.
- `terminal.attach` queues the snapshot frame before its response. Credit is one frame: after any render (the snapshot included) the daemon renders again only after `terminal.ack { frame }` for exactly that frame, at most once per ~8 ms, and never queues a render while credit is 0. Attaching twice from one connection replaces the first attachment; `terminal.detach`, closing the connection, and `session.restart` drop attachments.
- Terminals of ended sessions stay attachable (final screen) until restart or workspace close. Unknown `terminal_id` → `not_found` (acks for a missing attachment are ignored). Input (kind=2) for unknown or ended terminals is dropped; a full per-terminal input queue stalls only the sending connection's reader.
- `terminal.history` answers at most 5 000 rows per call (`count` is clamped) and stops adding rows once the JSON encoding of the rows would exceed 2 MiB (the first row is always included). The response may be shorter than requested although more history exists: page with `from_line += rows.len()` until `rows` is empty. Any response that still would not fit in one frame becomes an `internal` error instead of closing the connection.
- `session.exited` carries the raw signal number (e.g. 15 for SIGTERM) in `signal`, `code` is absent for a signal death.
- `session.restart` starts the process in the last directory the program reported with OSC 7 when that directory still exists, else in the spec `cwd`; `SessionInfo.cwd` reflects the directory used.
- A connection that has not sent `hello` within 5 s of being accepted is closed (logged at debug); the timeout covers the handshake only.
- `workspace.close`: `default` and workspaces with live sessions → `invalid_state`; ended sessions leave with their workspace. `daemon.shutdown { stop_sessions: false }` with live sessions → `invalid_state`; every shutdown (request, SIGTERM, SIGINT) hangs up all sessions concurrently and reaps them before the socket is removed.

Persistence and restore (M3-A; capability `session.restore`, protocol 0.3; `crates/daemon/src/{registry,restore,config}.rs`, `crates/storage`):
- Workspaces and sessions are written through to SQLite (`$HEELER_HOME/heeler.db`) on every change: workspace create/rename/close, session create/rename/stop/restart/exit. Title and cwd changes (OSC 0/2, OSC 7) are coalesced: a session is rewritten at most once per second because of them. Writes go to the storage writer thread and never block request handling.
- At start the daemon loads the stored state before it listens, under the same ids: workspaces (and which one is `default`), sessions with title, workspace, creation time. A new epoch begins; clients treat it as "everything may have restarted" and re-snapshot.
- Sessions that were live when the previous daemon stopped (or died) are spawned again as **new terminals** (new `terminal_id`, same `SessionId`) in their last known cwd (falling back to the spec `cwd`, then the shell default, when the directory is gone): `starting` → `running`, title and workspace kept, `restored: false`. A stored `resume_ref` (`{"argv": [...]}` until the agent integrations of M3.3 define more) replaces the spec command for this spawn; a restart afterwards uses the spec again. A stored session whose spec cannot be read comes back `failed`.
- Sessions that had ended (`exited`, `failed`, `stopped`) stay listed in that state with `restored: true`: the process and its final screen are gone, so there is no terminal to attach (`terminal_id` is absent). `session.restart` yields a live, unrestored session with the same id. `restored` is `false` and omitted from the wire for every other session.
- `daemon.toml` in the daemon's home directory: `[daemon] restore_sessions = "respawn" | "stopped"` (default `respawn`). `stopped` lists sessions that were live as `stopped` + `restored` instead of spawning them; the file is optional, an unreadable or invalid one stops the daemon from starting.
- Stopping the daemon does not forget its sessions. `daemon.shutdown { stop_sessions: true }` (and SIGTERM/SIGINT) hang the sessions up as always but leave them in the restore set, so the next daemon respawns them; clients still see them become `stopped` before the connection closes. `forget: true` (`heeler daemon stop --stop-sessions --forget`) records them as stopped instead, so the next start lists them as ended and does not respawn them. Sessions the user stopped or that exited earlier were never in the restore set.
- Storage is mandatory: a database that cannot be opened or migrated (including one written by a newer Heeler) stops the daemon with an error naming the file; the file is left unchanged. Before migrating an existing database the daemon copies it to `backups/heeler-<unix ms>-v<version>.db` (newest 5 kept). Environment variables in a session spec whose names look like secrets (`*TOKEN*`, `*SECRET*`, `*PASSWORD*`, `*API_KEY*`, …) are not stored; restored sessions start without them.

## Error codes
`bad_request`, `unsupported_version`, `unauthorized`, `not_found`, `conflict`, `invalid_state`, `option_not_offered`, `slow_consumer`, `provider_error`, `internal`.

## Compatibility rules
- Unknown fields are ignored; unknown events are ignored; unknown methods → `bad_request`.
- Additive change = minor bump + capability string; breaking = major bump + ADR.
- Golden fixtures for every message live in `crates/protocol/tests/fixtures/v{major}/`.
