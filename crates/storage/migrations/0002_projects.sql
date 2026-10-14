-- Schema v2: projects and workspaces (roadmap M12-A).
--
-- Workspaces gain a kind (exactly one `home`), pin, origin, and last activity; closed projects
-- are remembered in `recent_projects`. The v1 `default` workspace becomes Home.

ALTER TABLE workspaces ADD COLUMN kind TEXT NOT NULL DEFAULT 'project'
    CHECK (kind IN ('home', 'project'));
ALTER TABLE workspaces ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workspaces ADD COLUMN created_by TEXT NOT NULL DEFAULT 'user'
    CHECK (created_by IN ('user', 'cli', 'agent'));
ALTER TABLE workspaces ADD COLUMN last_active_ms INTEGER NOT NULL DEFAULT 0;

-- The workspace sessions landed in without a workspace becomes Home; its placeholder name
-- follows.
UPDATE workspaces
   SET kind = 'home',
       name = CASE WHEN name = 'default' THEN 'Home' ELSE name END
 WHERE id = (SELECT value FROM app_meta WHERE key = 'default_workspace_id');
DELETE FROM app_meta WHERE key = 'default_workspace_id';

-- Existing workspaces were last active when their newest session was created or ended.
UPDATE workspaces
   SET last_active_ms = MAX(
           created_at_ms,
           COALESCE((SELECT MAX(MAX(s.created_at_ms, COALESCE(s.ended_at_ms, 0)))
                       FROM sessions s
                      WHERE s.workspace_id = workspaces.id), 0));

-- At most one Home.
CREATE UNIQUE INDEX workspaces_one_home ON workspaces (kind) WHERE kind = 'home';

CREATE TABLE recent_projects (
    path           TEXT PRIMARY KEY NOT NULL,
    name           TEXT NOT NULL,
    last_opened_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX recent_projects_by_time ON recent_projects (last_opened_ms DESC, path);
