# Status (handoff log)

Newest entry first. Every session ends by updating **Current** and adding a log entry (see `process.md`).

## Current
- **Milestone**: M0 — Foundations
- **Active slice**: none (M0.3 done)
- **Next action**: M0.1 — create the Cargo workspace: root `Cargo.toml` with `[workspace.dependencies]` pinning `gpui`/`gpui_platform` (ADR 0003 rev `ef376006f32728b15e6a67706a04849cb781c812`, features `font-kit`, `runtime_shaders`), `alacritty_terminal = "=0.26.0"`, `portable-pty = "=0.9.0"`; empty crates per `architecture.md` § Crates; `rust-toolchain.toml` (1.98.0), `deny.toml`, `justfile` with `check`; verify `just check` green.
- **Blockers**: none.
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

## Log

### 2026-10-13 — Scope, architecture, process, spikes
- Wrote `vision.md`, `architecture.md`, `protocol.md` (v0 draft), `roadmap.md` (M0–M9, risks), `process.md`, `AGENTS.md` (+ `CLAUDE.md` symlink).
- ADRs accepted: 0001 topology, 0002 IPC, 0003 GPUI, 0004 terminal engine/stream, 0005 agents, 0006 storage, 0008 credentials, 0009 process survival. Proposed: 0007 packaging/notifications (M6 spike).
- Spikes (reports in `docs/spikes/`):
  - GPUI: upstream zed git rev builds and runs here; 6.3 MiB binary, 75 MiB idle RSS; gpui-component/kit incompatible (pins `gpui-pre` snapshot).
  - Terminal: alacritty_terminal 0.26.0 181 MB/s, 23 MiB @10k lines, replies OK; libghostty-vt 0.2.2 blocked by Zig/SDK and lacks binary snapshot API; VT-replay reattach loses state → daemon-side render deltas.
  - Agents: Codex app-server end-to-end with real approval; ACP via codex-acp initialize/session/new; Codex `thread/resume` drops tool items → Heeler journal is authoritative.
- Change vs original spec: terminal stream is render snapshots + ack-paced deltas (not raw bytes + client parsing); notifications need a bundled, signed process (Notifier helper or bundled daemon, to verify in M6).
