# Orchestration: worktrees and session placement

Roadmap M7, part 1 (slice M7-A, protocol 0.14, capability `worktree`). Tasks and their dependencies are M7-B and not described here. Vision: placement is the user's choice — the repository's main checkout (one agent, quick fixes) or a git worktree (isolated parallel work); Hutan never forces a worktree, and it warns when two live agents write to the same checkout.

Wire details: `docs/protocol.md` § Worktrees and placement. Code: `crates/daemon/src/{worktrees,placement}.rs` (daemon), `crates/protocol/src/worktree.rs` (DTOs), `crates/cli/src/commands/worktrees.rs` (CLI), `crates/desktop/src/workbench/worktrees.rs` and `crates/desktop/src/sessions/placement.rs` (desktop).

## Principles

- **git is the source of truth.** Hutan runs `git worktree list --porcelain -z` for the repositories of a workspace and never keeps its own copy of what exists. It remembers one thing: which worktrees *it* created (SQLite table `worktrees`, migration 6), so that a UI can offer cleanup of those and leave the user's own worktrees alone.
- **The daemon owns it** (ADR 0001): creating, listing and removing worktrees, and deriving where sessions work. Clients only ask and render.
- **No network, no prompts.** Every git invocation has `GIT_TERMINAL_PROMPT=0`, `GIT_OPTIONAL_LOCKS=0` (listing never rewrites an index under a running agent), a timeout, and a closed stdin.
- **Never destructive by surprise.** Removing refuses the main worktree, live sessions, a locked worktree and uncommitted changes unless forced; `delete_branch` is `git branch -d`, never `-D`.

## Worktrees

A workspace's repositories are the git repositories its roots belong to; a root that is itself a linked worktree leads to its repository, and one repository reached twice is listed once.

| Operation | Behaviour |
|---|---|
| `worktree.create {workspace_id, repo?, branch, base?, path?}` | Creates a worktree at `path` (default `<parent of the main checkout>/<repo name>-wt/<branch with / replaced by ->`, e.g. `~/proj/hutan` + `slice/M7-A` → `~/proj/hutan-wt/slice-M7-A`). The branch is created from `base` (default `HEAD`) when it does not exist and checked out when it does; `base` is ignored for an existing branch. Refused: invalid branch names (`git check-ref-format --branch`), a `base` that is no commit, an existing path (`conflict`), a branch already checked out in another worktree (`conflict`, naming it). |
| `worktree.list {workspace_id?}` | Per repository the main worktree first, then the others in git's order, each with `branch?`, `head`, `main`, `created_by_hutan`, `dirty`, `live_sessions`, `locked`, `prunable`. |
| `worktree.remove {path, force?, delete_branch?}` | `git worktree remove`. Refused with `invalid_state` and the reason: the main worktree; live sessions in it (always, `force` does not override this); a locked worktree; uncommitted changes without `force`. With `delete_branch` the branch is deleted with `-d` afterwards; an unmerged branch stays, the answer says why (`branch_kept`), and the worktree is removed anyway. |
| `worktree.changed {workspace_id}` | Event after a create or remove, for every workspace with a root in the repository: list again. |

Creating and removing are serialised in the daemon. A repository that cannot be listed any more (its folder is gone) is skipped, not an error for the rest.

## Placement

`SessionInfo.placement` says where a session works: `{kind: main | worktree, root, branch?, shared_with}`.

- **Derivation.** From the session's live directory (OSC 7, so it follows `cd`), else its spec directory: the nearest ancestor containing `.git` is the checkout root. A `.git` directory means `main`; a `.git` file means a linked `worktree` when the private git directory it points to has a `commondir` (a submodule is `main`). The branch is read from that checkout's `HEAD`. No git process is spawned; the directory → checkout answer is cached (60 s, bounded) and cleared whenever `worktree.create` or `.remove` changes something. Sessions outside git have no placement.
- **`shared_with`.** The other live sessions that have an agent (`SessionInfo.agent`, any state; absent once the agent is gone) in the same checkout root — and only for a session that has an agent itself. Two agents in one checkout can overwrite each other's edits; a shell next to an agent is the usual and harmless case and is not flagged. The list is recomputed when an agent is reported or released, when a session changes directory or ends, and announced with the existing `session.updated` event for every session whose list changed.
- Nothing is forced: a session started in the main checkout while an agent works there is allowed, and merely marked.

## CLI

```text
hutan wt new <branch> [--ws ID|NAME] [--base REF] [--path PATH] [--repo PATH]
hutan wt ls [--ws ID|NAME] [--json]
hutan wt rm <path|branch> [--ws ID|NAME] [--force] [--delete-branch]
hutan new --worktree <branch> [--ws ...] [--title T] [--attach] [-- argv]
hutan ls
```

- The workspace is `--ws`, else `$HUTAN_WORKSPACE`, else the workspace whose root contains the current directory (`wt ls` falls back to all workspaces; the others stop with a hint to run `hutan ws open .`). When the current directory chose the workspace it also picks the repository (`--repo` overrides); otherwise the workspace's first git root is used.
- `wt new` prints the path of the worktree on stdout (`cd "$(hutan wt new feature/x)"`); `--json` prints `{"worktree": {...}}`.
- `wt ls` prints `BRANCH PATH STATE` (`main`, `hutan` = created by Hutan, `dirty`, `locked`, `prunable`, live session count); `--json` prints `{"worktrees": [...]}` with every field.
- `wt rm` takes a path (as written, or relative to the current directory) or a branch; a branch that exists in several repositories is a usage error listing the paths. `--delete-branch` reports a branch that stayed.
- `new --worktree <branch>` reuses the branch's worktree when there is one and makes it otherwise, then starts the session in it, in the workspace that contains the repository (it conflicts with `--cwd`). A branch checked out in the main checkout is refused.
- `hutan ls` has a `PLACE` column: `main`, `wt <branch>` (`wt (detached)`), `-` outside git, and `⚠ shared` appended while `shared_with` is not empty.

## Desktop

Deliberately small (the layout and panels are ADR 0010's business):

- Session rows (the `placement` row field, on by default) and tab tooltips show the badge `main` / `wt <branch>` (`docs/design.md` § session row); a warning border and `⚠` while the checkout is shared; the badge's tooltip names the checkout, branch and the sharing. A session outside git keeps the old folder-name label.
- Palette commands: **Worktree: New Terminal in Worktree…** (asks for a branch, reuses or creates its worktree in the active workspace's repository, opens a terminal there) and **Worktree: Remove…** (a picker of the removable worktrees of the active workspace, then a confirmation with a checkbox for deleting the branch; a worktree with uncommitted changes says so and the confirmation removes it with `force`).

## Tests

- `crates/daemon/src/{worktrees,placement}.rs`: porcelain parsing (detached, locked, prunable, bare, paths with spaces and newlines), default path derivation, branch-name validation against git, placement derivation (main, worktree, subdirectory, symlink, detached, cache and invalidation), `shared_with` transitions.
- `crates/daemon/tests/worktrees.rs`: a real daemon on temporary git repositories — create/list/remove, every refusal, `delete_branch` of a merged and an unmerged branch, placement of sessions in main and in a worktree and after `cd`, `shared_with` set by two agent reports and cleared by a stop, a release or a move, `created_by_hutan` across a daemon restart.
- `crates/cli/tests/worktrees.rs`: the real `hutan` for `wt new|ls|rm`, `new --worktree`, the `PLACE` column.
- `crates/desktop/tests/worktrees_gpui.rs`: rows draw the badge, tab tooltips carry it, the warning follows two agents, the commands are in the palette.
- `crates/protocol/tests/fixtures/v0/*worktree*`, `*placement*`: golden fixtures.

## Not here

Tasks and dependencies (M7-B), scheduling parallel agents, merging a worktree back, diff review, a worktrees panel. Remote repositories and fetching are never done by these requests.
