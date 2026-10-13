# Heeler IPC Protocol — v0 (draft)

Normative decisions: ADR 0002 (transport, framing, sequencing) and ADR 0004 (terminal render stream). This file is the message catalog; the `protocol` crate is the source of truth once it exists, and this document must match it.

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
| `daemon.shutdown` | command | `{stop_sessions: bool}` | M1 |
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
| `session.updated` | session (state, title, cwd, attention) | M1 |
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

## Error codes
`bad_request`, `unsupported_version`, `unauthorized`, `not_found`, `conflict`, `invalid_state`, `option_not_offered`, `slow_consumer`, `provider_error`, `internal`.

## Compatibility rules
- Unknown fields are ignored; unknown events are ignored; unknown methods → `bad_request`.
- Additive change = minor bump + capability string; breaking = major bump + ADR.
- Golden fixtures for every message live in `crates/protocol/tests/fixtures/v{major}/`.
