# 0006. Storage: SQLite with a single writer actor

- Status: accepted
- Date: 2026-10-13
- Milestone: M3

## Context
Durable state: workspaces, sessions (kind, cwd, command, env allowlist, agent resume ref), layouts the daemon must restore, conversations and messages, tasks, approvals and decisions, provider configs (not secrets), audit of permission decisions. High-frequency state (terminal grid, deltas) stays in memory. Daemon is the only writer; clients never open the DB.

## Options
| Option | Pros | Cons |
|---|---|---|
| sqlx (async) | Async API, compile-time checked queries | Heavier, needs DB at build or offline cache; pool contention for SQLite |
| **rusqlite (bundled) on a dedicated thread** | Boring, fast, full SQLite control | Sync API → wrap in actor |
| JSON files (Herdr's `session.json`) | Simple | No queries, conversation history grows, partial-write risk |

## Decision
- `rusqlite` with `bundled` feature; one `storage` actor thread owns the write connection; Tokio side talks via a bounded channel. Read-only connections for heavy queries if needed.
- `PRAGMA journal_mode=WAL; synchronous=NORMAL; foreign_keys=ON`.
- File: `~/Library/Application Support/Staffy/staffy.db`, dir `0700`.
- Migrations embedded in the binary, forward-only, numbered; on failure the daemon refuses to start and keeps the old file (copy to `backups/` before migrating).
- IDs: ULID strings (sortable, client-visible).
- Terminal scrollback is not stored by default (may contain secrets). Optional per-session journaling is a later, opt-in feature.

## Consequences
- Storage API is message-based (`StorageCmd`) — easy to test with a temp DB.
- Event log ring is in memory; durable history comes from domain tables, not from replaying events.
