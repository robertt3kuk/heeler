# 0004. Terminal engine, PTY, and render stream model

- Status: accepted
- Date: 2026-10-13
- Milestone: M1/M2

## Context
The daemon owns canonical terminal state (ADR 0001). Clients must render immediately on attach and stay correct under any TUI. Spike `docs/spikes/2026-10-13-terminal-engine.md` compared engines on a ~200 MB corpus, a feature corpus, real zsh/vim in a PTY, and snapshot/restore:

| Engine | MB/s | RSS (10k lines) | Replies (DA/DSR/kitty) | Snapshot | Threading | Build |
|---|---:|---:|---|---|---|---|
| **alacritty_terminal 0.26.0** | 181.6 | 23.5 MiB | Yes (`Event::PtyWrite`) | Grid serde only; cursors skipped | Send + Sync | Pure Rust |
| libghostty-vt 0.2.2 | 68.3 | 4.4 MiB* | Yes | RenderState; VT formatter restore buggy; binary Snapshot API **not in 0.2.2** | !Send + !Sync | Needs Zig 0.15.2 (0.16 fails) + SDK 15.4 workaround |
| vt100 0.16.2 | 208.5 | 29.0 MiB | **No** | Partial, loses primary screen | Send + Sync | Pure Rust |

\* Ghostty's "max_scrollback" is a byte budget: only 787 mixed lines retained, not comparable.

VT-sequence replay (zmx approach) loses alternate/primary screen pairs and unfinished parser state on reattach — confirmed for both vt100 `state_formatted` and Ghostty's formatter.

## Options
| Option | Pros | Cons |
|---|---|---|
| Raw PTY bytes + client-side engine fed by VT replay snapshot | Low bandwidth; any VT widget can be a client | Double parsing; replay loses state (measured); engine version coupling |
| **Daemon is the only parser; clients receive render snapshots + damage deltas** | Exact state on attach; one parser; latest-state-wins backpressure; thin clients (GPUI, CLI, future mobile) | Own compact render schema; scrollback fetched lazily |

## Decision
- Engine: `alacritty_terminal = "=0.26.0"` with `Config { kitty_keyboard: true, scrolling_history: 10_000, .. }`. Engine types stay inside the `terminal` crate; nothing outside sees them.
- PTY: `portable-pty = "=0.9.0"`. Drop the slave handle right after spawn; dedicated blocking reader thread per PTY feeding the engine (Tokio side gets damage notifications); writes (input + engine replies) go through one writer per PTY; explicit reaping. Spawns serialized behind a lock to contain the non-atomic `FD_CLOEXEC` window.
- Synchronized output (DEC 2026): honour the engine's `sync_timeout()`/`stop_sync()` with the stock 150 ms deadline.
- OSC 133 / OSC 7 / OSC 0/2 / OSC 9/777 notifications: a small byte-stream sniffer in front of the engine extracts prompt marks, cwd, title, and notifications into session metadata (Alacritty ignores OSC 133).
- Stream model (refines ADR 0002 binary frames):
  - `terminal.attach` → `TerminalSnapshot` (owned DTO): size, cursor (pos, shape, visible), modes needed by clients (alt screen, mouse modes, bracketed paste, app cursor, kitty keyboard flags), palette overrides, viewport rows as styled runs (text, fg/bg, flags, hyperlink id), hyperlink table, title, `frame` number.
  - Updates: `TerminalFrame { frame, dirty rows as styled runs, cursor, mode changes, scroll delta }`, coalesced per client at most every ~8 ms and **only after the client acks the previous frame** (credit = 1–2 frames). Slow clients skip intermediate states; daemon memory per client stays O(screen).
  - Scrollback: `terminal.history { from_line, count }` query, fetched lazily by clients.
  - Input: `kind=2` frames carry already-encoded bytes; key encoding (incl. kitty protocol) lives in the client using the modes from the snapshot, so encoding stays correct per active mode.
  - Optional `terminal.tap_output` raw byte subscription for automation/logging (bounded, lossy with a gap marker).
- Watch item: re-evaluate libghostty-vt when a released Rust binding exposes the binary Snapshot API and a line-bounded scrollback contract.

## Consequences
- The CLI `attach` renders frames into the host terminal (tmux-style), no second engine.
- Render DTOs are versioned protocol types with golden fixtures; conformance corpus compares engine grids, not bytes.
- Engine swap later touches only the `terminal` crate.

## Evidence
`docs/spikes/2026-10-13-terminal-engine.md`
