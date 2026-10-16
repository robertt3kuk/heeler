# M8-B: desktop for harness chats, accounts and the assistant backend per scope

Agent: implementer   Depends on: master `fd101e6` (M8-A protocol 0.17, D-1 visual system v2)   Branch: slice/M8-B

## Context
Read AGENTS.md, then ADR 0015, `docs/agents.md` § ACP harnesses (M8-A), `docs/assistant.md`, `docs/design.md` § Visual system v2 + § Workbench model. Code: `crates/client` (harness chat, `account_*`), `crates/desktop/src/workbench/chat/*` (chat view, composer, model picker), `workbench/providers.rs` (Models sheet), `workbench/assistant.rs` + `panels/assistant.rs` (assistant panel), `workbench/actions.rs` (New Agent ⌘⇧T picker), `workbench/settings_sheet.rs`, `workbench/ui.rs` (component kit — use it, theme tokens only).

## Scope (contract changes authorized)
`crates/desktop`, `crates/daemon` + `crates/protocol` only for the assistant backend (below; additive, take the next free minor at merge — use **0.22**, the integrator renumbers), `crates/client`, docs (`docs/assistant.md`, `docs/design.md` § Accounts / § Assistant, `docs/protocol.md`).

## Contract
- **New Agent picker** (⌘⇧T): lists available harnesses (OMP first, then Claude Code, Codex) with detection state (installed / not found + how to install) and the logged-in providers per harness; picking one opens a harness chat in the focused cwd; a model menu shows the harness's models (OMP grouped by provider); the chat header shows harness + model and lets you switch model between turns where the harness allows.
- **Accounts** (Settings sheet → Accounts, and ⌘K "Accounts…"): per harness the accounts from `account.list` (provider, label, plan, status), buttons Log in / Log out that call `account.login/logout` and focus the terminal session the daemon opens for the login; refresh on `account.changed`. Never shows tokens. Empty state with the mascot when no harness is installed.
- **Assistant backend per scope**: `assistant.enabled` (default true; off hides the Assistant panel and unregisters ⌥Space and the palette entries). Global and per-project/workspace assistant choose a backend: `omp` / `claude` / `codex` / a direct provider (M5) + model. Daemon: `daemon.toml [assistant] backend = "omp" | "claude" | "codex" | "provider"` + `model` (+ existing `provider`), per-workspace override in the daemon's workspace settings via a new `assistant.configure {scope, workspace_id?, backend, model?, provider?}` request (persisted in storage); `assistant.open` creates the default assistant with the configured backend; harness assistants run with tools disabled or read-only when the harness supports it (OMP: `--tools` / ACP mode when offered; document what each harness can do), attachments sent as ACP embedded context. Panel header: backend + model switcher; first-run card offers "Use OMP" (when detected and logged in) or "Add a provider".
- Replace the raw "configure provider in daemon.toml" notice with that first-run card.

## Acceptance
- Daemon tests: `assistant.configure` persistence and `assistant.open` with omp/claude/codex/provider backends (fake ACP agent + fake Codex fixtures), attachments reaching an ACP harness as embedded context.
- GPUI tests: New Agent picker lists harnesses and opens a chat; model switch; Accounts lists and Log in focuses the login session; assistant disabled hides panel and shortcut; backend switch in the panel; first-run card.
- Golden fixtures; docs; `just check` green; committed on `slice/M8-B`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run real logins or read harness credentials; fakes only in tests. Theme tokens and `ui.rs` only. No push, no hard reset; `pgrep -x hutand` clean. Do not edit docs/status.md or docs/roadmap.md. Disk is tight: targeted runs while working, `just check` once at the end. Concurrent desktop slices: M7-C (tasks panel), M8-C (`workbench/remote.rs`); keep to your files and rebase late.
