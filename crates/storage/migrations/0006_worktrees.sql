-- Schema v6: worktrees Hutan created (roadmap M7-A, protocol 0.14).
--
-- git is the source of truth for which worktrees exist; this table only remembers which of them
-- Hutan made, so the UI can offer cleanup. No foreign key to `workspaces`: the worktree outlives
-- the workspace that asked for it.
CREATE TABLE worktrees (
    path          TEXT PRIMARY KEY NOT NULL,
    workspace_id  TEXT NOT NULL,
    repo_root     TEXT NOT NULL,
    branch        TEXT NOT NULL,
    base          TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT;
