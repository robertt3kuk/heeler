-- Schema v4: the agent observation tree of a session (roadmap M13, protocol 0.10): what the
-- daemon knew about the agents (and subagents) of a session when it last wrote, so that a
-- restarted daemon can say which nodes lost contact instead of forgetting them. Transient
-- details (what an agent is doing right now) are not stored.

CREATE TABLE agent_trees (
    session_id    TEXT PRIMARY KEY NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    tree_json     TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
