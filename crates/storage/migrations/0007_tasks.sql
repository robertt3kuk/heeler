-- Schema v7: tasks with dependencies (roadmap M7-B, protocol 0.16).
--
-- `state` is the stored state only: `ready` (every dependency done) and `blocked` (the running
-- task's agent waits for the user) are derived when a task is shown. Placement and run are
-- small JSON documents of the core model. A task goes with its workspace; a task that others
-- depend on is only deleted together with them (the daemon refuses otherwise), and a deleted
-- task takes its edges with it.
CREATE TABLE tasks (
    id            TEXT PRIMARY KEY NOT NULL,
    workspace_id  TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    prompt        TEXT,
    state         TEXT NOT NULL
                  CHECK (state IN ('todo', 'running', 'done', 'failed', 'cancelled')),
    placement_json TEXT NOT NULL,
    run_json      TEXT,
    auto_start    INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    ended_at_ms   INTEGER,
    outcome       TEXT
) STRICT;

-- Listing a workspace's tasks in creation order.
CREATE INDEX tasks_by_workspace ON tasks (workspace_id, created_at_ms);

CREATE TABLE task_deps (
    task_id    TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    depends_on TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    position   INTEGER NOT NULL,
    PRIMARY KEY (task_id, depends_on)
) STRICT;

CREATE INDEX task_deps_by_dependency ON task_deps (depends_on);

-- The sessions that ran a task, oldest first. No foreign key to `sessions`: a session that is
-- gone stays named in the task's history.
CREATE TABLE task_sessions (
    task_id    TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    position   INTEGER NOT NULL,
    PRIMARY KEY (task_id, session_id)
) STRICT;
