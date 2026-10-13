# M1-DT1: M1 debt — terminal replies, exit signals, history cap, restart cwd, handshake timeout, agent TUI corpus

Agent: implementer   Depends on: master `b6e6867`   Branch: slice/M1-DT1

## Context
Read `AGENTS.md`, `docs/status.md` § Tech debt, `docs/protocol.md` (terminal + daemon semantics sections), ADR 0004, `docs/spikes/2026-10-13-terminal-engine.md` § Corpus and replies. Code: `crates/terminal` (`terminal.rs` `ExitStatus`, `emulator.rs`, `sniffer.rs`, tests/corpus), `crates/daemon` (`registry.rs` `signal_number`, `terminals.rs` history, `conn.rs` handshake).

## Scope
`crates/terminal`, `crates/daemon`, `docs/protocol.md` (semantics sections only). Do not edit `crates/core`, `crates/protocol` types, `crates/cli`, `docs/status.md`, `docs/roadmap.md`.

## Changes

1. **Exit signal as a number** (debt 2). `heeler_terminal::ExitStatus { code: Option<i32>, signal: Option<i32> }` — the raw signal number from the wait status (use `portable-pty`'s data or `libc` W* macros in `sys.rs`; keep `unsafe` confined there with `// SAFETY:`). Delete the daemon's `signal_number` name table and its tests; the daemon passes the number through. Test: `kill -TERM $$` inside a session → `session.exited { signal: Some(15) }`.

2. **Terminal query replies** (new; real TUIs such as Claude Code and Codex query these at startup and may stall or pick the wrong theme without answers). Answer, via the existing single PTY writer:
   - OSC 10 / 11 / 12 `?` (foreground / background / cursor color) → `ESC ] 1x ; rgb:RRRR/GGGG/BBBB` + the terminator the query used (BEL or ST). Value = palette override if the app set one, else the terminal's default colors from a new `TerminalConfig.default_colors { foreground, background, cursor }` (default: fg `#d4d4d4`, bg `#1e1e1e`, cursor `#d4d4d4`).
   - OSC 4 `;N;?` → `ESC ] 4 ; N ; rgb:…` from override or the standard xterm 256-color table.
   - CSI 18 t → `CSI 8 ; rows ; cols t`; CSI 14 t → `CSI 4 ; height_px ; width_px t`; CSI 16 t → `CSI 6 ; cell_h ; cell_w t` (from `TerminalSize` pixel fields; reply `0` when unknown).
   - Corpus cases asserting exact reply bytes for each, including BEL vs ST terminator echo and queries split across reads.

3. **History cap** (debt 3). `terminal.history` clamps `count` to 5 000 and stops adding rows once the encoded JSON size of the rows would exceed 2 MiB; the response may be shorter than requested and clients page with `from_line`. Keep the `internal` fallback for anything still oversized. Update `protocol.md` semantics. Test: request 100 000 lines on a 20 000-line history → ≤ 5 000 rows, connection stays open, paging covers the history.

4. **Restart cwd** (debt 5). `session.restart` spawns in the last cwd reported via OSC 7 when that directory still exists, else the spec cwd; `SessionInfo.cwd` reflects it. Test with `cd /tmp && printf '\033]7;file://localhost/private/tmp\007'`-style shell input (use the real path form your shell reports), exit, restart, `pwd` output.

5. **Handshake timeout** (new). A connection that does not send `hello` within 5 s (configurable in daemon options for tests) is closed; log at debug. Test with a raw socket that connects and stays silent while another client works normally.

6. **Agent TUI corpus** (M0.4 open item). With the recorder example, record startup screens (no prompt submitted, quit with the app's exit key or SIGHUP after ~3 s) of `omp` and `codex` at 100×30 into `crates/terminal/tests/corpus/recorded/` and add them to the replay test. Before committing, scan the `.raw`/`.grid` files and drop or redact anything that looks like an account email, token, or API key; if a recording cannot be made clean, skip it and say why. Note `htop` stays unrecorded if not installed.

## Acceptance
All new tests pass; `cargo nextest run -p heeler-terminal -p heeler-daemon` repeated 3× without flakes; `just check` green; committed on `slice/M1-DT1`.

## Out of scope
CLI changes, protocol type changes, `scrolled` detection, client theme reporting (M2.4).

## Report
Summary, files changed, commands run with results, deviations, open questions. Do not edit `docs/status.md` or `docs/roadmap.md`.
