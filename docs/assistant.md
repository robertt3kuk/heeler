# Assistant chat (roadmap M16: the daemon side, and the desktop panel in § Desktop)

Decisions: ADR 0008 (no secrets in context; credentials), ADR 0001 (the daemon owns conversations). Wire messages: `docs/protocol.md` § Assistant chat and context attachments. The direct chat it is built on: `docs/gateway.md`. The index it reads and feeds: `docs/history-index.md`. The desktop panel for it is described in § Desktop.

An **assistant** is a direct chat (a provider's chat API, no tools, no approvals) that lives in a scope, starts from a system prompt that explains attachments, and can be sent **context**: shell output, recent terminal lines, the conversation of a coding agent, a file, search results. The context is chosen by the user for each message, read by the daemon, redacted, bounded and journaled with the message. Nothing is read that was not attached.

## Parts

| Where | What |
|---|---|
| `crates/daemon/src/assistant.rs` | `AssistantService`: `assistant.open`/`assistant.list`, permission checks, the resolver of each attachment kind |
| `crates/daemon/src/context.rs` | the pure half: redaction, budget share, truncation, headers, assembly, turns of a conversation |
| `crates/daemon/src/context_file.rs` | reading a file below the workspace roots without following symlinks |
| `crates/daemon/src/{chat,direct}.rs` | `chat.agent = "assistant"` is a gateway chat; the message with its attachments is the user message of the turn |
| `crates/daemon/src/history.rs`, `crates/index` | chat journals feed the index; `IndexHandle::session_text` reads a session's rows |
| `crates/client`, `crates/cli` | `Client::chat_send_with`, `assistant_open`, `assistant_list`; `heeler ask`, `heeler assistant` |

## Scope and sessions

A session's scope follows from its workspace: the Home workspace holds the **global** assistants (they see the Home folder, nothing else), every other workspace its **workspace** assistants (they see the workspace's roots). `assistant.open { scope, workspace_id? }` returns the default assistant of the scope — the oldest assistant session of that workspace — and creates it on first use with `[assistant] provider` and `model` from `daemon.toml` (the model defaults to the provider's default model). Without a configured provider it fails with `invalid_state` and says which setting to add. A stopped default is started again. `new: true` (or `chat.create` with `agent: "assistant"` and a provider) makes one more; `assistant.list` shows them all and which is the default.

Sessions created in the same millisecond keep their insertion order, including after updates
and daemon restart. Restore must not use random ULID order as the tie breaker: otherwise
opening the default assistant could select a different conversation.

An assistant session is an ordinary chat session: `chat.send`, `chat.history`, `chat.interrupt`, `chat.set_model`, `session.stop`/`restart`, restore after a daemon restart all work as for a direct chat. Its system prompt is `assistant::SYSTEM_PROMPT`.

## Attachments

`chat.send { session_id, text, attachments }`. At most 16 per message. Only direct and assistant chats take attachments. Every attachment is validated (its own bounds, then the permission of the session's scope) before the first one is read, and the first one that cannot be resolved fails the message: nothing is journaled, nothing reaches the provider.

| Kind | Fields | Read from | Cut keeps |
|---|---|---|---|
| `shell_block` | `terminal_id`, `block_id` | the terminal's output of that block (`terminal.block_text` path, M10) | the end |
| `shell_recent` | `terminal_id`, `lines` ≤ 500 | the last lines of the screen and scrollback (soft wraps joined; on a full-screen program only the visible screen) | the end |
| `agent_transcript` | `session_id`, `last_turns` ≤ 20 | a Heeler chat's journal (Codex, direct, assistant: the `message_end` events), else the history index rows of that session id (Claude Code, Codex, OMP; needs indexing) | the newest turns |
| `file` | `path`, `range?` `{start, end}` (1-based, inclusive) | the file, under the session's workspace roots only | the start |
| `search` | `query`, `limit` ≤ 10 | `history.search` (needs indexing): one line per session with its id, so that it can be attached next | the best hits |

A turn is a user message and what follows it. A relative file path is relative to the first root of the workspace. Terminal and file text is plain text; a file that is not UTF-8 is `bad_request`.

**What the message becomes.** The user's text, then

```text
--- attached context (read-only, redacted, bounded by the user's settings) ---
[shell: cargo test -p daemon · exit 101 · 12 lines · cut to the last 3000 of 9000 characters]
…output…

[file: src/lib.rs · lines 1-40 · 40 lines]
…text…
--- end of attached context ---
```

Headers: `[shell: <command> · exit <code>|running · <n> lines]`, `[shell: terminal <id> · last <n> lines]`, `[agent: <title> · <heeler chat|claude|codex|omp> · <n> turns]`, `[file: <path> · lines a-b · <n> lines]`, `[search: "<query>" · <n> hits]`. A part that was cut says so (`cut to the first|last N of M characters`; M is left out when the source was only read in part, as for a big file), a source that was incomplete says `incomplete` (the terminal lost lines). This whole text is the journaled message: `chat.history` and the next turns' conversation contain it, so the conversation is reproducible and visible. The user's `message_end` event carries `payload.attachments_meta = [{kind, label, chars, truncated}]` so that a client can show chips without parsing text.

**Redaction.** Every text goes through the history index's redactor (`heeler_index::redact`: API keys, tokens, `Authorization:`/`Bearer`, PEM keys, `password=…` pairs, …; best effort, see `docs/history-index.md` § Privacy) *before* it is cut, so a secret straddling a cut is removed whole. Headers (a command line or a path may hold a secret) are redacted too. Files are read a little beyond their share (512 characters) so that redaction sees the end of a secret at the cut.

**Budget.** `assistant.context_budget_chars` (default 60 000) is split evenly over the attachments of one message (at least 1 character each); the user's own text and the earlier conversation are not part of it (the conversation is still limited by `gateway.max_context_chars`, oldest messages first; the new message, attachments included, is always sent).

**Files.** `context_file::read_file` canonicalises the path and the roots, requires the path to be inside a root (so `..`, an absolute path elsewhere, a symlink out and a sibling directory with the root's name as a prefix are refused with `forbidden`), and then opens it relative to a descriptor of the root with `openat(O_NOFOLLOW)` per component, the root's own ancestors included, so a symlink swapped in after the check fails the open instead of being followed. It reads only regular files, at most 8 MiB scanned, and builds the UTF-8 text from whole buffers (a character split by a read boundary is not an error).

## Permissions

```toml
# daemon.toml
[assistant]
provider = "openrouter"            # a provider of `heeler provider ls`; used for assistants created from now on
model = "meta-llama/llama-3.3-70b-instruct"
context_budget_chars = 60000

[assistant.context]                # assistants of project workspaces
shell = true                       # shell_block, shell_recent
agents = true                      # agent_transcript
files = true                       # file
search = true                      # search

[assistant.global_context]         # the global (Home) assistant: files are off by default
files = false

[assistant.workspaces.heeler]      # per workspace, by name or id: only what differs from [assistant.context]
files = false
```

An attachment the settings do not allow is `forbidden`, and the message names the setting (`assistant.context.shell`, `assistant.global_context.files`, `assistant.workspaces.<key>.files`); the error's `data.setting` has it too. The file is read when the daemon starts: restart it after editing. Settings gate the daemon's own reading; they do not make an attachment out of anything that was not attached.

Other refusals: `bad_request` (bounds, more than 16 attachments, a session that is not a direct or assistant chat), `not_found` (terminal, block, file, transcript), `invalid_state` (`search` or an external transcript while indexing is disabled).

## Indexing of chats

With `[history] enabled` the daemon feeds the journals of **all** chat sessions (Codex, direct, assistant) into the index as source `heeler`: user and assistant messages (`user`/`assistant` rows), tool calls and their output (`tool` rows), as they are journaled, keyed `c:<seq>`. The session id is the key and `history.search` hits carry it as `session_id` (also for terminal sessions), so a client can open the session; there is no `resume_argv` (resume = focus the session). Like all Heeler rows these are not backfilled from before indexing was on. A message with attachments is indexed with its attachments (redacted, at most 16 chunks).

## Desktop

The **Assistant** panel (`assistant`, registered like every panel: movable between docks and the center, listed under ⌘K → **Panel: Open Assistant**) shows one assistant chat with the ordinary chat transcript and composer of agent chats. ⌥Space (⌘K → **Assistant: Open / Focus**) reveals it and puts the keyboard in its composer; registering ⌥Space for the whole OS belongs to M6. Opening an assistant calls `assistant.open` and shows the returned direct-chat session: no terminal and no vendor agent process is started.

**Scope.** The header chooses **Global** (the Home assistant: it sees the Home folder only) or **Workspace · name** (the active workspace's assistant; disabled while Home is active). Each scope has its own session, so switching keeps both transcripts, drafts and chips; nothing moves from one to the other. A history hit that is an assistant conversation (`session_id` of a `heeler` hit) opens in the panel, in its own scope, instead of as a tab; the Sessions panel still opens any assistant as an ordinary chat tab, with the same composer.

**Without a provider.** The daemon names the assistants' provider and model under `[assistant]` in `daemon.toml` and reads it at start. When `assistant.open` is refused with `invalid_state` the panel says so in the daemon's words, links to **Models settings** and offers Retry. The desktop never picks a provider or model of its own and makes no model call. A configured provider whose key is missing or rejected shows **Set key…** like any gateway chat, a missing model **Pick another model…**; the header's **Switch model…** applies to assistants too.

**Context.** Context is opt-in per message and never read by the desktop. The composer's **＋ Attach** menu opens the form of each kind; typing a mention in the composer offers the same form (↹ or the hint's button accepts it and removes the mention from the draft):

| Mention | Form | Chip (`kind · source · bounds`) |
|---|---|---|
| `@shell` | the daemon's live terminals (those of the assistant's workspace first); **Recent output** (the field is the number of lines, default 50, at most 500) or **Blocks ▾** to pick one of the newest blocks | `shell recent · title · last 50 lines`, `shell block · title · $ cmd · block #7 · exit 1` |
| `@omp`, `@agent` | sessions that reported an agent session id, Heeler chats, and the indexed transcripts of the words typed after the mention (`@omp` is OMP only); a stepper for the turns (default 5, at most 20) | `agent · title · omp · last 5 turns` |
| `@file path[:first-last]` | the path and optional lines; with the argument typed the chip is made at once | `file · src/lib.rs · lines 1-40` |
| `@search words` | the words and a hits stepper (default 5, at most 10) | `search · “words” · 5 hits` |

Chips are removable (×) before the send and become structured attachments of `chat.send` (at most 16). An attachment that is not a chip is never read or added to the prompt. After the send the user's message shows what the daemon resolved for each attachment (`attachments_meta`: kind, label, characters kept, and a warning tint with "cut to the budget" when truncated), with the attached text folded away. A refusal (`forbidden`, `not_found`, `invalid_state`, bounds) shows the daemon's message in the banner, keeps the draft and the chips, marks the chips the settings refuse as *refused* and journals and sends nothing. File paths are checked by the daemon (see Files above): the form says so and never opens the file.

**Send to agent.** "Send to agent" of a finished terminal block (button, ⌘K, key) attaches it to the chat the user looked at last — the assistant the Assistant panel shows counts once the user opened or focused it after the last chat tab — or to the only running chat, or to the one picked from the menu. When that chat is an assistant the block becomes a `shell_block` attachment (a block reference the daemon reads when the message is sent; the panel is shown and the chip waits in its composer); other chats keep the quoted-text chip. When several chats run and none is an assistant the menu adds **Send to Assistant**: the assistant of the terminal's workspace, opened on demand. With no chat running the notice points at ⌥Space.

## CLI

```text
heeler ask "<question>" [--ws <id|name>] [--attach <spec>]…
heeler assistant ls [--ws <id|name>]
heeler assistant open [--ws <id|name>] [--new]
```

`--attach` specs: `block:<terminal>:<block>`, `recent:<terminal>:<lines>`, `agent:<session>[:<turns>]` (default 5), `file:<path>[:<first>-<last>]`, `search:<query>` (5 hits; quotes optional). `ask` uses the default assistant of `--ws`, else `$HEELER_WORKSPACE`, else the global one (Home), prints the answer as it streams and exits 1 when the turn fails (also when the daemon refuses an attachment: the message is on stderr). `--json` prints every event. `heeler chat log <session>` shows the whole conversation, attachments included.

## Tests

Resolver unit tests (`crates/daemon/src/{context,assistant,context_file}.rs`): bounds, cuts and headers, redaction before the cut, row joining, turn counting, path confinement including `..`, absolute paths, symlinks to files and directories, symlinked ancestors of a root and prefix siblings. Daemon tests (`crates/daemon/tests/assistant.rs`) run a real daemon with the in-memory credential store, a mock OpenAI-compatible server (`heeler_gateway::testing::MockServer`), real terminals with OSC 133 blocks and the history index on temporary directories: `assistant.open`/`list` per scope, restart and stop; every attachment kind resolved, journaled (with `attachments_meta`) and sent, the provider's request asserted (headers present, unattached terminal/file text absent, secrets redacted); refusals by setting and by confinement, bounds, nothing sent; the budget split and the cut headers; external transcripts from the index; chat journals found by `history.search` with `session_id`. CLI tests (`crates/cli/tests/assistant.rs`) drive the real `heeler` binary with every attachment kind.

Desktop tests (`crates/desktop/tests/assistant_gpui.rs`, GPUI against an in-process daemon with the in-memory credential store and the same mock server): the global and a workspace assistant stream with no terminal and each scope keeps its transcript, draft and chips; every chip kind added through the mentions and the Attach menu, removed, and only the selected sources in the provider's prompt; truncation metadata after the send; refusals by setting and by confinement (chips marked, draft restored, nothing journaled or sent); a relative path no desktop process can resolve read by the daemon; a history hit of an assistant conversation opened in the panel and continued; the unconfigured prerequisite with no model call; a terminal block sent to the assistant as a `shell_block` attachment.

## Limits

Attachments are text only; a file larger than the scan bound (8 MiB) is read from its start. `shell_recent` joins soft-wrapped rows but cannot tell a hard line break from a wrap on a program that draws its own screen. An external transcript is read from the index (redacted chunks of at most 16 per message, merged back into messages), not from the transcript file. Settings are read at daemon start. The assistant cannot run commands or change files. The desktop cannot read the daemon's `[assistant]` settings: it learns that a provider is missing from `assistant.open`'s refusal and that a context kind is not allowed from `forbidden` on the first message that uses it, not before.
