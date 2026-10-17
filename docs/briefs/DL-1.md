# DL-1: delivery states — finished ≠ reviewed ≠ merged

Agent: implementer   Depends on: master `1094079` (protocol 0.23)   Branch: slice/DL-1

## Context
Read AGENTS.md, `docs/spikes/2026-10-17-ux-audit.md` § J7 and gap #6, `docs/orchestration.md` (tasks, finish detection, worktree diff/merge), `docs/protocol.md` (tasks, worktrees). Code: `crates/core` task types, `crates/daemon/src/tasks.rs`, `worktrees.rs`, storage migration for tasks, `crates/protocol/src/task.rs`/`worktree.rs`, desktop `workbench/panels/tasks.rs`, `workbench/changes.rs`, CLI `commands/tasks.rs`.

Today a task is `done` when its agent goes idle after working; dependants start; nothing distinguishes "the agent stopped" from "a human accepted the work" from "it was merged".

## Scope (contract change authorized: protocol **0.24**, capability `task.review`)
`crates/core`, `crates/protocol`, `crates/storage` (migration), `crates/daemon`, `crates/client`, `crates/cli`, `crates/desktop` (Tasks panel, Changes view), docs + goldens.

## Contract
- A task gains a **delivery** facet, independent of its run state: `none` → `awaiting_review` (set automatically when the run finishes `done` and the task has a worktree or touched the main checkout) → `accepted` | `changes_requested` → `merged` (set by a successful `worktree.merge` of the task's branch, recording the merge commit and strategy) | `discarded` (worktree removed without merge). Each transition records who (`decided_by`: desktop/cli/agent) and when; history kept (bounded, last 20).
- Requests: `task.review { task_id, decision: accept | request_changes, note? }`; `task.delivery` events via `task.updated`. `request_changes` with a note sends the note to the task's chat session as a user message (when it is a chat) or shows it in the task (terminal agent), and sets the run back to `running` when the session takes it.
- **Dependency policy** per task: `after_policy = done | accepted | merged` (default `done`, today's behavior). A dependant with `accepted` waits until each dependency is accepted (or merged).
- **Merge scope truth**: `worktree.diff` and the Changes view say separately: *committed on branch* (what merge integrates), *uncommitted in the worktree* (what merge would NOT take; offer "Commit all…" with a message before merge), and the merge preview lists exactly the commits. After merge, if uncommitted changes remain, the cleanup offer says they would be discarded and defaults to Keep.
- Desktop: Tasks panel rows show a delivery pill (Awaiting review / Accepted / Changes requested / Merged / Discarded) next to the run state; the Changes view header has **Accept**, **Request changes…** (note), **Merge into …** buttons; the attention model counts `awaiting_review` as a soft "needs you" item (sidebar Needs you, not a badge).
- CLI: `hutan task review <task> accept|changes [-m NOTE]`, `hutan task ls` shows the delivery column; `hutan wt commit <branch> -m MSG`.

## Acceptance
- Daemon tests: auto `awaiting_review`; accept/request-changes; merge sets `merged` with commit; worktree removal without merge → `discarded`; `after_policy` gates; restart keeps delivery; agent-origin `task.review` follows ADR 0016 approval rules (ask).
- Desktop GPUI tests for the pills, buttons, merge-scope sections and cleanup default.
- `just check` green; committed on `slice/DL-1`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run `hutand`/`hutan-desktop` without it. No paid turns. No push, no hard reset. Do not edit docs/status.md or docs/roadmap.md. Concurrent: RC-1 also edits the Changes view (it adds line comments) — coordinate via `write agent://RC1DiffComments`; S-1 owns the launcher; Q-1/RR-1 touch protocol too (the integrator renumbers versions at merge). The user runs `/Applications/Hutan.app` on the real home: never touch it.
