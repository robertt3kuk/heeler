-- Schema v3: agent chat sessions (roadmap M4). Hutan's journal is the source of a chat's
-- history (ADR 0005); the provider's own copy only lets the agent continue.

-- One row per user-message-to-completion cycle.
CREATE TABLE chat_turns (
    session_id       TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    turn_id          TEXT NOT NULL,
    provider_turn_id TEXT,
    status           TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed', 'cancelled')),
    started_at_ms    INTEGER NOT NULL,
    ended_at_ms      INTEGER,
    error            TEXT,
    PRIMARY KEY (session_id, turn_id)
) STRICT, WITHOUT ROWID;

-- The event journal: every event of a session except streaming deltas (the matching *_end
-- events carry the complete text). `seq` is per session and strictly increasing; it has gaps
-- where deltas were.
CREATE TABLE chat_items (
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    ts_ms      INTEGER NOT NULL,
    turn_id    TEXT,
    item_id    TEXT,
    kind       TEXT NOT NULL,
    event_json TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) STRICT, WITHOUT ROWID;

-- Permission requests and what became of them: the audit trail of approvals.
CREATE TABLE approvals (
    id              TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    turn_id         TEXT,
    kind            TEXT NOT NULL,
    title           TEXT NOT NULL,
    options_json    TEXT NOT NULL,
    requested_at_ms INTEGER NOT NULL,
    resolved_at_ms  INTEGER,
    outcome         TEXT CHECK (outcome IN ('selected', 'expired')),
    option_id       TEXT,
    decided_by      TEXT
) STRICT;

CREATE INDEX approvals_by_session ON approvals (session_id, requested_at_ms);
-- Finding what is still waiting.
CREATE INDEX approvals_unresolved ON approvals (resolved_at_ms) WHERE resolved_at_ms IS NULL;
