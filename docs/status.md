# Status (handoff log)

Newest entry first. Every session ends by updating **Current** and adding a log entry (see `process.md`).

## Current
- **Milestone**: M1 — Daemon + CLI terminals (M0 complete)
- **Active slice**: M1-C (`../heeler-wt/M1-C`, CLI) and M1-D1 (`../heeler-wt/M1-D1`, daemon sessions/attach) running on the `implementer` agent. Merged: M1-A `4addc5d`, M1-B `dd63216`.
- **Next action**: review/merge M1-C and M1-D1; then M1-D2 (orchestrator): CLI `new/ls/attach/send/stop` on top of both, M1.7 bench, M1 demo.
- **Blockers**: none. Antigravity (`agy`) was dropped from the process: account not eligible ("Eligibility check failed"); can return once the account works.
- **Open questions** (decide during M0/M1, not blocking):
  1. Project license (affects porting components from Zed's GPL crates; ADR 0003).
  2. Distribution: notarized Developer ID vs ad-hoc only for personal use (affects ADR 0007, Keychain prompts).
  3. Default detach key for `heeler attach` (proposal: `ctrl+\` then `d`).
  4. Product name: "Heeler" is already used by an iOS console for herdr (heeler.bybee.dev) and by heeler.com (AppSec). Keep as codename or rename before any public release.

## Environment notes
- macOS 26.6.2 arm64, Rust 1.98.0, Xcode installed **without** separate Metal compiler → GPUI needs `runtime_shaders` (ADR 0003).
- Installed Zig 0.16.0 cannot build libghostty-vt 0.2.2 (needs 0.15.2) — not used (ADR 0004).
- Codex CLI 0.144.2 rejects the configured default model `gpt-6-astra` ("requires a newer version of Codex"); `gpt-5.6-sol` from `model/list` works. Upgrade Codex before M4, and pick models from `model/list` in the adapter.
- Installed agents: `codex` 0.144.2, `codex-acp` 1.12.0, `omp` 18.10.3 (`omp acp`), `opencode` 2.0.14; no Claude Code ACP adapter.
- Antigravity CLI `agy` 1.3.4 is installed but unusable (account not eligible); not part of the process.
- Gate tools: just 1.58.0, cargo-deny 0.20.2, cargo-nextest 0.9.148 (Homebrew).

## Log

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
