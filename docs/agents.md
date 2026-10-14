# Agent chat sessions (Codex)

Decisions: ADR 0005 (agent integration strategy), ADR 0006 (SQLite), ADR 0008 (credentials). Wire messages: `docs/protocol.md` § Agent chat and approvals. Roadmap: M4.1–M4.3, M4.5 (the chat view is M4.4).

A chat session is a conversation with a coding agent that Heeler's daemon runs: one `codex app-server` process per session, spoken to over stdio JSON-RPC, mapped to one internal event model, journaled in Heeler's own database, and shown by any client (today the CLI). The agent's login is Codex's own (`codex login`); Heeler never reads or stores its credentials (ADR 0008).

## Parts

| Where | What |
|---|---|
| `crates/core/src/agent.rs` | `AgentEvent` (+ `AgentEventDraft`, `ApprovalPolicy`, `SandboxMode`, `ModelInfo`): the unified event model |
| `crates/agents` | `AgentAdapter` trait, `CodexAppServerAdapter`, the JSON-RPC transport, and the fixture tool (`heeler-codex-fixture`) |
| `crates/daemon/src/chat.rs` | `ChatManager`: adapters, one pump task per session (numbers, journals and publishes events), the approval engine |
| `crates/daemon/src/registry.rs` | the session records: create / stop / restart / restore of chat sessions |
| `crates/storage` (schema v3) | `chat_items`, `chat_turns`, `approvals` |
| `crates/client`, `crates/cli` | `Client::chat_*`, `Client::approvals/approve`, the store's pending approvals; `heeler chat …`, `approvals`, `approve` |

## The adapter

`CodexAppServerAdapter::start(config, spec)` spawns `codex app-server`, then:

1. `initialize` (client name `heeler`, experimental API off) and the **version check**: the server's `userAgent` carries its version; only `>=0.160, <0.170` is accepted (`SUPPORTED_VERSIONS`). Anything else — or a version it cannot read — fails with a message naming the version found and the supported range. The wire structs in `crates/agents/src/codex/wire.rs` follow the schema of 0.162.1; they list only the fields Heeler reads and tolerate everything else.
2. `model/list` and **model choice**: the requested model must be in the catalog (the error lists what is); without one the catalog's default is used. The model named in the user's `~/.codex/config.toml` is never trusted: an older CLI can reject it after a successful handshake (`docs/spikes/2026-10-13-agent-protocols.md`).
3. `thread/start` with `model`, `cwd`, `approvalPolicy`, `sandbox` and `approvalsReviewer: "user"` (approvals always come to Heeler), or — when the session has a stored provider thread id — `thread/resume` with `excludeTurns: true`: the provider's copy of the history is not needed, Heeler's journal is the history.

Then a pump task turns every message of the process into events. `send_message` is `turn/start` (it answers early; the turn's progress arrives as notifications), `interrupt` is `turn/interrupt`, `respond` answers a server request.

### What becomes what

| Codex | `AgentEvent` |
|---|---|
| `turn/started`, `turn/completed` (`completed`/`interrupted`/`failed`) | `turn_started`; `turn_completed` / `turn_cancelled` / `turn_failed` (a JSON backend error is reduced to its `error.message`, the raw text kept in `details`) |
| `item/started`/`completed` `userMessage`, `agentMessage` (+ `item/agentMessage/delta`) | `message_start` / `message_delta` / `message_end` (role, phase, the complete text) |
| `reasoning` (+ summary/text deltas) | `reasoning_start` / `reasoning_delta{summary,content}` / `reasoning_end` |
| `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `webSearch` | `tool_start` (name, one-line title, input) … `tool_update` … `tool_end` (status, output cut at 256 KiB with `truncated`, exit code, duration) |
| `item/commandExecution/outputDelta` | `command_output` (live only) |
| `fileChange` changes, `item/fileChange/patchUpdated`, `turn/diff/updated` | `file_diff` (per file `patch`, or the turn's aggregate `patch`) |
| approval requests of the server | `permission_requested`; `serverRequest/resolved` or the turn's end → `permission_resolved` |
| `turn/plan/updated`, `plan` item | `plan` (steps, or prose) |
| `thread/tokenUsage/updated` | `usage` (fields the server did not send stay absent) |
| `error`, `warning` | `notice` |
| anything else | `opaque { method }` with the raw `payload` (account, rate-limit and startup chatter is dropped) |

Messages of other threads (sub-agents) are ignored. A notification that does not fit its schema also becomes `opaque`: the adapter never fails on what it does not understand. Server requests Heeler cannot serve (`item/tool/requestUserInput`, MCP elicitation, token refresh, …) are answered with JSON-RPC "method not found" and noted as a `notice`, so the agent never waits for them.

## Approvals

Codex asks before running a command, applying a patch, or widening its permissions (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`). The adapter turns each into a `permission_requested` event whose `options` are exactly the decisions the server listed in `availableDecisions` (when it lists none, the schema's `accept`, `acceptForSession`, `decline`, `cancel`); permission profiles offer `grant`, `grant_session`, `deny`. Each option keeps the provider's own decision (`options[].provider`) and gets a `kind` for styling.

The daemon owns the rest (`ChatManager`):

- A request becomes pending in daemon state (`approval.list`, `approval.required`). It survives any client disconnect; the session shows `waiting_approval`.
- `approval.respond` is accepted only with an `option_id` the request offered (`option_not_offered` otherwise, and nothing reaches the agent); the adapter checks again. A request is answered once.
- A request **expires** when its turn ends (completed, failed, interrupted), when Codex withdraws it, when the agent process exits, and when the daemon restarts. `approval.resolved` says `selected` (with the option) or `expired`.
- Audit: every request is a row of `approvals` (kind, title, offered options, time); its end adds the time, outcome, chosen option and which kind of client answered.

Codex's own approval policy decides whether a command asks at all (`untrusted`: everything but known-safe commands; `on-request`: the agent decides; `never`); a safe command like `echo` may run without asking even under `untrusted`.

## Journal and restore

The pump gives every event the session's next `seq` and a timestamp. Everything but the streaming deltas is written to `chat_items` (`event_json` = the whole event), turns to `chat_turns`; `chat.history` reads it back (marked `replay`). Streaming events are delivered live and not stored — the `*_end` events carry the full text — so `seq` has gaps. The journal grows with use and is deleted with its session; there is no compaction yet.

The `ChatSpec` of a chat (agent, model, policies, **provider thread id**) is the session row's `spec_json`. After a daemon restart (`restore_sessions = "respawn"`, the default) live chat sessions start again: `thread/resume` with the stored thread id, the same session id, history unchanged. `restore_sessions = "stopped"`, a stopped session and a failed one stay listed; `heeler restart <id>` (`session.restart`) resumes them. An open turn at the time of a crash is closed in the journal (`turn_failed` / `turn_cancelled`) and approvals left open become `expired`. Provider resume only continues the agent: Codex's resumed thread omits tool items and renumbers ids, which is why the history shown comes from Heeler's journal.

## Configuration

`daemon.toml`:

```toml
[codex]
bin = "/Users/me/.local/bin/codex"                    # default: `codex` on the daemon's PATH
args = ["app-server", "-c", 'model_reasoning_effort="low"']   # default: ["app-server"]
```

A GUI-launched daemon may have a shorter `PATH` than a shell: set `bin` when `heeler chat new` says Codex could not be started. `CODEX_HOME` is inherited from the daemon's environment.

## CLI

```text
heeler chat new [--cwd DIR] [--model M] [--approval-policy untrusted|on-request|never] [--sandbox read-only|workspace-write|danger-full-access] [--ws W] [--title T]
heeler chat send <session> "<text>" [--wait]   # --wait streams the reply; tool calls are one line each; exit 1 if the turn fails or is cancelled
heeler chat log <session>                      # the journal as text (--json: the history)
heeler chat attach <session>                   # recent history, then live events until the session ends
heeler chat interrupt <session>
heeler chat models
heeler approvals                               # pending requests (--json)
heeler approve <approval-id|prefix> <option>
```

`--json` prints events as JSON lines (`send --wait`, `attach`) or the protocol result.

## Tests and fixtures

Real Codex never runs in the default gate. Tests run against **`heeler-codex-fixture replay`**, a fake `codex app-server` that replays a recorded transcript (`crates/agents/tests/fixtures/codex-0.162/*.jsonl`, one `{"dir": "out"|"in", "msg": …}` object per line, in the order observed). Requests are matched by method; a response gets the id of the request it answers; after `thread/resume` the recorded thread id is rewritten to the requested one; a mismatch exits 3 with a message. A directory instead of a file serves one transcript per spawn (restart tests); `--received FILE` logs what the client sent, which the tests assert on (e.g. that the provider got exactly the chosen decision, or `thread/resume` with the stored thread id).

Fixtures: `plain-turn`, `approval-touch` (command approval), `approval-decline`, `approval-interrupt` (approval pending when the turn is interrupted), `file-change` (patch approval with default options), `interrupt`, `turn-failed`, `resume-turn`. Tests: adapter mapping per notification kind (`crates/agents/tests/mapper.rs`), the adapter against the fake server (`tests/codex.rs`: approvals, non-offered options, expiry, resume, version refusal, unknown messages, process death), the daemon end to end (`crates/daemon/tests/chat.rs`: lifecycle, approvals across clients, journal, restart), the CLI (`crates/cli/tests/chat.rs`), storage, and golden fixtures for protocol 0.9.

### Re-recording (new Codex version, new scenario)

Use synthetic prompts, a temporary working directory, and a temporary `CODEX_HOME` holding a *copy* of the login (delete it afterwards); never record from your real `~/.codex`.

```sh
# 1. compare the schema the installed Codex generates with what wire.rs expects
codex app-server generate-json-schema --out /tmp/schema     # and generate-ts

# 2. record: point a throwaway daemon's codex at the proxy, then drive it with the CLI
cat > $HEELER_HOME/daemon.toml <<'EOF'
[codex]
bin = "/path/to/target/debug/heeler-codex-fixture"
args = ["proxy", "/tmp/raw.jsonl", "--", "codex", "app-server"]
EOF
HEELER_HOME=… CODEX_HOME=… heeler chat new --cwd /tmp/scratch --approval-policy untrusted
HEELER_HOME=… CODEX_HOME=… heeler chat send <id> 'Run `touch hi.txt` in a shell, then reply done.' --wait   # approve from another shell
# the proxy rewrites /tmp/raw.jsonl at every spawn: stop after the scenario you want

# 3. scrub (refuses output that still looks private) and commit
heeler-codex-fixture scrub /tmp/raw.jsonl crates/agents/tests/fixtures/codex-0.162/<name>.jsonl
```

The scrubber drops timestamps and rate-limit notices, renames every id to a stable counter, replaces temp directories, the Codex home, host name, account plan and installation id, and fails on home directories, e-mail addresses, API keys and tokens. A test asserts the committed fixtures are already scrubbed (scrubbing changes nothing). For a new Codex minor version, update `SUPPORTED_VERSIONS`, re-record the fixtures, and run `just check`.

## Limits

One agent (Codex), one conversation per session, text input only (no images or attachments), no chat view in the desktop yet (M4-B). Questions of the agent (`requestUserInput`) and MCP elicitations are declined. History search over chat journals is not wired into the M17 index yet: the journal has user, assistant and tool rows with the shape the indexer needs (`source: heeler`).
