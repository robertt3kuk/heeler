# 0014. Automatic projects alongside user workspaces

- Status: accepted (2026-10-15, with the user; revised the same day: workspaces stay as the user's own mechanism, both ways are supported)
- Milestone: M18

## Context
M12 made the user open workspaces (`workspace.open`, ⌘O) and put sessions into the active one; a workspace has roots, its own `.hutan/settings.toml` layer, a layout, a pin and an order. Since M7-A every session also knows its checkout (`SessionInfo.placement`). The user wants sessions grouped by project without ceremony **and** keeps workspaces as the deliberate way to organize work (several repos, own settings and layout), with easier management than today.

## Decision
- **Two kinds of container**, both `WorkspaceInfo` on the wire:
  - **Project** (`kind: project`), automatic: identity = the deepest enclosing git repository's *common dir* (main checkout and all its worktrees are one project), else the folder; `$HOME`, `/` and the Home folder are not projects. Created by the daemon when a session, chat, task, worktree or restore needs one and `projects.auto` is on (default on).
  - **Workspace** (`kind: workspace`), made by the user: a name, a pin, an order, settings and a layout, containing **projects** (each project is in at most one workspace: `WorkspaceInfo.parent_id`). Adding a folder to a workspace adds (or creates) the project for that folder. This is today's workspace with its roots expressed as member projects.
  - **Home** (`kind: home`) unchanged.
- **Where a new session goes**: an explicit `workspace_id` wins; else the project of its cwd (auto) — and that project's workspace, if any, is the session's workspace for settings and layout; with `projects.auto = false`, the original rule (active workspace, else Home).
- **Settings layering**: user < workspace (`<first project root>/.hutan/settings.toml` of the workspace, as today) < project (`<project root>/.hutan/settings.toml`). A project outside any workspace layers only its own.
- **Sidebar** (`sidebar.organize = "workspaces" | "projects"`, default `workspaces`): workspaces → their projects → sessions, then projects that belong to no workspace, then Home; or a flat list of projects. Every session row shows its title, its live folder (last two path parts) and its placement badge (`main` / `wt <branch>`).
- **Easier management**: one-step create (`workspace.create {name, paths?, project_ids?}`), drag a project onto a workspace (`workspace.assign {project_id, workspace_id?}`), "New Workspace from Selection" (multi-select projects), inline rename, delete keeps the projects (they become ungrouped), ⌘O "Open Folder…" offers "in workspace ▸". CLI `hutan ws new NAME [PATH…]`, `hutan ws add WS PATH|PROJECT`, `hutan ws remove WS PROJECT`, `hutan ws rename|pin|unpin|rm`, `hutan ws ls` as a tree; `hutan project ls|open|rename|close`.
- **Migration**: every existing workspace stays a workspace (name, pin, order, layout); each of its roots becomes a member project. Home stays Home.
- **Wire**: additive (protocol minor bump, capability `projects.auto`): `WorkspaceInfo.kind` gains `project`/`workspace`, `WorkspaceInfo.parent_id?`, `WorkspaceInfo.project {common_dir?, root}`, `SessionInfo.folder`; `workspace.create`, `workspace.assign`, `workspace.delete`. No separate `group.*` family.

## Consequences
- Users who never create a workspace still get tidy per-project lists; users who do keep full control.
- Nested repos and submodules are their own projects; the deepest repository wins.
- Restore re-derives the project only when the stored one's root is gone.
