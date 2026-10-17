-- Schema v9: questions of agents (roadmap Q-1, protocol 0.25).
--
-- The audit trail of what an agent asked and how it ended, like `approvals`. The answers are
-- not here: a value may be a secret, and the journal only keeps what the question said and who
-- decided (`outcome` `answered` carries no values at all).
CREATE TABLE questions (
    id              TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    turn_id         TEXT,
    title           TEXT NOT NULL,
    requested_at_ms INTEGER NOT NULL,
    resolved_at_ms  INTEGER,
    outcome         TEXT CHECK (outcome IN ('answered', 'declined', 'cancelled', 'expired')),
    decided_by      TEXT
) STRICT;

CREATE INDEX questions_by_session ON questions (session_id, requested_at_ms);
-- Finding what is still waiting.
CREATE INDEX questions_unresolved ON questions (resolved_at_ms) WHERE resolved_at_ms IS NULL;
