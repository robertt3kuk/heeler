# M7-C: Tasks panel, diff review and merge (desktop + small daemon additions)

Agent: implementer   Depends on: master `b7398fd` (M7-A worktrees, M7-B tasks protocol 0.16, D-1 visual system v2). Protocol: additive requests below take the next free minor at merge (M8-A 0.17, M18-A 0.18, M8-C 0.19, M13-C 0.20 are in flight; use **0.21**, the integrator renumbers).   Branch: slice/M7-C

## Context
Read AGENTS.md, then `docs/orchestration.md` (§ Worktrees, § Tasks), `docs/design.md` (§ Visual system v2, § Workbench model: panels are registered, movable, own settings namespace), `docs/protocol.md` § Tasks. Code: `crates/daemon/src/tasks.rs`, `worktrees.rs`, `crates/desktop/src/workbench/ui.rs` (component kit: use it), `workbench/panels/*` (panel registry pattern), `workbench/worktrees.rs`, `crates/chat_view/src/diff.rs` (unified-diff parser), `workbench/chat/cards.rs` (diff rendering in chat), `crates/client` (`task_*`, `worktree_*`, notices).

## Scope (contract changes authorized)
`crates/desktop` (new `tasks` panel + `changes` view), `crates/daemon` (git diff/merge requests in `worktrees.rs`), `crates/protocol` (additive), `crates/client`, `crates/cli` (`hutan wt diff|merge`), docs (`docs/orchestration.md`, `docs/design.md` § Tasks panel, `docs/protocol.md`).

## Contract
- **Tasks panel** (`tasks`, default in the right dock next to Agents in the default preset; movable): tasks of the active workspace/project as a dependency tree with state glyphs and colors from the theme, title, placement badge (`main` / `wt <branch>`), run kind, elapsed time, outcome; row actions Start / Done / Cancel / Reset / Delete (per state), click focuses the task's session; "+ New task" opens an inline form (title, prompt, after-dependencies multi-select, placement main/new worktree with branch, run: terminal command / Codex chat / omp chat (when M8-A is merged; hide otherwise), auto-start). Filters `tasks.show_done`, `tasks.scope = "workspace" | "all"`. Empty state with the mascot. ⌘K "Tasks: New Task…", "Tasks: Show Panel".
- **Changes view** for a worktree (or the main checkout) — from a task row, a session's placement badge menu, or ⌘K "Worktree: Review Changes…": file list (added/modified/deleted/renamed, +/− counts) and a unified diff viewer per file (reuse `chat_view::diff` parsing and the chat diff rendering, mono font, theme colors, collapsible hunks), against the merge base with the base branch (default: the repo's default branch) plus uncommitted changes, togglable. Read-only.
- **Daemon**: `worktree.diff {path, base?, include_uncommitted = true}` → `{base, files: [{path, old_path?, status, additions, deletions}], patch_truncated}` and `worktree.file_diff {path, file, base?}` → `{patch}` (bounded: 2 MiB per file, 20 MiB total, binary files flagged) via `git diff --merge-base` / `git diff` with the M7-A invocation rules (timeouts, no prompts, no pager, `--no-ext-diff`, `--no-color`). `worktree.merge {path, into?: branch (default base), strategy: merge | squash | rebase, message?}` → runs in the **main checkout**: refuses when the main checkout is dirty, when the target branch is not checked out there, or when there are conflicts (aborts cleanly and reports conflicting files); on success returns the new commit and offers removal. Never pushes.
- **Merge UI**: in the Changes view, "Merge into main…" with strategy and message; result toast; on success offer "Remove worktree and branch".
- **CLI**: `hutan wt diff <path|branch> [--stat]`, `hutan wt merge <path|branch> [--squash|--rebase] [-m MSG]`.

## Acceptance
- Daemon tests on temp repos: diff of a worktree with commits + uncommitted edits, renamed and binary files, truncation; merge (merge/squash/rebase) success; refusals (dirty main, target not checked out, conflict with clean abort and file list).
- GPUI tests: tasks panel shows the tree and reacts to `task.updated`; actions call the right requests; new-task form creates a task with dependencies and a worktree placement; changes view lists files and renders a diff; merge flow with a fake daemon reply.
- CLI e2e for `wt diff` / `wt merge`. Golden fixtures; docs; `just check` green; committed on `slice/M7-C`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; git only in temp repos the tests create. Use theme tokens and `workbench/ui.rs` components only (no color literals). No push, no hard reset; `pgrep -x hutand` clean. Do not edit docs/status.md or docs/roadmap.md. Disk is tight: targeted test runs while working, `just check` once at the end.
