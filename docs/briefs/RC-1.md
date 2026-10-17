# RC-1: comment on a diff, the agent fixes it

Agent: implementer   Depends on: master `1094079` (protocol 0.23, R-1 rich input)   Branch: slice/RC-1

## Context
Read AGENTS.md, `docs/spikes/2026-10-17-ux-audit.md` § J7 and gap #7, `docs/orchestration.md` (Changes view), `docs/protocol.md` § Rich input (R-1 attachment kinds), `docs/assistant.md` (attachments). Code: desktop `workbench/changes.rs` (Changes · <branch>, unified hunks), `workbench/chat/*` (composer, attachments), `sessions/tasks.rs`, `client` `chat_send` with attachments, terminal `send_text` for TUI agents.

Orca, Conductor, Codex and Claude Code let you select diff lines, write a comment and send it to the agent. Hutan's diff is read-only.

## Scope
`crates/desktop` (Changes view + a review-comments tray), small additive client helpers if needed. No protocol change: comments travel as an ordinary user message with R-1 attachments (`file` with line range + `pasted_text` of the hunk), so the chat journal keeps them.

## Contract
- In the Changes view: click a line number (or drag over several, shift-click to extend) to select lines in one hunk; a **+ Comment** affordance appears in the gutter; ⌘↩ in the comment box adds it. Comments show inline under their lines (author "you", time, Edit, Delete) and in a **Review** tray at the top ("3 comments · Send to agent ▾").
- Comments are drafts held per (worktree/branch, base) in desktop state and survive window reopen (desktop-state, bounded to 200 per diff); stale anchors (the file changed after Refresh) are shown as "outdated" with the original snippet.
- **Send to agent**: one message to the target session — by default the task's / worktree's agent session; the ▾ lets you pick another live chat or terminal agent in that checkout, or "Start a new agent with these comments" (opens S-1's launcher if merged, else ⌘⇧T flow) — composed as:
  - text: a short instruction header ("Please address these review comments:") + each comment numbered with `path:line-range`;
  - attachments: for each comment, a `file` attachment with the range (in the worktree checkout) and a `pasted_text` with the hunk excerpt (bounded 200 lines).
  - For a terminal agent (no chat API): the same content as plain text typed into its PTY via the existing send-text path (bracketed paste), without attachments.
- After sending, comments move to "Sent · <time>" (kept, collapsible); Refresh after the agent's next turn marks them resolved when the anchored lines changed, else keeps them open; manual Resolve/Unresolve.
- Single-comment fast path: "Send now" on one comment.
- Keyboard: in the Changes view `c` on a selected line opens the comment box, `]c`/`[c` next/previous comment (vim users), ⌘↩ send review.

## Acceptance
- GPUI tests with the in-process daemon and fake chat: select lines → comment → tray count; Send composes the expected text and attachments to the default target; target picker; terminal target gets bracketed-paste text; outdated anchors after Refresh; persistence across reopen; resolve.
- `just check` green; committed on `slice/RC-1`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run `hutand`/`hutan-desktop` without it. No paid turns. No push, no hard reset. Do not edit docs/status.md or docs/roadmap.md. Concurrent: DL-1 adds Accept/Request changes/merge-scope sections to the same Changes view — coordinate with `write agent://DL1Delivery` (DL-1 owns the header and merge sections, you own line selection, comments and the Review tray). The user runs `/Applications/Hutan.app` on the real home: never touch it.
