# M3-A: `storage` crate (SQLite actor) + daemon restore pipeline (roadmap M3.1 + M3.2)

Agent: implementer   Depends on: master `2c75da5`+ (runs concurrently with M2-H, which owns `crates/terminal` and `xtask` — do not edit those)   Branch: slice/M3-A

## Context
Read `AGENTS.md`, ADR 0006 (storage: rusqlite bundled, single writer actor thread, WAL, forward-only embedded migrations with pre-migration backup, ULID ids), ADR 0009 step 1 (children die with the daemon; restore structure and respawn), `docs/architecture.md` (§ State ownership, § Persistence and restore, § Paths), `docs/protocol.md`, `crates/daemon/src/{registry,terminals,daemon}.rs`, `crates/core`.

## Scope
New crate `crates/storage` (package `staffy-storage`, no GPUI, no Tokio in the public API except channel types), `crates/daemon` wiring, `docs/protocol.md` semantics notes. No protocol type changes unless strictly needed (report first).

## Change
1. `staffy-storage`: `Storage::open(path) -> Result<Storage>`; dedicated OS thread owns the write connection; bounded command channel; typed repository API (`workspaces`, `sessions`, `app_meta`). Pragmas per ADR 0006. Migrations `0001_init.sql` … embedded; before applying pending migrations copy the DB to `backups/staffy-<timestamp>-v<from>.db` (keep the newest 5); a failed migration leaves the original file intact and the daemon refuses to start with a clear error.
2. Schema v1: `workspaces(id, name, roots_json, created_at_ms, position)`, `sessions(id, workspace_id, kind, title, title_locked, spec_json, cwd, state_json, created_at_ms, ended_at_ms, resume_ref_json)`, `app_meta(key, value)` (epoch history, schema version). Indexes for listing by workspace.
3. Daemon writes through on every change (create/rename/close workspace; create/rename/stop/restart/exit session; cwd updates debounced to ≤ 1 write/s per session). Writes never block request handling (fire into the actor; ordering per entity preserved).
4. **Restore on start** (ADR 0009 step 1): load workspaces and sessions; sessions that were live when the daemon stopped are respawned as **new terminals in their last known cwd with the same `SessionId`** (state `starting` → `running`), keeping title and workspace. Sessions with a `resume_ref` (M3.3, may be empty now) use it instead of the spec command. Sessions that had ended stay listed as ended (final screen is gone: mark `restored: true` so clients can show "restored after daemon restart"). Setting `daemon.restore_sessions = "respawn" | "stopped"` (daemon config file `$STAFFY_HOME/daemon.toml`, default `respawn`).
5. `staffy daemon stop --stop-sessions` = sessions removed from the restore set only if the user asked (`--forget`); default keeps them for the next start. Document.
6. DB path `$STAFFY_HOME/staffy.db` (else `~/Library/Application Support/Staffy/staffy.db`).

## Acceptance
- Storage unit tests: migrations from empty, idempotent reopen, backup created before a (test) migration, failed migration keeps original, concurrent request ordering, crash-safety (kill the writer thread mid-batch → reopen consistent).
- Daemon integration tests: create workspaces + sessions (zsh with shell integration, `cat`, an exited one) → stop daemon → start a new daemon on the same `STAFFY_HOME` → `session.snapshot` shows the same ids, titles, workspaces; live ones running again in their cwd (verify with `pwd` output via history); ended one listed as ended+restored; new epoch.
- CLI: `staffy ls` after a daemon restart shows the restored sessions.
- `just check` green; committed on `slice/M3-A`.

## Rules
Inline `STAFFY_HOME=<temp dir>` on every command; never touch `/tmp/hh-*` or the real Staffy home. Do not push.
