# 0009. Process survival across daemon restarts

- Status: accepted
- Date: 2026-10-13
- Milestone: M1 (policy), M9 (handoff)

## Context
UI restarts must never kill processes (ADR 0001). Daemon restarts (crash, upgrade) are rarer. Options to keep PTYs alive across a daemon restart: a per-session holder process (zmx/shpool style), or live fd handoff to the replacement daemon (Herdr `--handoff`, `SCM_RIGHTS`). Both add complexity that the first slice does not need.

## Decision
Staged:
1. **M1–M3**: PTY children die with the daemon (daemon is their session leader parent; on clean shutdown it sends SIGHUP and reaps). After restart the daemon restores structure from SQLite: respawns terminals in their saved cwd; agent sessions resume through native resume references (`codex resume <id>`, `claude --resume <id>`, `omp --resume=<id>`, app-server `thread/resume`, ACP `session/load`) when recorded; otherwise they come back as stopped sessions with a "restart" action.
2. **M9**: live upgrade handoff — the old daemon passes PTY master fds and session state to the new daemon over a Unix socket with `SCM_RIGHTS`, then exits. Crash survival via per-session holder processes is reconsidered only if crash frequency justifies it.

Design constraints now so step 2 stays possible:
- PTY master fds are owned by a `TerminalHandle` that can be detached from its reader task and serialized with its state.
- Session IDs and terminal IDs are stable across daemon epochs.

## Consequences
- Daemon stability matters; panics in session tasks are caught per task and never take down the process.
- Clients must handle `epoch` change as "everything may have restarted".
