# 0014. Automatic projects instead of hand-made workspaces

- Status: accepted (2026-10-15, with the user)
- Milestone: M18

## Context
M12 made the user open workspaces (`workspace.open`, ⌘O) and put sessions into the active one. In practice a session's folder already says where it belongs, and since M7-A every session also knows its checkout (`SessionInfo.placement`: main or a worktree of a repository). The user wants sessions grouped by project without managing workspaces, each session showing its folder and worktree, and optional groups for several projects together.

## Decision
- **A project is a repository or a folder.** Its identity is the git *common* directory (`git rev-parse --git-common-dir`, read from `.git`/`commondir` on disk like M7-A placement), so the main checkout and every worktree of a repository are one project; a folder outside git is its own project (its path). Sessions outside any project (e.g. in `~`) go to **Home**.
- **Projects are created automatically** by the daemon from a session's start cwd (`session.create`, `chat.create`, tasks, restore). A session keeps the project it started in; its *current* folder (OSC 7) and placement (`main` / `wt <branch>`) are shown live. Nobody has to open a project first; ⌘O still exists to open a folder you have no session in yet.
- **Groups** (optional) collect several projects under one name (a multi-repo product, a client). A project is in at most one group. Groups have a name, an order and a pin; nothing else.
- **Wire compatibility**: the daemon keeps the `workspace.*` messages and `WorkspaceInfo` (a project *is* a workspace with `kind: project`), adds `WorkspaceInfo.project {common_dir?, folder, group_id?}` and `group.create|list|rename|delete|move|assign`, and auto-creation. Additive: protocol minor bump, capability `projects.auto`. Existing workspaces with several roots are migrated: each root becomes a project, and the old workspace becomes a group of them.
- **UI language**: the desktop and CLI say *project* and *group*; `workspace` remains only in the protocol. The sidebar lists groups → projects → sessions; a session row shows its title, its folder (last two path parts) and its placement badge.
- Per-project layouts (M12-B) stay, keyed by project. `.hutan/settings.toml` is read from the project's main checkout root.

## Consequences
- Fewer concepts for the user; `hutan ws` becomes `hutan project` (alias kept for one release is *not* added: clean cutover per AGENTS.md; `ws` is removed).
- A session started in a subfolder of a repo belongs to the repo, not the subfolder (deepest repository wins for nested repos/submodules).
- Restore must re-derive the project for sessions whose repository moved; a missing folder keeps the stored project.
