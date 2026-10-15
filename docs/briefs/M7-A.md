# M7-A: worktrees and session placement (roadmap M7, part 1)

Agent: implementer   Depends on: master `1c3f7aa` (protocol 0.13; this slice takes **0.14**)   Branch: slice/M7-A

## Context
Read AGENTS.md, then `docs/vision.md` § Orchestration (placement is the user's choice: main checkout or worktree; never forced; warn when two live agents write to one checkout), `docs/architecture.md` (domain model REPO/WORKTREE, state ownership: worktrees are daemon state), `docs/design.md` § session row "Placement badge", `docs/protocol.md` catalog rows `worktree.*` (M7), ADR 0006 (storage). Reuse: `crates/daemon/src/projects.rs` (git-root detection, path canonicalization), `registry.rs` (sessions, cwd, agent state, `SessionInfo`), `crates/storage` (migrations), `crates/cli/src/commands/workspaces.rs` (target resolution, table output), `crates/desktop/src/workbench/panels/sessions.rs` (rows).

## Scope (contract changes authorized)
`crates/daemon` (new `worktrees.rs`), `crates/storage` (migration), `crates/protocol` (0.14, capability `worktree`), `crates/client`, `crates/cli`, `crates/desktop` (placement badge + palette command only), `docs/protocol.md`, new `docs/orchestration.md`. Tasks/dependencies are M7-B: do not add `task.*`.

## Contract
- **git is the source of truth.** Worktrees are discovered with `git worktree list --porcelain -z` per workspace root (run with a timeout, `GIT_TERMINAL_PROMPT=0`, no network). Hutan remembers only which ones it created (migration: `worktrees(path PK, workspace_id, repo_root, branch, base, created_at_ms)`), so the UI can offer cleanup.
- `worktree.create {workspace_id, repo?: path (default: the workspace's first root that is a git repo), branch, base?: ref (default HEAD), path?}` → `{worktree}`. Default path: `<parent of repo>/<repo name>-wt/<branch with / → ->` (the layout this project uses). New branch from `base` when `branch` does not exist, else checks it out; refuses a branch already checked out elsewhere (`conflict`, names where). The path must not exist.
- `worktree.list {workspace_id?}` → `{worktrees: [WorktreeInfo]}`; `WorktreeInfo {path, repo_root, branch?, head, main: bool, created_by_hutan: bool, dirty: bool, live_sessions: [SessionId], locked, prunable}`.
- `worktree.remove {path, force?: bool, delete_branch?: bool}` → `{}`. Refuses (`invalid_state`, with the reason) the main worktree, a worktree with live sessions inside it, or uncommitted changes without `force`. `delete_branch` uses `git branch -d` (never `-D`; an unmerged branch is reported, the worktree is still removed).
- **Placement** (additive on `SessionInfo`): `placement?: {kind: main | worktree, root, branch?, shared_with: [SessionId]}` derived from the session's live cwd (OSC 7) or spec cwd: the git toplevel containing it. `shared_with` lists other live sessions whose **agent** (`SessionInfo.agent`, any state but gone) is in the same checkout root; updated on cwd/agent changes and announced with the existing `session.updated` event. No git call per keystroke: cache toplevel per directory, invalidate on `worktree.*`.
- Events: `worktree.changed {workspace_id}` after create/remove (clients re-list).
- **CLI**: `hutan wt new <branch> [--ws] [--base] [--path] [--repo]`, `hutan wt ls [--ws] [--json]`, `hutan wt rm <path|branch> [--force] [--delete-branch]`; `hutan new --worktree <branch> [-- argv]` creates (or reuses) the worktree and starts the session in it. `hutan ls` shows `main`/`wt <branch>` and `⚠ shared` when `shared_with` is not empty.
- **Desktop (minimal)**: session rows and tab tooltips show the badge `main` / `wt <branch>` (design.md § session row) and a warning tint when shared; palette commands "Worktree: New Terminal in Worktree…" (branch name input) and "Worktree: Remove…". Nothing else.

## Acceptance
- Unit tests: porcelain parsing (incl. detached, locked, prunable, paths with spaces), default path derivation, branch name validation (`git check-ref-format`), placement derivation, `shared_with` transitions.
- Daemon tests on temporary git repos (`git init` in a temp dir, commits made by the test): create/list/remove, conflicts (existing path, branch checked out elsewhere, live session inside, dirty without force), `delete_branch` of a merged and an unmerged branch, placement of sessions started in main and in a worktree, `shared_with` when two sessions report agents in the same checkout and cleared when one stops, restart keeps `created_by_hutan`.
- CLI end-to-end for `wt new|ls|rm` and `new --worktree`. A GPUI test that a row shows the badge.
- Golden fixtures for every new message; `docs/protocol.md` and `docs/orchestration.md` updated; `just check` green; committed on `slice/M7-A`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; git operations only inside temp repos created by tests (never on this repository or the user's repos). No push, no hard reset; `git grep -n '^<<<<<<<'` empty after any rebase; `pgrep -x hutand` clean before finishing. Do not edit docs/status.md or docs/roadmap.md.
