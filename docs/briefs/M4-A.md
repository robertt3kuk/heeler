# M4-A: Codex chat sessions in the daemon — adapter, AgentEvent, approvals, journal, CLI (roadmap M4.1–M4.3, M4.5)

Agent: implementer   Depends on: M17-A merged (protocol 0.8; this slice takes **0.9**)   Branch: slice/M4-A

## Context
Read `AGENTS.md`, ADR 0005 (agent integration strategy — normative), ADR 0006 (SQLite), ADR 0008 (credentials), `docs/spikes/2026-10-13-agent-protocols.md` (+ `/tmp/staffy-spikes/agents/REPORT.md` if present), `docs/roadmap.md` § M4, `docs/protocol.md`, `crates/storage`, `crates/daemon/src/{registry,restore}.rs`, M3-B agent reports (`SessionInfo.agent`), `SessionKind` in `crates/core`.

Installed: `codex-cli 0.162.1` (the spike used 0.144.2 — regenerate everything from 0.162.1). `codex app-server generate-json-schema --out <dir>` / `generate-ts` produce the wire schema.

## Scope (contract changes authorized for this slice)
- `crates/core`: the internal `AgentEvent` model exactly as ADR 0005 § Decision lists (local session/turn/item ids + provider ids, seq, ts, `live|replay`, opaque provider payload; variants message start/delta/end, reasoning, tool start/update/end, command output, file diff, permission requested/resolved with the original offered options, plan, usage with optional fields, turn completed/failed/cancelled, resume locator). `SessionKind::Chat` (or existing equivalent) for chat sessions.
- New crate `crates/agents` (`staffy-agents`): `AgentAdapter` trait + `CodexAppServerAdapter` (spawn `codex app-server` over stdio JSON-RPC, initialize, `model/list`, `thread/start` / `thread/resume`, `turn/start`, `turn/interrupt`, notifications → `AgentEvent`, server→client approval requests → pending approvals). Typed structs only for the messages used, generated/checked against the 0.162.1 schema; unknown notifications kept as opaque payload events, never a crash. Supported version range pinned (`>=0.160, <0.170`), checked at spawn with a clear error.
- `crates/daemon`: chat session lifecycle (create with cwd + model + approval policy + sandbox; one adapter process per chat session), **approval engine** (pending decisions in daemon state, survive client disconnect, answerable only with an offered option, expire when the turn resolves, audit rows), journal persistence (storage migration: `chat_items` / `chat_turns` / `approvals` tables — Staffy's journal is the source of history; provider resume only continues the agent), restore after daemon restart (`thread/resume` with the stored provider thread id; history shown from our journal).
- `crates/protocol` **0.9**, capability `chat.codex`: `chat.create { workspace_id?, cwd, agent: "codex", model?, approval_policy?, sandbox? }`, `chat.send { session_id, text }`, `chat.interrupt`, `chat.history { session_id, before_seq?, limit }`, `chat.models { agent }`, `approval.list`, `approval.respond { approval_id, option_id }`; events `chat.event { session_id, event: AgentEvent DTO }` (streamed, seq'd per session; attach/replay from journal like terminals), `approval.required`, `approval.resolved`. Golden fixtures for all.
- `crates/client` helpers + store; `crates/cli`: `staffy chat new [--cwd] [--model]`, `staffy chat send <id> "<text>" [--wait]` (prints streamed text; tool calls as one-line summaries), `staffy chat log <id>`, `staffy chat attach <id>` (live tail), `staffy approvals` + `staffy approve <id> <option>`.
- No desktop UI (M4-B).

## Testing rules
- **Fake app-server**: a test binary/script that replays recorded JSONL transcripts (requests matched by method, responses/notifications/server-requests emitted in order). Record transcripts from the real 0.162.1 **only with synthetic prompts in a temp dir**, scrub them (no tokens, no account/email, no real paths beyond the temp dir), commit them as fixtures under `crates/agents/tests/fixtures/codex-0.162/`.
- Real Codex runs are allowed at most for recording fixtures and one final manual check: tiny prompts (e.g. "run `echo hi` in a shell and tell me the output"), in a temp cwd, read-only sandbox where possible, ephemeral threads if the schema supports it. Do not change `~/.codex/config.toml`; pass overrides with `-c`. Pick the model from `model/list` (the config default may be rejected — see the spike).
- Tests: adapter mapping per notification kind; approval flow (request → pending → disconnect → reconnect still pending → respond with an offered option → provider receives it; non-offered option rejected; expiry on turn end); journal + restart (history from journal, `thread/resume` called); version-range refusal; unknown notification tolerated; CLI end-to-end against the fake server.

## Acceptance
- All tests above pass; `just check` green; protocol 0.9 documented in `docs/protocol.md`; `docs/agents.md` (new) explains the adapter, event model, approvals, journal, and how to re-record fixtures.
- Manual: with the real Codex, `staffy chat new` → `send` a tiny prompt that triggers a command approval → `staffy approvals` shows it → `staffy approve` → reply streams; report the transcript summary (no secrets).
- Committed on `slice/M4-A`.

## Rules
Inline `STAFFY_HOME=<temp dir>`; never touch `/tmp/hh-*` or the real Staffy home. No push, no hard reset; `git grep -n '^<<<<<<<'` empty after any rebase. Never commit content from the user's real `~/.codex` sessions.
