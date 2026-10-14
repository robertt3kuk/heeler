# Agent integrations

Coding agents running in a Heeler terminal session tell the daemon what they are doing and how to start them again. The daemon shows the state on the session (`SessionInfo.agent`, protocol 0.4, capability `agent.report`) and, after a daemon restart, runs the reported command instead of the session's original one, so a restart finds Codex, Claude Code, and OMP in the same conversation. The monitor of M13 builds on the same data.

Wire contract and daemon semantics: `docs/protocol.md` § Agent reporting. This file is about the pieces on the agent side.

## What a session process gets

Every session process runs with `HEELER_ENV=1`, `HEELER_SESSION_ID`, `HEELER_WORKSPACE_ID`, `HEELER_SOCKET`, and `HEELER_BIN` (the `heeler` CLI). Anything that runs inside the session — an agent, one of its hooks, an extension — can report without configuration.

## Reporting from the command line

```sh
heeler agent report --source my-hooks --agent omp --state working \
    [--seq N] [--message TEXT] [--agent-session-id ID] [-- omp --resume=ID]
heeler agent release --source my-hooks
```

- `--state` is `idle`, `working` (a turn is running) or `blocked` (waiting for a permission or an answer).
- Everything after `--` is the **resume command**: a plain command name on `PATH` (no path, so it keeps working when binaries move), at most 64 words and 8 KiB, no apostrophes or control characters. The daemon runs it in place of the session's command after a restart. Report it as early as the agent knows its conversation id, and again whenever the id changes.
- `--seq` must increase per `(session, source)`; the default is the current time in nanoseconds. Pass your own when reports are produced by one process and delivered by several.
- `release` says the agent is gone. The daemon also forgets the agent when the session's process ends and when the shell prompts again (OSC 133 `A` from the shell integration, `docs/shell-integration.md`).

Both commands are built to be called from hooks: outside Heeler (no `HEELER_SESSION_ID`), without a daemon, on any daemon error, or after 200 ms they do nothing, print nothing, and exit 0. They never start a daemon.

`heeler agent hook <codex|claude>` is the same for hook events: it reads the hook's JSON from stdin and turns it into the report below. The installed hook scripts call it.

## Installing the integrations

```sh
heeler integration install   codex|claude|omp [--dir DIR]
heeler integration status    codex|claude|omp [--dir DIR]
heeler integration uninstall codex|claude|omp [--dir DIR]
```

The agent's configuration directory is `--dir`, else `$CODEX_HOME` / `$CLAUDE_CONFIG_DIR` / `$PI_CODING_AGENT_DIR`, else `~/.codex` / `~/.claude` / `~/.omp/agent`. `--json` prints the report as JSON.

What `install` does, and only this:

- Writes Heeler's own file: `<codex dir>/heeler-agent-state.sh`, `<claude dir>/hooks/heeler-agent-state.sh` (the same script, `integrations/hooks/`), or `<omp dir>/extensions/heeler-agent-state.ts` (`integrations/omp/`). Files carry a `heeler-managed` marker in their first lines; a file of that name without the marker is never overwritten or removed.
- Codex and Claude Code: adds one hook group per event to `hooks.json` / `settings.json`, running `: heeler-integration=<agent>; sh '<script>' <agent>`. The marker in the command is how Heeler finds its entries. Every other key, group and hook — including those of Herdr and Orca — stays; the document keeps its key order and indentation (a file that was written by Node or by Heeler comes back byte for byte after an uninstall).
- Before it changes an existing configuration file it copies it to `<name>.heeler-<unix ms>.bak`. A file that is not valid JSON, or not shaped like the agent documents, is left alone and the install fails.
- Idempotent: a second install changes nothing and writes no backup. An install of a newer version updates its own entries and script in place.

`uninstall` removes exactly those hooks (and the groups, events, and `hooks` table they leave empty), the script, and the extension. `status` says `installed`, `partial` (with what is missing or outdated) or `not installed`.

Codex asks you to review new or changed hooks the first time it starts after an install (its `hooks.state` trust list in `config.toml`); Heeler does not touch that file, so approve the Heeler hooks there once.

## What each integration reports

| Agent | Event | Report | Resume command |
|---|---|---|---|
| Codex | `SessionStart` | idle | `codex resume <session_id>` |
| | `UserPromptSubmit`, `PostToolUse` | working | same |
| | `PermissionRequest` | blocked, `permission: <tool>` | same |
| | `Stop` | idle | same |
| Claude Code | `SessionStart` | idle | `claude --resume <session_id>` |
| | `UserPromptSubmit`, `PostToolUse` | working | same |
| | `PermissionRequest` | blocked, `permission: <tool>` | same |
| | `Stop`, `StopFailure` | idle | same |
| | `SubagentStart` / `SubagentStop` | working, `subagents: N` while any run | same |
| | `SessionEnd` | release | — |
| OMP | `session_start`, `session_switch` | idle | `omp --resume=<id>` |
| | `agent_start` | working | same |
| | `tool_approval_requested`, the `ask` tool | blocked, the reason or question | same |
| | `agent_end` | idle (after 400 ms, so an automatic retry stays working) | same |

Sources are `codex-hooks`, `claude-hooks`, `omp-extension`. `PostToolUse` is hooked because hooks never say that a permission prompt was answered; the next tool result is the signal. Subagent counts live in `$TMPDIR/heeler-agent/<session id>.subagents` until the turn ends. Hook events of a Codex whose `CODEX_THREAD_ID` is another conversation (a Codex started by Codex's own tools) are ignored. OMP reports only from the interactive session; subagents and print mode have no UI and stay silent. Session ids that are not made of letters, digits, `-_.:` are never put in a resume command (the state is still reported).

Resume commands are what the agents document: `codex resume <id>`, `claude --resume <id>`, `omp --resume=<id>`. For other agents (`opencode --session <id>`, …) call `heeler agent report` from their own hooks.

## Safety

Nothing here runs with more rights than the agent has. The hook script is a few lines of `sh` that checks `HEELER_ENV`, finds `HEELER_BIN`, and calls `heeler agent hook`; the daemon validates every resume command before it stores it. No credentials pass through any of it.

Tests: `crates/daemon/tests/agent.rs` (protocol semantics, restart resumes a fake agent through its reported argv), `crates/cli/tests/{agent,integration,omp_extension}.rs` (CLI against a real daemon; installs on temporary copies of real-shaped configuration; the OMP extension under Bun, skipped when Bun is missing).
