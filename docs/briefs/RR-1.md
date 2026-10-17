# RR-1: say exactly what survived a restart

Agent: implementer   Depends on: master `1094079` (protocol 0.23)   Branch: slice/RR-1

## Context
Read AGENTS.md, ADR 0009 (process survival), `docs/spikes/2026-10-17-ux-audit.md` § J8 and gap #9, `docs/protocol.md` (`SessionInfo.restored`, `session.restore`), `docs/integrations.md` (resume refs). Code: `crates/daemon/src/restore.rs`, `registry.rs` (restore on start), `chat.rs` (harness/Codex resume via `thread_id`, journal), `tasks.rs` (tasks whose session did not survive), desktop `sessions/attention.rs`, sidebar rows, chat view banners, the reconnect path in `client`.

Today a restarted session just says `restored`; the user cannot tell "your dev server is still running" (it is not — the daemon died) from "shell respawned in the same folder" from "agent resumed its conversation" from "the turn was interrupted".

## Scope (contract change authorized: protocol **0.26**, capability `session.recovery`)
`crates/core`, `crates/protocol`, `crates/daemon`, `crates/storage` if needed, `crates/client`, `crates/cli`, `crates/desktop`, docs + goldens.

## Contract
- `SessionInfo.recovery: Option<Recovery>` set when a daemon start restored the session, cleared when the user dismisses it (`session.recovery_ack`) or after the session's next user action:
  - `kind`: `shell_respawned { cwd }` (terminal; previous command line not re-run — show what was running if known from blocks: last running block's command), `agent_resumed { resume_argv }` (terminal agent relaunched with its reported resume command), `chat_resumed { thread_id, last_turn: completed | interrupted }`, `not_restarted { reason }` (`restore_sessions = "stopped"`, missing binary, resume failed + error), `ended_before` (it had ended before the restart; listed for history only).
  - `previous_epoch`, `daemon_stopped_at` (from the epoch history: clean stop vs crash/kill when known), `lost`: approvals that expired, an interrupted turn, a running task moved to failed.
- Daemon start computes them (no guessing: only facts it has).
- Desktop: after a reconnect to a new epoch with recovered sessions, a **Recovery summary** sheet/toast: "hutand restarted at 02:14 (it was killed). 5 sessions: 2 shells respawned, 1 agent resumed (claude --resume …), 1 chat turn interrupted — Retry, 1 not restarted (omp not found) — Restart". Each line: Open, Retry/Restart where applicable, Dismiss all. Per-row: the sidebar row and the chat/terminal header show a small "respawned"/"resumed"/"interrupted" chip until acknowledged. "Interrupted" chat turns offer "Retry last message".
- CLI: `hutan ls` shows a RECOVERY column; `hutan recovery` prints the summary; `hutan recovery ack [--all|<session>]`.
- Never say "still running" for anything that did not run through the restart.

## Acceptance
- Daemon tests: each recovery kind from real restarts in tests (terminal respawn, agent resume with a fake agent reporting `resume_argv`, chat with interrupted turn via the fake app-server/ACP fixture, missing binary → not_restarted, `restore_sessions = stopped`); ack clears; crash vs clean stop distinguished when known.
- Desktop GPUI tests: summary appears once per new epoch, actions route, chips clear on ack.
- `just check` green; committed on `slice/RR-1`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run `hutand`/`hutan-desktop` without it. No paid turns. No push, no hard reset. Do not edit docs/status.md or docs/roadmap.md. Concurrent protocol slices DL-1 (0.24), Q-1 (0.25): the integrator renumbers at merge. The user runs `/Applications/Hutan.app` on the real home: never touch it.
