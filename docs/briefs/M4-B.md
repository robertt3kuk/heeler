# M4-B: chat view in the desktop (roadmap M4.4)

Agent: implementer   Depends on: M4-A merged (protocol 0.9 `chat.*`, `approval.*`, `AgentEvent` DTOs), M12-B merged   Branch: slice/M4-B

## Context
Read `AGENTS.md`, ADR 0005, `docs/agents.md` (from M4-A), `docs/roadmap.md` § M4 (exit demo), `docs/design.md` (chat surfaces, attention colors, typography, density), ADR 0010 (workbench items, panels, settings), `crates/desktop/src/workbench` (item kinds, terminal items, palette, inspector, attention), `crates/client` (`chat_*`, `approval_*` helpers).

## Scope
`crates/desktop`, possibly a new `crates/chat_view` (mirrors `terminal_view`: model + view, no daemon types beyond protocol DTOs), small additive `crates/client` helpers. No daemon/protocol changes (report gaps).

## Behavior
- **Chat item** in the center pane tree (same tab/split/move rules as terminals); ⌘⇧T "New agent" (currently disabled with "Agents arrive with M4") opens a picker: agent (Codex; others greyed "M8"), model from `chat.models`, cwd = active workspace root; creates via `chat.create`.
- **Transcript**: virtualized list of turns → items: user message, assistant message (streaming deltas, markdown rendering: paragraphs, emphasis, inline code, fenced code with monospace + copy button, lists, links — no HTML, no images), reasoning (collapsed by default, "Thinking…" while streaming), **tool call cards** (command: argv + cwd + live output tail + exit code; file change: list of files with +/- counts and an expandable unified **diff view** with syntax-neutral red/green lines; other tools: name + JSON args collapsed), plan updates as a checklist, usage footer per turn when present. History loads from `chat.history` (journal) then follows `chat.event`; reconnect resumes by seq.
- **Composer**: multi-line input (⌘↩ send, ↩ newline per setting `chat.send_on_enter` default false), interrupt button (⌘.) while a turn runs, disabled state while the session restarts.
- **Approvals**: inline approval card at the tool call (command/diff preview, the provider's offered options as buttons, keyboard 1–4), plus global surfacing: sidebar attention badge, status bar count, ⌘K "Review approvals" listing all pending (`approval.list`) across sessions; a resolved approval shows who/how it was answered (UI, CLI).
- **Send to agent** from M10-C blocks (currently disabled "M4"): attaches the block's command + output (via `terminal.block_text`, truncated to a setting `chat.attach_max_bytes`, default 64 KiB) to the composer of a chosen/last-focused chat as a quoted context chip.
- Sessions panel and inspector show chat sessions with agent state (working/idle/blocked) — reuse M3-B attention presentation.

## Acceptance
- GPUI tests with an in-process daemon and M4-A's fake Codex app-server fixtures: new chat → send → streamed assistant text renders; command tool card with output; file change card with a diff; approval card → click option → daemon receives it, card resolves; approval answered from the CLI resolves the card live; reconnect mid-turn continues without duplicates; interrupt; "Send to agent" attaches a block.
- Manual: desktop on a temp `HEELER_HOME` against the fake server; one optional run against the real Codex with a tiny prompt (temp cwd, `-c` overrides only); no screenshots (orchestrator verifies visually).
- `just check` green; committed on `slice/M4-B`.

## Rules
Inline `HEELER_HOME=<temp dir>`; never touch `/tmp/hh-*`, the real Heeler home, or `~/.codex/config.toml`. No push, no hard reset; `git grep -n '^<<<<<<<'` empty after any rebase.
