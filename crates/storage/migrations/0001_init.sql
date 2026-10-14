-- Schema v1: workspaces, sessions, and key/value app metadata (ADR 0006, roadmap M3.1).

CREATE TABLE app_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE workspaces (
    id            TEXT PRIMARY KEY NOT NULL,
    name          TEXT NOT NULL,
    roots_json    TEXT NOT NULL DEFAULT '[]',
    created_at_ms INTEGER NOT NULL,
    position      INTEGER NOT NULL
) STRICT;

CREATE TABLE sessions (
    id              TEXT PRIMARY KEY NOT NULL,
    workspace_id    TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    title           TEXT NOT NULL,
    title_locked    INTEGER NOT NULL,
    spec_json       TEXT NOT NULL,
    cwd             TEXT,
    state_json      TEXT NOT NULL,
    created_at_ms   INTEGER NOT NULL,
    ended_at_ms     INTEGER,
    resume_ref_json TEXT
) STRICT;

-- Listing a workspace's sessions in creation order.
CREATE INDEX sessions_by_workspace ON sessions (workspace_id, created_at_ms, id);
