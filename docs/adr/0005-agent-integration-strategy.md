# 0005. Agent integration strategy

- Status: accepted
- Date: 2026-10-13
- Milestone: M4 (Codex), M8 (ACP)

## Context
Three ways to run agents: (1) TUI in a PTY (works for every agent, no structure), (2) Codex app-server JSON-RPC over stdio (rich, Codex-specific), (3) ACP v1 over stdio (one protocol for Claude Code, OpenCode, OMP, Gemini, codex-acp). Spike `docs/spikes/2026-10-13-agent-protocols.md` exercised (2) end-to-end with a real approval and (3) through `initialize` + `session/new`.

Key facts from the spike:
- `codex app-server` (0.144.2) generates its own JSON Schema/TS (`generate-json-schema`, `generate-ts`): 87 client requests, 10 server requests, 68 notifications. Thread → turn → item lifecycle; approvals are typed server→client requests (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, …) with `availableDecisions`.
- `turn/start` acks early; completion arrives as `turn/completed`. ACP `session/prompt` instead stays pending until `stopReason`.
- `thread/resume` after process restart works but **drops tool items and renumbers item IDs** → provider history is not a faithful journal.
- Version skew: config default model `gpt-6-astra` rejected by CLI 0.144.2 although handshake succeeded; `model/list` catalog default worked.
- ACP permission = `session/request_permission` with opaque option IDs (`allow_once/allow_always/reject_once/reject_always`); updates via `session/update` variants; `session/load` replays, `session/resume` doesn't.
- `omp acp`, `codex-acp` 1.12.0, `opencode` are installed locally; claude-code ACP adapter is not.

## Options
| Option | Pros | Cons |
|---|---|---|
| ACP only | One adapter for many agents | Loses Codex-specific detail; Codex via codex-acp adds a hop |
| Codex app-server only | Richest Codex UX | Single vendor |
| **Codex app-server first, ACP v1 second, both behind one internal `AgentEvent` model** | Best Codex fidelity now, broad coverage next | Two adapters to maintain |

## Decision
- `agents` crate hosts adapters implementing one `AgentAdapter` trait: `CodexAppServerAdapter` (M4), `AcpV1Adapter` (M8). Daemon owns subprocess (via `runtime`), transport, request-ID maps, pending approvals.
- Internal `AgentEvent` (in `core`) carries: local session/turn/item IDs + provider IDs, sequence, timestamp, `live|replay` marker, opaque provider payload, and variants for message start/delta/end, reasoning, tool start/update/end, command output, file diff (patch or before/after), permission requested/resolved (original offered options preserved), plan, usage (optional fields, never zero-filled), turn completed/failed/cancelled, resume locator.
- **Hutan's own journal is the source of conversation history** (SQLite, ADR 0006); provider resume is used to continue the agent, not to rebuild our UI history.
- Pending approvals live in daemon state, survive client disconnects, are answered only with an option the provider actually offered, and expire when the turn resolves.
- Pin the supported Codex version range; generate schema from the installed binary in CI fixtures; pick models from `model/list`, never trust a config default blindly.
- TUI agents in terminals remain first-class; their state detection (M7) uses integration hooks first, screen manifests as fallback (Herdr model).
- Rust ACP SDK: `agent-client-protocol` 2.x (ACP wire v1). Compile check is part of M8 kickoff `[UNVERIFIED build]`.

## Consequences
- Codex adapter test fixtures: recorded JSONL transcripts replayed by a fake app-server.
- Adding an agent = ACP config entry when it speaks ACP; custom adapter only when ACP loses essential behavior.

## Evidence
`docs/spikes/2026-10-13-agent-protocols.md`
