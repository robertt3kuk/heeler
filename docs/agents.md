# Agent chat sessions (Codex)

Decisions: ADR 0005 (agent integration strategy), ADR 0006 (SQLite), ADR 0008 (credentials). Wire messages: `docs/protocol.md` § Agent chat and approvals. Roadmap: M4.1–M4.3, M4.5 (the chat view is M4.4).

A chat session is a conversation with a coding agent that Hutan's daemon runs: one `codex app-server` process per session, spoken to over stdio JSON-RPC, mapped to one internal event model, journaled in Hutan's own database, and shown by any client (today the CLI). The agent's login is Codex's own (`codex login`); Hutan never reads or stores its credentials (ADR 0008).

## Parts

| Where | What |
|---|---|
| `crates/core/src/agent.rs` | `AgentEvent` (+ `AgentEventDraft`, `ApprovalPolicy`, `SandboxMode`, `ModelInfo`): the unified event model |
| `crates/agents` | `AgentAdapter` trait, `CodexAppServerAdapter`, the JSON-RPC transport, and the fixture tool (`hutan-codex-fixture`) |
| `crates/daemon/src/chat.rs` | `ChatManager`: adapters, one pump task per session (numbers, journals and publishes events), the approval engine |
| `crates/daemon/src/registry.rs` | the session records: create / stop / restart / restore of chat sessions |
| `crates/storage` (schema v3) | `chat_items`, `chat_turns`, `approvals` |
| `crates/client`, `crates/cli` | `Client::chat_*`, `Client::approvals/approve`, the store's pending approvals; `hutan chat …`, `approvals`, `approve` |

## The adapter

`CodexAppServerAdapter::start(config, spec)` spawns `codex app-server`, then:

1. `initialize` (client name `hutan`, experimental API off) and the **version check**: the server's `userAgent` carries its version; only `>=0.160, <0.170` is accepted (`SUPPORTED_VERSIONS`). Anything else — or a version it cannot read — fails with a message naming the version found and the supported range. The wire structs in `crates/agents/src/codex/wire.rs` follow the schema of 0.162.1; they list only the fields Hutan reads and tolerate everything else.
2. `model/list` and **model choice**: the requested model must be in the catalog (the error lists what is); without one the catalog's default is used. The model named in the user's `~/.codex/config.toml` is never trusted: an older CLI can reject it after a successful handshake (`docs/spikes/2026-10-13-agent-protocols.md`).
3. `thread/start` with `model`, `cwd`, `approvalPolicy`, `sandbox` and `approvalsReviewer: "user"` (approvals always come to Hutan), or — when the session has a stored provider thread id — `thread/resume` with `excludeTurns: true`: the provider's copy of the history is not needed, Hutan's journal is the history.

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

Messages of other threads (sub-agents) are ignored. A notification that does not fit its schema also becomes `opaque`: the adapter never fails on what it does not understand. Server requests Hutan cannot serve (`item/tool/requestUserInput`, MCP elicitation, token refresh, …) are answered with JSON-RPC "method not found" and noted as a `notice`, so the agent never waits for them.

## Approvals

Codex asks before running a command, applying a patch, or widening its permissions (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`). The adapter turns each into a `permission_requested` event whose `options` are exactly the decisions the server listed in `availableDecisions` (when it lists none, the schema's `accept`, `acceptForSession`, `decline`, `cancel`); permission profiles offer `grant`, `grant_session`, `deny`. Each option keeps the provider's own decision (`options[].provider`) and gets a `kind` for styling.

The daemon owns the rest (`ChatManager`):

- A request becomes pending in daemon state (`approval.list`, `approval.required`). It survives any client disconnect; the session shows `waiting_approval`.
- `approval.respond` is accepted only with an `option_id` the request offered (`option_not_offered` otherwise, and nothing reaches the agent); the adapter checks again. A request is answered once.
- A request **expires** when its turn ends (completed, failed, interrupted), when Codex withdraws it, when the agent process exits, and when the daemon restarts. `approval.resolved` says `selected` (with the option) or `expired`.
- Audit: every request is a row of `approvals` (kind, title, offered options, time); its end adds the time, outcome, chosen option and which kind of client answered.
- Provenance (protocol 0.15, capability `chat.provenance`): the same fact travels with the answer: `approval.resolved` and the journaled `permission_resolved` event carry `decided_by: {client_kind, client_name?}` (`client_kind` is the answering connection's `hello` kind: `desktop`, `cli`, `notifier`, `other`). The first answer the agent accepts is credited; a refused attempt (an option that was not offered, an approval already answered) is not. An expiry has no `decided_by`. The adapter does not know who answered; the daemon (`ChatManager::attribute`) fills it in before the event is journaled, so `chat.history` replays it. `hutan chat log` prints `approval <id>: accept (decided by cli)`.

Codex's own approval policy decides whether a command asks at all (`untrusted`: everything but known-safe commands; `on-request`: the agent decides; `never`); a safe command like `echo` may run without asking even under `untrusted`.

## Journal and restore

The pump gives every event the session's next `seq` and a timestamp. Everything but the streaming deltas is written to `chat_items` (`event_json` = the whole event), turns to `chat_turns`; `chat.history` reads it back (marked `replay`). Streaming events are delivered live and not stored — the `*_end` events carry the full text — so `seq` has gaps.

**Snapshots of streaming text (0.15).** So that a window opened mid-stream shows the text so far, `Live` keeps a `Partials` (`crates/daemon/src/partial.rs`) beside the journal: the text of every open assistant message and reasoning block of the running turn (one entry per open item, at most 1 MiB of text each). The pump journals a `message_partial` / `reasoning_partial` event for the items that changed at most every 500 ms (a one-shot timer armed when text arrives that no snapshot has; nothing runs while nothing streams) and for what is left when a turn ends. A snapshot is **replaced in place**: writing the new row deletes the item's previous one in the same transaction, so the journal holds one row per open item however many deltas arrive, and the item's `message_end` / `reasoning_end` deletes it. Numbering an event and folding it into `Partials` happen under one lock, which is what makes a snapshot's meaning exact: it includes every delta of its item with a smaller `seq`. `chat.history` takes the lock, answers an open item with a fresh snapshot (a new `seq` from the session's counter, never journaled by that call; the stale journal row of the item is left out) and reads `last_seq` under the same lock; live deltas that follow the reply have a greater `seq`. A snapshot that is all that is left of a message (the turn failed or was interrupted before `message_end`, or the daemon died mid-stream) stays in the journal and the chat shows it as the text the message had. The journal grows with use and is deleted with its session; there is no compaction yet.

The `ChatSpec` of a chat (agent, model, policies, **provider thread id**) is the session row's `spec_json`. After a daemon restart (`restore_sessions = "respawn"`, the default) live chat sessions start again: `thread/resume` with the stored thread id, the same session id, history unchanged. `restore_sessions = "stopped"`, a stopped session and a failed one stay listed; `hutan restart <id>` (`session.restart`) resumes them. An open turn at the time of a crash is closed in the journal (`turn_failed` / `turn_cancelled`) and approvals left open become `expired`. Provider resume only continues the agent: Codex's resumed thread omits tool items and renumbers ids, which is why the history shown comes from Hutan's journal.

## Configuration

`daemon.toml`:

```toml
[codex]
bin = "/Users/me/.local/bin/codex"                    # default: `codex` on the daemon's PATH
args = ["app-server", "-c", 'model_reasoning_effort="low"']   # default: ["app-server"]
```

A GUI-launched daemon may have a shorter `PATH` than a shell: set `bin` when `hutan chat new` says Codex could not be started. `CODEX_HOME` is inherited from the daemon's environment.

## CLI

```text
hutan chat new [--cwd DIR] [--model M] [--approval-policy untrusted|on-request|never] [--sandbox read-only|workspace-write|danger-full-access] [--ws W] [--title T]
hutan chat send <session> "<text>" [--wait]   # --wait streams the reply; tool calls are one line each; exit 1 if the turn fails or is cancelled
hutan chat log <session>                      # the journal as text (--json: the history)
hutan chat attach <session>                   # recent history, then live events until the session ends
hutan chat interrupt <session>
hutan chat models
hutan approvals                               # pending requests (--json)
hutan approve <approval-id|prefix> <option>
```

`--json` prints events as JSON lines (`send --wait`, `attach`) or the protocol result.

## Tests and fixtures

Real Codex never runs in the default gate. Tests run against **`hutan-codex-fixture replay`**, a fake `codex app-server` that replays a recorded transcript (`crates/agents/tests/fixtures/codex-0.162/*.jsonl`, one `{"dir": "out"|"in", "msg": …}` object per line, in the order observed). Requests are matched by method; a response gets the id of the request it answers; after `thread/resume` the recorded thread id is rewritten to the requested one; a mismatch exits 3 with a message. A directory instead of a file serves one transcript per spawn (restart tests); `--received FILE` logs what the client sent, which the tests assert on (e.g. that the provider got exactly the chosen decision, or `thread/resume` with the stored thread id).

A transcript entry `{"dir": "hold", "file": PATH}` pauses the replay until `PATH` exists (at most a minute): the tests that need a stream to stand still half way (a window opened mid-stream) put one between two deltas and create the file when they are ready.

Fixtures: `plain-turn`, `approval-touch` (command approval), `approval-decline`, `approval-interrupt` (approval pending when the turn is interrupted), `file-change` (patch approval with default options), `interrupt`, `turn-failed`, `resume-turn`. Tests: adapter mapping per notification kind (`crates/agents/tests/mapper.rs`), the adapter against the fake server (`tests/codex.rs`: approvals, non-offered options, expiry, resume, version refusal, unknown messages, process death), the daemon end to end (`crates/daemon/tests/chat.rs`: lifecycle, approvals across clients, journal, restart), the CLI (`crates/cli/tests/chat.rs`), storage, and golden fixtures for protocol 0.9.

### Re-recording (new Codex version, new scenario)

Use synthetic prompts, a temporary working directory, and a temporary `CODEX_HOME` holding a *copy* of the login (delete it afterwards); never record from your real `~/.codex`.

```sh
# 1. compare the schema the installed Codex generates with what wire.rs expects
codex app-server generate-json-schema --out /tmp/schema     # and generate-ts

# 2. record: point a throwaway daemon's codex at the proxy, then drive it with the CLI
cat > $HUTAN_HOME/daemon.toml <<'EOF'
[codex]
bin = "/path/to/target/debug/hutan-codex-fixture"
args = ["proxy", "/tmp/raw.jsonl", "--", "codex", "app-server"]
EOF
HUTAN_HOME=… CODEX_HOME=… hutan chat new --cwd /tmp/scratch --approval-policy untrusted
HUTAN_HOME=… CODEX_HOME=… hutan chat send <id> 'Run `touch hi.txt` in a shell, then reply done.' --wait   # approve from another shell
# the proxy rewrites /tmp/raw.jsonl at every spawn: stop after the scenario you want

# 3. scrub (refuses output that still looks private) and commit
hutan-codex-fixture scrub /tmp/raw.jsonl crates/agents/tests/fixtures/codex-0.162/<name>.jsonl
```

The scrubber drops timestamps and rate-limit notices, renames every id to a stable counter, replaces temp directories, the Codex home, host name, account plan and installation id, and fails on home directories, e-mail addresses, API keys and tokens. A test asserts the committed fixtures are already scrubbed (scrubbing changes nothing). For a new Codex minor version, update `SUPPORTED_VERSIONS`, re-record the fixtures, and run `just check`.

## The desktop chat view

`⌘⇧T` (New agent) asks for the agent (Codex; the others are greyed until M8) and a model from `chat.models`, starts the chat in the active workspace's first folder (`chat.create`) and opens it as a **chat tab**: same tab/split/move rules as terminals, the tab icon `◆`. Code: `crates/chat_view` (GPUI-free: `Transcript` folds `AgentEvent`s into entries, the markdown subset, unified diffs) and `crates/desktop/src/workbench/{agents.rs,chat/}` (model, view, cards, composer).

- **Transcript**: virtualized list. User and assistant messages (markdown: paragraphs, emphasis, inline code, fenced code with a Copy button, lists, quotes, links; no HTML, no images; tables stay preformatted), reasoning (folded, "Thinking…" while it streams), command cards (command, cwd, live output tail, exit code, click to show all), file-change cards (files with +/− counts, click a file for its unified diff; shown automatically while an approval waits), other tools (name, arguments folded), plans as a checklist, a footer per turn (outcome, duration, tokens, context use).
- **History and follow-up**: the newest 200 events of the journal (`chat.history`, `HISTORY_DEFAULT_LIMIT`) are read when a tab opens, then `chat.event`s are applied. Every event is applied once (by `seq`), a reconnect reads the journal again and applies only what is new, a daemon restart (new epoch) starts the transcript over. Text that streamed while the window was away is complete again when the item ends, and a window opened mid-stream starts from the journal's snapshot of the open message and continues with the live deltas after it (`Transcript` keeps the `seq` its text includes: a delta already in a snapshot is not added twice, one that came live before the snapshot is put back after it).
- **Load earlier**: while the journal has events before the first one shown, a row above the transcript says so with a **Load earlier** button, and scrolling to the first entry does the same: the page before the oldest `seq` read (`before_seq`, 200 events) is put in front of the transcript (`Transcript::prepend`: an event seen before is skipped, an item that continues across the page boundary is completed in place, the turn in progress and the usage of its footer are not touched) and the list keeps the entry the reader was at. Paging continues until `has_more` is false. The list is virtualized, so only visible entries are laid out; the loaded entries are all in memory.
- **Composer**: `⌘↩` sends, `↩` is a newline (`chat.send_on_enter = true` swaps them; `⇧↩` is then the newline); `⌘.` or the Interrupt button stops the turn; the composer is disabled while the session starts, ended, or the connection is down (Restart button for ended sessions).
- **Approvals**: the request sits on its tool's card with one button per option the agent offered (keys `1`–`4` answer the oldest request while the composer is empty). The same requests are in the inspector's Approvals tab, the status bar (`approvals` item), the sessions panel (state `blocked`, needs you) and ⌘K → *Agent: Review Approvals…*. A resolved card says what was decided and where the answer came from: "Allowed once · from this window", "· from CLI", "· from another window" (a desktop connection that is not this one), "· from a notification", "· from another client" (an answer journaled before protocol 0.15, or a kind this build does not know) — from the `decided_by` the daemon journals with the answer, so a window opened later shows it too.
- **Send to agent**: the hover bar's button and the *Block: Send to Agent* command attach the block's command and output (cut at `chat.attach_max_bytes`, default 64 KiB) as a chip in the composer of the chat that was focused last (a menu asks when none was); the chip is quoted in a fenced block in front of the message when it is sent.
- Settings namespace `chat`: `send_on_enter`, `attach_max_bytes`. `panels.inspector.reveal_on_approval` opens the inspector when a new request arrives.

Tests: `crates/chat_view` (unit), `crates/desktop/tests/chat_gpui.rs` (an in-process daemon over `hutan-codex-fixture replay`).

## Limits

One agent (Codex; direct model chat has its own page, `docs/gateway.md`), one conversation per session, text input only (no images; terminal blocks are quoted into the text). Questions of the agent (`requestUserInput`) and MCP elicitations are declined. History search over chat journals is not wired into the M17 index yet: the journal has user, assistant and tool rows with the shape the indexer needs (`source: hutan`).
