# 0016. Everything controllable by agents: one control surface (CLI, MCP, UI commands)

- Status: accepted (2026-10-16, with the user)
- Milestone: M8-C
- Builds on: ADR 0001 (daemon owns state), ADR 0010 (layout is desktop state), ADR 0015 (harness chats)

## Context
The user wants to drive almost everything in Hutan through the assistant or any agent: create, send to, stop and inspect sessions, run tasks and worktrees, organize projects and workspaces, and change the desktop layout (presets, docks, splits, panels, focus, settings). Today agents can already call the `hutan` CLI from inside a session (`HUTAN_BIN`, `HUTAN_SESSION_ID`), but the CLI cannot touch the desktop, the daemon cannot tell an agent's request from the user's, and harness agents get no structured tool list.

## Decision
- **One surface, three doors.** Every controllable thing is a daemon request. The CLI is its command-line door (exists), `hutan mcp` is its MCP door (stdio MCP server, tools generated from the same request set), and the desktop is a client that also *executes* UI requests the daemon routes to it.
- **UI control through the daemon.** The desktop publishes on connect (and on change) its **action catalog** — every palette command and every settings key, with id, title, argument schema — and a **layout summary** (active workspace/project, preset, docks with panels, the pane tree with item kinds and session ids, focus, zoom, appearance). New requests: `ui.actions` (catalog), `ui.layout` (summary), `ui.run {action, args}` (routed to the focused desktop of the target window; correlated result `{ok | error}`), `ui.set_setting {namespace, key, value, layer: user | workspace | project}` (the desktop writes its TOML, validated by the schema). With no desktop connected these fail with `invalid_state` naming that no window is open. Layout stays desktop state (ADR 0010): the daemon only routes and caches the last summary.
- **Who is asking.** `ClientInfo` gains `origin: {session_id}` when the CLI or MCP server runs inside a Hutan session (from `HUTAN_SESSION_ID`); the daemon marks such connections **agent-originated**.
- **Policy, enforced in the daemon** (`daemon.toml [control] agents = "ask" | "allow" | "read_only"`, default `ask`; per-session override when an assistant is created with a control level): reads are always allowed; **mutating** requests from an agent-originated connection become a Hutan approval (kind `control`, shows the request in plain words, options Allow once / Allow for this session / Deny) — the same approvals UI, notifications (M6-B) and audit table as agent tool approvals. `read_only` refuses mutations with `forbidden`; `allow` runs them and still audits them. A request can never raise the caller's own privileges (e.g. change `[control]` itself is user-only).
- **Wiring into agents.** Harness sessions started by Hutan (assistants, chats, tasks) get `hutan mcp` in their MCP server list (ACP `mcpServers`, Codex `-c mcp_servers.hutan…`) unless `[control] mcp = false`; `hutan integration install` can add it to the user's own OMP / Claude Code / Codex configs on request (marked entries, backups, like M3-B hooks). Agents in plain terminals use the CLI.
- **Discoverability.** `hutan help agent` prints a compact guide; MCP tool descriptions are written for models (what it does, when to use it, what needs approval).

## Consequences
- Anything added to the palette or settings becomes agent-controllable automatically; new daemon requests become MCP tools by declaring them once.
- The approval prompt is the safety boundary; `ask` keeps the user in the loop by default.
