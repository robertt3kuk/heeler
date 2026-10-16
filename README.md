<p align="center">
  <img src="assets/brand/01-logo-classic/hutan-logo.png" alt="hutan — code from the canopy" width="640">
</p>

# Hutan

**A native macOS agentic development environment in Rust.** *Code from the canopy.* A real terminal emulator and an AI-agent workspace in one GPUI app. It runs on a separate daemon (`hutand`) that keeps your shells, coding agents and chats alive when the window is closed. *Hutan* is Malay and Indonesian for "forest", as in *orang-hutan*, "person of the forest", which is our mascot.

> **Status: pre-alpha, macOS only, not released.** The first vertical slice (milestones M0–M6) is nearly complete: native notifications work; keep-running at login, the menu bar item and the Dock badge (M6-C) are in progress, and several UI surfaces have not had a visual review yet. Expect breaking protocol changes. See [`docs/status.md`](docs/status.md) and [`docs/roadmap.md`](docs/roadmap.md).

## Why

- **The daemon is the source of truth.** If the UI crashes or you close it, work keeps going. The desktop app, the `hutan` CLI and any future remote client all speak the same protocol over a Unix socket.
- **Terminal-first.** Shells and TUI agents (Claude Code, Codex, OMP, OpenCode) run unmodified in a real PTY. Structured agent chat is added on top of terminals, not instead of them.
- **Native and light.** No Electron and no webview for the core UI, and performance budgets are measured (`just bench`).
- **Permissions are enforced in the daemon.** UI dialogs only collect decisions, and API keys live in the macOS Keychain.

## What works today

| Area | What you get |
|---|---|
| Terminals | PTY sessions owned by the daemon, compact bounded scrollback, detach/reattach, several clients attached at once, restore after a daemon restart (respawned in the last cwd) |
| Shell blocks | zsh/bash/fish integration injected automatically (OSC 133/7): commands become blocks with exit code and duration that you can jump to, copy, collapse or send to an agent |
| Workbench | GPUI desktop with docks, split panes, zoom, presets (default/focus/overview), ⌘K palette, settings in TOML with hot reload, keymap file |
| Workspaces | One workspace per project, plus a permanent **Home**; ⌘O to open, ⌘J to jump; sort modes, pins and per-workspace layouts |
| Agent chat | Codex through `codex app-server`: streaming, tool cards, diffs, plans, approvals enforced in the daemon; the same conversation is visible from the CLI |
| Agent monitor | Live state (working/blocked/idle) of agents and subagents from Codex, Claude Code hooks and the OMP extension |
| Direct model chat | OpenAI-compatible, Anthropic and Ollama/LM Studio providers; switch models mid-conversation; keys in Keychain |
| Assistant | ⌘K → Assistant opens a quick chat (optional system-wide shortcut), global or per workspace; context (shell output, agent transcript, file, search) is attached explicitly as chips, then redacted and bounded |
| History search | Opt-in local SQLite FTS5 index over Hutan sessions and Claude Code, Codex and OMP transcripts, with one-click **Resume**, Reveal File and Open Block |
| Notifications | The daemon raises intents (approval needed, agent blocked/finished, non-zero exit, reply ready). `Hutan.app` posts them as macOS notifications: **Allow once** on the banner answers the agent's approval; `hutan notify watch` prints them anywhere |
| Worktrees | `hutan wt new/ls/rm`, `hutan new --worktree <branch>`; every session shows `main` or `wt <branch>`, with a warning when two agents work in one checkout |
| Session environment | New terminals start from your login environment, like Terminal.app; commands you run directly get the `PATH` your login shell builds |
| App bundle | `just bundle` builds an ad-hoc signed `Hutan.app` (desktop, daemon, CLI) with the orangutan icon |

## The mascot

One orangutan, many moods: each state of the interface has its face.

<p align="center">
  <img src="assets/brand/05-mascot/png/state-thinking@2x.png" alt="thinking: an agent is working" width="96">
  <img src="assets/brand/05-mascot/png/state-success@2x.png" alt="success: tests passed" width="96">
  <img src="assets/brand/05-mascot/png/state-error@2x.png" alt="error: the build failed" width="96">
  <img src="assets/brand/05-mascot/png/state-focus@2x.png" alt="focus mode" width="96">
  <img src="assets/brand/05-mascot/png/state-wink@2x.png" alt="onboarding tip" width="96">
  <img src="assets/brand/05-mascot/png/state-sleep@2x.png" alt="idle" width="96">
  <img src="assets/brand/05-mascot/png/state-empty@2x.png" alt="nothing here yet" width="96">
  <img src="assets/brand/05-mascot/png/state-404-hanging@2x.png" alt="404: hang on" width="96">
</p>

Icons, logos, angles, animated SVGs and the scripts that generate them live in [`assets/brand/`](assets/brand/README.md); `just icon` renders the app icon from `assets/brand/05-mascot/svg/icon-dark.svg`.

## Architecture

```mermaid
flowchart LR
  D[Hutan desktop<br/>GPUI] <-->|Unix socket protocol| H[(hutand)]
  CLI[hutan CLI] <-->|Unix socket protocol| H
  H -->|PTY| T[shells and TUI agents]
  H -->|stdio JSON-RPC| C[codex app-server]
  H -->|HTTPS| M[model APIs / local models]
  H --- DB[(SQLite)]
  H --- KC[(Keychain)]
```

Crates live in `crates/` (`core`, `protocol`, `daemon`, `client`, `cli`, `terminal`, `terminal_view`, `desktop`, `agents`, `gateway`, `storage`, `index`, `settings`, `chat_view`). Details are in [`docs/architecture.md`](docs/architecture.md), and the decisions behind them in [`docs/adr/`](docs/adr).

## Requirements

- macOS 13 or later; developed on Apple Silicon.
- [rustup](https://rustup.rs). The toolchain (Rust 1.98.0) is pinned in `rust-toolchain.toml` and installs itself on the first build.
- Xcode or the Command Line Tools (`xcode-select --install`). The separate Metal toolchain is **not** needed because GPUI is built with `runtime_shaders`.
- For the quality gate: `brew install just cargo-nextest cargo-deny`.
- Optional:
  - `codex` ≥ 0.160 for Codex agent chat;
  - [Ollama](https://ollama.com) or LM Studio for local models;
  - an API key for OpenAI-compatible or Anthropic providers.

## Quick start

The first build fetches and compiles GPUI, which takes a few minutes.

```sh
git clone https://github.com/robertt3kuk/hutan.git
cd hutan
cargo build --workspace --bins --locked
```

Keep experiments away from your real data: `HUTAN_HOME` puts the socket, database, logs and the Home folder into one directory.

```sh
export HUTAN_HOME="$(mktemp -d)"
```

### Desktop app

```sh
cargo run -p hutan-desktop
```

The app starts `hutand` itself, using the copy built next to it. On quit it stops the daemon if no session is running; otherwise it asks.

| Keys | Action |
|---|---|
| ⌘T / ⌘⇧T / ⌘⌥T | New terminal / new agent chat / new model chat |
| ⌘⇧[ / ⌘⇧] | Previous / next tab |
| ⌘K | Command palette |
| ⌘D / ⌘⇧D | Split right / down |
| ⌘O / ⌘J | Open project / jump to a workspace or session |
| ⌘⇧O | Overview of all sessions |
| ⌘B / ⌘⌥B / ⌘⌥J | Toggle left / right / bottom dock |
| ⌘⇧J | Jump to the next session that needs you |
| ⌘, | Settings |

### CLI

```sh
alias hutan="$PWD/target/debug/hutan"

hutan daemon start
hutan new --title demo -- /bin/sh -c 'echo hello; exec $SHELL'
hutan ls
hutan attach demo           # detach with ctrl+\ then d
hutan history demo          # scrollback as plain text
hutan ws ls                 # workspaces (Home is always there)
hutan notify watch          # notification intents as they happen
hutan daemon stop --stop-sessions
```

Other useful commands (each takes `--help`, and every command takes `--json`):

```sh
hutan provider discover                      # find local Ollama / LM Studio
hutan provider add ollama --name Local       # provider id is made from the name
hutan chat new --direct --provider local     # chat with a model
hutan ask "what does this error mean?" --attach file:src/main.rs   # needs [assistant] provider = "local" in daemon.toml
hutan chat new                               # Codex agent chat (needs codex >= 0.160)
hutan approvals && hutan approve <id> accept
hutan index enable && hutan search "cargo test -p daemon"
hutan integration status claude              # agent hooks; install only when you want them
```

### App bundle

```sh
just bundle                  # target/bundle/Hutan.app, ad-hoc signed
just bundle --release
```

Running the bundle is safe to try. Registering it as a launch agent and native notifications are not implemented yet (M6-B).

## Development

```sh
just check     # fmt + clippy -D warnings + nextest (about 1460 tests) + cargo-deny; must be green before a merge
just bench     # performance budgets B1–B6 (docs/perf.md)
just fmt
```

- Process and conventions: [`AGENTS.md`](AGENTS.md), [`docs/process.md`](docs/process.md)
- Wire protocol: [`docs/protocol.md`](docs/protocol.md)
- UI design: [`docs/design.md`](docs/design.md)
- Vision and target scope: [`docs/vision.md`](docs/vision.md)

## Where data lives

| | Default | With `HUTAN_HOME` |
|---|---|---|
| Socket, database (`hutan.db`), daemon settings (`daemon.toml`) | `~/Library/Application Support/Hutan` | `$HUTAN_HOME` |
| Logs | `~/Library/Logs/Hutan` | `$HUTAN_HOME/logs` |
| Home workspace folder | `~/Hutan` | `$HUTAN_HOME/Hutan` |
| API keys | macOS Keychain, service `dev.hutan.provider` | same |

## License

Not decided yet. It depends on whether the embedded editor (M11) reuses GPL-3.0 crates from Zed ([ADR 0012](docs/adr/0012-embedded-editor.md)). Until a license is added, all rights are reserved.
