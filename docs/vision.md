# Heeler — Vision and Target Scope

Heeler is a lightweight, macOS-first, fully Rust **agentic development environment**: a real terminal emulator and an AI agent workspace in one native GPUI app, backed by an independent daemon that keeps everything running when the window is closed.

Inspirations and what we take from each:

| Project | What it is | What we take | What we avoid |
|---|---|---|---|
| [Orca](https://github.com/stablyai/orca) | Electron ADE: parallel agents in git worktrees, terminals, diffs, browser, CLI, remote servers | Worktree-per-task model, agent CLI that drives the app, unread/attention state, server/client split (`orca serve`) | Electron footprint |
| [Herdr](https://herdr.dev) | Rust/ratatui agent multiplexer with server + socket API | Server-owned panes, agent state `idle/working/blocked`, screen-detection manifests + integration reports, env injection into panes, native agent resume commands, live PTY handoff on upgrade | TUI-only presentation |
| [Heeler for herdr](https://heeler.bybee.dev/) | Native iOS console for herdr | Remote-client shape: a thin native client over a daemon API | — |
| Zed | GPUI editor with ACP agent panel | GPUI, ACP client model | Editor scope |

## Product principles

1. **Daemon is the source of truth.** UI crash/close never stops work. Every UI (GPUI, CLI, future remote/mobile) is a client of one protocol.
2. **Terminal-first.** Any TUI agent (OMP, Claude Code, Codex, OpenCode) and any shell must work unmodified in a real PTY. Structured chat is additive, never a replacement.
3. **Native and light.** No Electron, no webview for core UI. Budgets are enforced, not hoped for (see budgets below).
4. **Model-agnostic, policy-honest.** Many providers, local models, API keys, OAuth only where the provider officially supports it. Consumer subscriptions are used only through the vendor's own harness (e.g. Codex/Claude Code login), never re-used as API credentials.
5. **Permissions enforced in the daemon.** UI dialogs only collect decisions.
6. **Boring by default.** Proven crates, explicit state machines, one convention per concern.

## Target scope (end state)

### Workspace and terminals
- Workspaces → tabs → split panes; any pane hosts a terminal session, a harness chat, or a direct model chat.
- Full PTY terminal: truecolor, alt screen, mouse, bracketed paste, kitty keyboard, OSC 8/133, synchronized output, wide chars/emoji; bounded scrollback; search; selection/copy.
- Detach/reattach, multiple simultaneous clients, session restore after UI restart (live) and daemon restart (respawn in cwd + native agent resume).
- Daemon upgrade without killing PTYs (fd handoff), later.

### Agents and chat
- Harness chat: structured sessions over Codex app-server and ACP (Claude Code, OpenCode, OMP, Gemini, …) with streaming, tool calls, diffs, plans, permission requests.
- TUI agents in terminals get state detection (`idle/working/blocked/done`) via integrations/hooks first, screen-detection manifests as fallback.
- Direct model chat via our own model gateway: OpenAI-compatible, Anthropic, Gemini, local (Ollama/LM Studio/llama.cpp server); credentials in Keychain.

### Orchestration
- Git worktree per task, multiple repositories per workspace, task list with dependencies, parallel agents and subagents, review/diff of agent changes, merge/cleanup.
- Attention model: unread, blocked, completed rollups up to tab/workspace/dock badge.

### macOS integration
- Native notifications with actions (approve/deny/open) routed back to the daemon.
- Per-user launch agent (SMAppService) so agents run with the window closed; menu bar status; global shortcuts; Dock badge.

### Extensibility
- Versioned IPC protocol with published schema; CLI with `--json` everywhere.
- ACP and MCP integration; provider adapters as crates behind traits.
- Plugins (manifest actions + event hooks), later remote clients (TLS over Tailscale/SSH), mobile.

## Non-goals (until explicitly re-scoped)
- Code editor / LSP host. We show diffs and open files in the user's editor.
- Embedded web browser.
- Windows/Linux UI. The daemon stays portable (Unix-first); the desktop is macOS.
- Hosting or reselling model access.
- Reusing subscription OAuth tokens outside their official clients.

## Budgets (enforced from M2 on)

| Metric | Budget |
|---|---|
| Idle desktop RSS, 1 window, 4 terminals | ≤ 150 MB |
| Idle daemon RSS, 10 sessions, 10k-line scrollback each | ≤ 120 MB |
| Idle CPU (daemon + desktop) | ≈ 0 %, no polling loops |
| Keystroke → glyph on screen (local) | ≤ 16 ms p95 |
| Sustained output `cat` 200 MB | UI stays interactive; daemon memory bounded |
| Cold start to first terminal prompt | ≤ 500 ms with daemon running |

Numbers are initial targets; adjust via ADR with measurements, never silently.
