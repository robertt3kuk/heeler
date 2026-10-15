# 0001. Daemon-authoritative process topology

- Status: accepted
- Date: 2026-10-13
- Milestone: M0

## Context
Agents and terminals must survive UI close/crash/restart; GUI and CLI must work against the same live state at the same time; future remote, web, and mobile clients must not require daemon changes. Herdr (server + socket API, clients attach) and Orca (`orca serve`, clients pair) both converged on this split.

## Options
| Option | Pros | Cons |
|---|---|---|
| Monolithic GUI app owning PTYs | Simplest start | UI close kills work; CLI needs IPC anyway |
| GUI owns state, daemon only for background | Fast UI | Two sources of truth, reconciliation bugs |
| **Daemon owns all execution/state; GUI/CLI are clients** | One truth, multi-client, remote-ready | Protocol and reconnection work up front |

## Decision
Four executables, one source of truth:

| Executable | Role | Depends on |
|---|---|---|
| `staffyd` | Tokio daemon: PTYs, terminal state, sessions, agents, model gateway, orchestration, SQLite, Keychain, permission policy, event log | `core`, `protocol`, internal crates |
| `Staffy.app` (desktop) | GPUI client: rendering, input, layout, ephemeral UI state | `protocol`, `client` |
| `staffy` (CLI) | Scriptable client; `staffy attach` raw-terminal attach | `protocol`, `client` |
| `Staffy Notifier` (helper inside the app bundle) | Delivers macOS notifications, routes actions back to the daemon | `protocol`, `client` |

Rules checked in review:
- Clients never spawn agent/terminal processes, never call provider APIs, never store credentials, never mutate authoritative state except through protocol commands.
- `staffyd` never links GPUI/AppKit UI code. macOS service calls (Keychain, SMAppService registration status) are allowed through `security`/`platform` crates.
- Crate dependencies point inward: `desktop`/`cli` → `client` → `protocol` → `core`. Nothing depends on `daemon`.

## Consequences
- Protocol, reconnection, and snapshots are first-milestone work, not polish.
- The CLI `attach` gives a usable persistent terminal (zmx/tmux-like) before the GUI exists, de-risking the daemon early.
- Remote clients later need only a new transport (TLS/SSH tunnel) in front of the same protocol.

## Evidence
- Herdr socket API and session-state docs: <https://herdr.dev/llms.txt>
- Orca runtime modes: <https://www.onorca.dev/docs/ways-to-run>
