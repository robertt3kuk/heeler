# M3-B: agent reporting channel — state + resume command (roadmap M3.3, foundation for M13)

Agent: implementer   Depends on: master `0e44c24`+ (M3-A merged: `resume_ref_json` stored and used on restore; protocol 0.3)   Branch: slice/M3-B

## Context
Read `AGENTS.md`, `docs/roadmap.md` § M3 and § M13, ADR 0009, `docs/protocol.md` (0.3), `crates/daemon/src/{registry,restore}.rs` (`resume_argv`), `crates/storage`.

Prior art on this machine (read, do not modify; Herdr is MIT? — check its LICENSE before copying any text; Orca is MIT? — check; never copy GPL/AGPL code):
- Herdr's model (<https://raw.githubusercontent.com/herdrdev/herdr/v0.9.3/docs/next/website/src/content/docs/add-herdr-support.mdx>): every pane process gets `HERDR_ENV`, `HERDR_PANE_ID`, `HERDR_BIN_PATH`, `HERDR_SOCKET_PATH`; agents (or hook scripts) report `state` (`idle|working|blocked`, monotonic `seq`), a **resume argv** (first word a plain command on PATH, ≤ 64 args, ≤ 8 KiB, no apostrophes/control chars) and `release` on exit; the server restarts the pane with that argv after a restart.
- Hook scripts installed by Herdr/Orca: `~/.codex/hooks.json` + `~/.codex/herdr-agent-state.sh` (Codex hooks `SessionStart`, `UserPromptSubmit`, `Stop`, …, JSON on stdin with `session_id`, `transcript_path`), `~/.claude/settings.json` hooks (`SessionStart`, `UserPromptSubmit`, `Stop`, `PermissionRequest`, `SubagentStart`, `SubagentStop`, …), OMP extensions `~/.omp/agent/extensions/{herdr-omp-agent-state.ts,orca-agent-status.ts}`.
- Resume commands: `codex resume <id>`, `claude --resume <id>`, `omp --resume=<id>`, `opencode --session <id>`.

## Scope
`crates/protocol` (contract change owned by this slice: **0.3 → 0.4**, capability `agent.report`), `crates/daemon`, `crates/cli` (`heeler agent report|release` subcommands), `crates/client` (store fields), `integrations/` (new top-level dir with hook scripts/extensions), `docs/integrations.md` (new), `docs/protocol.md`. Desktop shows the state badge only if trivial; full monitor UI is M13.

## Contract
- Env injected into every session process (daemon-managed, wins over spec env): `HEELER_ENV=1`, `HEELER_SESSION_ID` (exists), `HEELER_BIN` (absolute path of the `heeler` CLI next to `heelerd`, else on PATH), `HEELER_SOCKET` (exists).
- Protocol 0.4 requests: `agent.report { session_id, source, agent, state: idle|working|blocked, message?, seq, agent_session_id?, resume_argv?: [String] }`, `agent.release { session_id, source, seq }`. Validation: unknown session → `not_found`; `seq` must increase per (session, source) else ignored (`{applied:false}`); resume argv rules as Herdr's (plain command name on PATH, ≤ 64 args, ≤ 8 KiB, no `'` or control chars) else `bad_request`.
- `SessionInfo.agent: Option<AgentReport { source, agent, state, message, agent_session_id, updated_at_ms }>` (additive, `skip_serializing_if = Option::is_none`), emitted via `session.updated`. Persist `resume_argv` into `resume_ref_json` (the field M3-A restores with); `release` clears it. A shell prompt returning (OSC 133 A from shell integration) after the agent process exits also clears a stale report (safety net, like Herdr).
- CLI: `heeler agent report --source X --agent omp --state working [--seq N] [--message M] [--agent-session-id ID] [-- argv…]` (session from `HEELER_SESSION_ID`, silent no-op outside Heeler, never blocks > 200 ms, exit 0 on daemon errors so agent hooks never break the agent); `heeler agent release`.
- Integrations (`integrations/`), each idempotently installable/uninstallable by `heeler integration install|uninstall|status <codex|claude|omp>` that edits only its own marked entries in the agent's config (back up the file first, never remove others' hooks — Herdr/Orca entries must survive):
  - **codex**: hooks for `SessionStart` (resume argv `codex resume <session_id>`), `UserPromptSubmit` → working, `Stop` → idle, permission prompts → blocked if Codex exposes them.
  - **claude**: `SessionStart` (`claude --resume <session_id>`), `UserPromptSubmit` → working, `Stop`/`StopFailure` → idle, `PermissionRequest` → blocked, `SessionEnd` → release; `SubagentStart/Stop` recorded as counters in `message` for now (M13 builds the tree).
  - **omp**: an OMP extension (TypeScript, like the existing ones) reporting turn start/end, approvals, session id → `omp --resume=<id>`.
- Desktop: session rows/tabs show the agent name + state dot when `agent` is set (optional; skip if it needs more than a small change).

## Acceptance
- Protocol golden fixtures for the new messages; `docs/protocol.md` 0.4.
- Daemon tests: report/release/seq ordering/argv validation; restart restores an agent session by running its resume argv (use a fake agent script on PATH that records its argv).
- Integration tests for `heeler integration install/uninstall` on temp copies of real-shaped config files (Codex hooks.json with existing herdr/orca entries; Claude settings.json with existing hooks; OMP extensions dir) — existing entries preserved byte-for-byte where possible, backups written, uninstall leaves the original.
- Manual (temp `HEELER_HOME` and a temp `HOME`/`CODEX_HOME`/config dir — **never install into the user's real agent configs**): start a fake-agent session, report states via the CLI, see them in `heeler ls --json`.
- `just check` green; committed on `slice/M3-B`.

## Rules
Inline `HEELER_HOME=<temp dir>`; never touch `/tmp/hh-*`, the real Heeler home, or the user's real `~/.codex`, `~/.claude`, `~/.omp`. No push.
