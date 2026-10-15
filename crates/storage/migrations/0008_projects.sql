-- Schema v8: automatic projects and user workspaces (roadmap M18-A, protocol 0.19, ADR 0014).
--
-- A workspace row is now a `project` (automatic: a repository or a folder) or a `workspace` (the
-- user's own container of projects); `home` is unchanged. A project's identity is `common_dir`,
-- the git common directory of its repository (NULL for a plain folder); a project belongs to at
-- most one workspace (`parent_id`), and deleting the workspace leaves it ungrouped.
--
-- The `kind` check has to grow a value, which SQLite only does by rebuilding the table. The
-- migration runner switches foreign keys off around migrations, as the SQLite documentation
-- prescribes for this, so dropping the old table does not cascade into sessions and tasks.
--
-- Turning the workspaces of earlier versions into workspaces with member projects needs the
-- filesystem (git directories), so the daemon does that at its first start on this schema and
-- notes it in `app_meta` (`projects_auto`).

CREATE TABLE workspaces_v8 (
    id             TEXT PRIMARY KEY NOT NULL,
    name           TEXT NOT NULL,
    roots_json     TEXT NOT NULL DEFAULT '[]',
    created_at_ms  INTEGER NOT NULL,
    position       INTEGER NOT NULL,
    kind           TEXT NOT NULL DEFAULT 'project'
                   CHECK (kind IN ('home', 'project', 'workspace')),
    pinned         INTEGER NOT NULL DEFAULT 0,
    created_by     TEXT NOT NULL DEFAULT 'user'
                   CHECK (created_by IN ('user', 'cli', 'agent')),
    last_active_ms INTEGER NOT NULL DEFAULT 0,
    common_dir     TEXT,
    parent_id      TEXT REFERENCES workspaces (id) ON DELETE SET NULL
) STRICT;

INSERT INTO workspaces_v8
    (id, name, roots_json, created_at_ms, position, kind, pinned, created_by, last_active_ms)
SELECT id, name, roots_json, created_at_ms, position, kind, pinned, created_by, last_active_ms
  FROM workspaces;

DROP TABLE workspaces;
ALTER TABLE workspaces_v8 RENAME TO workspaces;

-- At most one Home.
CREATE UNIQUE INDEX workspaces_one_home ON workspaces (kind) WHERE kind = 'home';
CREATE INDEX workspaces_by_parent ON workspaces (parent_id);
