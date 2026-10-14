# Status (handoff log)

Newest entry first. Every session ends by updating **Current** and adding a log entry (see `process.md`).

## Current
- **Milestone**: M2 — GPUI workbench and terminal client (M0, M1 complete). Design: `docs/design.md`, ADR 0010.
- **Active slice**: wave 2 running: M2-E1 (`../heeler-wt/M2-E1`, desktop integration: live sessions, panels, settings wiring + UI) and M2-E2 (`../heeler-wt/M2-E2`, `terminal.set_colors` in terminal/daemon/client). Wave 1 merged.
- **Next action**: review/merge M2-E2 then M2-E1; visual check of the integrated app (omp in a pane, splits, panel moves, presets, overview previews, live settings); then M2-F (performance budgets + `docs/demos/M2.md`).
- **Blockers**: none. Antigravity (`agy`) was dropped from the process: account not eligible ("Eligibility check failed"); can return once the account works.
- **Open questions** (decide during M0/M1, not blocking):
  1. Project license (affects porting components from Zed's GPL crates; ADR 0003).
  2. Distribution: notarized Developer ID vs ad-hoc only for personal use (affects ADR 0007, Keychain prompts).
  3. ~~Default detach key~~ → decided: `ctrl+\` then `d` (M1-C).
  4. Product name: "Heeler" is already used by an iOS console for herdr (heeler.bybee.dev) and by heeler.com (AppSec). Keep as codename or rename before any public release.
- **M1 tech debt**: all 9 items closed (2026-10-13, see log). Accepted, not scheduled: `scrolled` always 0 (full dirty rows on scroll; fine locally, revisit for remote clients M9); new link ids from `terminal.history` arrive with the next frame; `SessionInfo.state` nests as `{"state":{"state":…}}` on the wire (fix with the next breaking protocol bump); daemon scrollback fixed at 10 000 lines (no config knob yet).

## Environment notes
- macOS 26.6.2 arm64, Rust 1.98.0, Xcode installed **without** separate Metal compiler → GPUI needs `runtime_shaders` (ADR 0003).
- Installed Zig 0.16.0 cannot build libghostty-vt 0.2.2 (needs 0.15.2) — not used (ADR 0004).
- Codex CLI 0.144.2 rejects the configured default model `gpt-6-astra` ("requires a newer version of Codex"); `gpt-5.6-sol` from `model/list` works. Upgrade Codex before M4, and pick models from `model/list` in the adapter.
- Installed agents: `codex` 0.144.2, `codex-acp` 1.12.0, `omp` 18.10.3 (`omp acp`), `opencode` 2.0.14; no Claude Code ACP adapter.
- Antigravity CLI `agy` 1.3.4 is installed but unusable (account not eligible); not part of the process.
- Gate tools: just 1.58.0, cargo-deny 0.20.2, cargo-nextest 0.9.148 (Homebrew).
### 2026-10-14 — M2 design + wave 1 merged; protocol 0.2
- Design with the user: layout A + overview board + ⌘K, native chrome + own terminal themes, density setting, one session with Chat/Terminal switch, **everything movable and configurable** → `docs/design.md`, ADR 0010 (workbench: docks + pane tree + panel registry + presets; layered TOML settings; GPUI only in UI crates). Vision/M7: task placement main checkout or worktree.
- Wave 1: `client::Connection` (`3f94df0`), `settings` (`ecf304b`), `terminal_view` (`fa7babc`), `desktop` workbench core (`6c2c2d9`, fixes `75d9c55`: exact overview round-trip, readable tinted sidebar). `just check` 484 tests green.
- Orchestrator: golden fixtures canonicalized (GPUI turns on `serde_json/preserve_order` workspace-wide); `deny.toml` gained MPL-2.0/CC0, zed font-kit + proptest git sources, 3 unmaintained-advisory ignores (all transitive via GPUI); protocol 0.2 `terminal.set_colors`.
- Visual checks (orchestrator screenshots via `screencapture -l`): workbench default layout matches the design (titlebar, sessions/inspector docks, status bar); sidebar contrast fixed; `terminal_view` renders OMP v18.10.3 correctly (truecolor gradient, block glyphs, emoji, status line), live input incl. Cyrillic lands on the grid.
- Known gaps (GPUI): no left/right Option distinction (option-as-meta applies to both), no keypad distinction, no system accent API (fixed blue). Client limitation: one attachment per terminal per connection → desktop shares one terminal model across views.


## Log

### 2026-10-13 — M1 tech debt paid
- DT2 (`c3d556a`): CLI tests locate a prebuilt `heelerd` (fail fast with a build hint, no nested `cargo build`), one shared test harness; `just test` builds `--bins` first; `heeler attach` on an ended session shows the final screen and exits 0 with the end line.
- DT1 (`f217b14`): `ExitStatus.signal` is the numeric signal (terminal reaps via `waitpid` in `sys.rs`, exactly once); replies to OSC 4/10/11/12 color queries (`default_colors` fg `#d4d4d4`, bg `#1e1e1e`) and CSI 14/16/18 t size queries; `terminal.history` capped at 5 000 rows / 2 MiB per call with paging; restart uses the last existing OSC 7 cwd; 5 s `hello` timeout; corpus recordings `omp_startup`, `codex_startup` (100×30, scanned clean; both probe OSC 10/11).
- Orchestrator: ADR 0002 amended (single queue + credit); `host::size()` falls back to 80×24 for unset PTY dimensions (found in live check: attach from a PTY without a window size sent 0×0 → `bad_request`); implementer agent instructions require absolute worktree paths (two implementers had written into the main checkout first).
- Verification: `just check` 203 tests green; live release run: `kill -TERM $$` session listed as `exited (signal 15)`, `terminal.history` with `count: 100000` returned 5 000 rows, attach to an exited session over a PTY showed the final text and `[session exited with code 3]`, exit 0.

### 2026-10-13 — M1-D2 merged, M1 closed
- `heeler` session commands + `attach` (raw mode guard incl. panic/SIGTERM/SIGHUP restore, write→flush→ack ordering, SIGWINCH resize, 300 ms restart follow), target resolution (id, prefix ≥ 4, `t:N`, title; ambiguous → exit 2), `history`; 22 new CLI tests incl. 11 PTY-driven attach tests; `crates/daemon/examples/flood.rs`; `docs/demos/M1.md`.
- Orchestrator verification: `just check` 188 tests green; flood bench rerun: `wall=1.4s output~193MiB (136MiB/s) acking_renders=137 silent_renders_after_snapshot=0 status_p50=0.03ms p99=0.11ms peak_rss=25.2MiB final_screen=match`.
- Live demo (orchestrator, release build, temp `HEELER_HOME`): `heeler new --title omp-demo -- omp` → OMP v18.10.3 TUI rendered in the daemon (seen via `heeler history`); two simultaneous `heeler attach` over real PTYs both rendered OMP and detached with ctrl+\ d (exit 0) while the session kept running; `heeler send` typed into OMP's composer (visible on screen); `stop` → `stopped`; `daemon stop --stop-sessions` → socket gone, no orphan processes.

### 2026-10-13 — M1-C, M1-D1 merged; protocol `SessionResult`
- M1-C `heeler-cli`: `daemon start|status|stop`, `ws …`, `api`, `events`; exit codes 0/1/2/3; `HostRenderer` (SGR incl. undercurl/underline color, wide runs, OSC 8, modes mirroring, `?2026` wrapping, row diffing, scroll via `CSI S/T`), `DetachDetector`. Orchestrator smoke: start (idempotent), ws new/ls, api, stop, status exit 3.
- M1-D1 daemon sessions: `heeler-terminal`-backed sessions, attach with credit-1 pacing (snapshot spends credit), bounded input via `spawn_blocking`, metadata events, workspace/shutdown rules, oversized responses → `internal`. 5/5 repeated daemon suite runs green. Reviewed `terminals.rs` render loop (ack → damage → coalesce → render).
- Orchestrator contract change: `SessionCreated` → `SessionResult` (create/stop/restart/rename), wire unchanged. `just check` 161 tests green.

### 2026-10-13 — M1-B merged
- `heeler-terminal`: `Emulator` (OSC sniffer → alacritty 0.26 → render DTOs), `Terminal` (portable-pty, reader/writer threads, bounded 256-entry input queue, DEC 2026 150 ms timeout, HUP→KILL terminate, reap once), `RenderTracker` diffs; `Terminal::render` returns a snapshot after resize/palette change (frames carry no size/palette). Semantics documented in `protocol.md`.
- Conformance corpus (M0.4) + recorder/flood examples. Flood: 180.7 MB/s median, 26.2 MiB peak RSS, history exactly 10 000 lines.
- Orchestrator: rebased (Cargo.lock conflict resolved from master + cargo re-lock), `just check` 81 tests green, reviewed `sys.rs` (only `unsafe`: `killpg`/`kill`).
- Open: htop/agent TUI recordings; `PromptMark::D` carries exit code; link table capped at 16 384.

### 2026-10-13 — M1-A merged
- `heeler-daemon` (`heelerd` + lib: paths, flock single instance, stale-socket cleanup, peer-UID check via tokio `peer_cred`, event bus ring 10k events / 8 MiB with atomic handshake+replay, bounded per-connection queues closing slow consumers, in-memory workspace registry actor, router) and `heeler-client` (`connect`, `connect_resuming`, `call`, `events`, `terminal_frames`, `send_input`, autostart).
- Verified by orchestrator: `just check` 48 tests green after rebase; reviewed bus/conn backpressure and replay atomicity. Implementer ran the manual `heelerd` start / second-instance exit 2 / SIGTERM socket cleanup.
- No `unsafe`/`libc`: lock via `File::try_lock`, UID via `peer_cred`.

### 2026-10-13 — M0 contracts, implementer workflow
- M0.1: workspace, toolchain pin, `just check` (fmt, clippy `-D warnings`, nextest, cargo-deny with `[licenses.private] ignore` until a license is chosen). Crates are created when their milestone starts.
- M0.2: `heeler-core` (ULID ids via `Ulid::generate`, `TerminalId`, `Seq`, `SessionState` machine) and `heeler-protocol` (frame codec with 4 MiB cap, control envelope that keeps request ids for unknown methods, typed requests/results/events, postcard render DTOs). 34 golden fixtures in `crates/protocol/tests/fixtures/v0/`. `just check`: 18 tests pass.
- Implementer workflow in `process.md`: briefs `docs/briefs/M1-A.md`, `M1-B.md`, `M1-C.md`; project agent `.omp/agents/implementer.md` on `anthropic/claude-sonnet-5-5:high`.
- Antigravity was tried first; `agy -p` fails with account eligibility, so it was removed from the process at the user's request.

### 2026-10-13 — Scope, architecture, process, spikes
- Wrote `vision.md`, `architecture.md`, `protocol.md` (v0 draft), `roadmap.md` (M0–M9, risks), `process.md`, `AGENTS.md` (+ `CLAUDE.md` symlink).
- ADRs accepted: 0001 topology, 0002 IPC, 0003 GPUI, 0004 terminal engine/stream, 0005 agents, 0006 storage, 0008 credentials, 0009 process survival. Proposed: 0007 packaging/notifications (M6 spike).
- Spikes (reports in `docs/spikes/`):
  - GPUI: upstream zed git rev builds and runs here; 6.3 MiB binary, 75 MiB idle RSS; gpui-component/kit incompatible (pins `gpui-pre` snapshot).
  - Terminal: alacritty_terminal 0.26.0 181 MB/s, 23 MiB @10k lines, replies OK; libghostty-vt 0.2.2 blocked by Zig/SDK and lacks binary snapshot API; VT-replay reattach loses state → daemon-side render deltas.
  - Agents: Codex app-server end-to-end with real approval; ACP via codex-acp initialize/session/new; Codex `thread/resume` drops tool items → Heeler journal is authoritative.
- Change vs original spec: terminal stream is render snapshots + ack-paced deltas (not raw bytes + client parsing); notifications need a bundled, signed process (Notifier helper or bundled daemon, to verify in M6).
