# 0015. Harness chat over ACP, accounts through harnesses, OMP first-class

- Status: accepted (2026-10-15, with the user)
- Milestone: M8 (pulled forward), M16 follow-up
- Amends: ADR 0005 (agent integration), ADR 0008 (credentials) — confirms its rule

## Context
- Users want to chat (and run the assistant) through the harnesses they already use and are logged in to: OMP (OAuth logins to Anthropic, OpenAI Codex, xAI, OpenCode, …), Claude Code, Codex. Measured here: `omp acp` (18.11.1) answers ACP `initialize` with `loadSession`, `sessionCapabilities {list, fork, resume, close}`, MCP over http/sse, embedded context and images, and one auth method `agent` ("use existing local credentials under ~/.omp"). `claude` 2.1.299 and `codex` 0.162.1 are installed; Codex already runs over `codex app-server` (M4).
- The user first asked for Hutan's own OAuth like OMP's; after weighing it (Anthropic blocks subscription OAuth outside Claude Code: we hit `403 OAuth authentication is currently not allowed` today; ADR 0008), the user chose **logins through harnesses**.

## Decision
- **Hutan never performs subscription OAuth and never reads, stores or forwards harness tokens.** ADR 0008 stands. API keys for direct providers stay in the Keychain (M5).
- **ACP client** in `crates/agents` (ACP v1 over stdio JSON-RPC, like the Codex adapter): sessions, prompt turns with streaming updates, tool calls, plans, permission requests (`session/request_permission` → Hutan approvals, enforced in the daemon), `session/load`/`resume`, model and mode selection where the agent offers them, MCP servers passed through. Mapped onto the existing `AgentEvent` model and chat journal, so the chat view, approvals, notifications, history index and tasks work unchanged.
- **Harnesses** (registry, `daemon.toml [harness.<id>]` with `bin`/`args`, detection on PATH):
  - `omp` — **first-class**: `omp acp`; models from OMP (`omp models --json`, or the ACP session config), OMP sessions resumable by id, OMP's own subagents shown in the agent tree.
  - `claude` — Claude Code over ACP through the Claude Code ACP adapter (`claude-code-acp`; Hutan points to it, does not bundle it); its login is Claude Code's.
  - `codex` — stays on `codex app-server` (M4); its login API (`account/read`, `account/login/start`) is used for the accounts view.
- **Accounts** view (daemon `account.list|login|logout`): per harness, which providers/accounts are logged in (OMP: providers with credentials from `omp models --json` / `omp usage`; Codex: `account/read`; Claude Code: its status command) — names and plan only, never tokens. *Log in* runs the harness's own flow: an interactive terminal session (`omp login <provider>`, `claude` `/login`, `codex login`) or the harness API when it has one (Codex `account/login/start` returns a URL the desktop opens). *Log out* likewise delegates.
- **Assistant backend per scope** (M16 follow-up): the global assistant and each project's assistant choose `omp` / `claude` / `codex` / a direct provider; the assistant is **optional** (`assistant.enabled`, default on; off hides the panel and ⌥Space). Harness assistants run with tools restricted to read-only (or none) where the harness allows it; attachments (M16) are sent as ACP embedded context.
- **Tools**: MCP servers configured in Hutan (`[mcp.<name>]`) are passed to ACP sessions; Hutan as an MCP server is M8 later.

## Consequences
- One chat UI for every harness; no token handling in Hutan; harness version ranges are pinned and fixture-tested like Codex.
- Features depend on what each harness exposes over ACP (models, modes, subagents); gaps are shown, not faked.
