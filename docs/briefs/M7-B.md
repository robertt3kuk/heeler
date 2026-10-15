# M7-B: tasks with dependencies, run in a placement (roadmap M7, part 2: daemon + CLI)

Agent: implementer   Depends on: master `e403bfc` (protocol 0.15; this slice takes **0.16**)   Branch: slice/M7-B

## Context
Read AGENTS.md, then `docs/vision.md` § Orchestration, `docs/architecture.md` § Domain model (TASK in WORKSPACE, runs as SESSIONs, depends on TASKs; tasks are daemon state in SQLite), `docs/orchestration.md` (M7-A worktrees + placement), `docs/protocol.md` catalog rows `task.*`, `docs/integrations.md` (agent state from hooks), `docs/agents.md` (Codex chat sessions, approvals). Reuse: `crates/daemon/src/worktrees.rs` + `placement.rs` (create a worktree for a task), `registry.rs` (session create, `SessionInfo.agent`, agent tree), `crates/daemon/src/notify.rs` (intents), `crates/storage` (migrations), `crates/cli/src/commands/workspaces.rs` / `worktrees` commands (target resolution, tables).

## Scope (contract changes authorized)
`crates/core` (task model + state machine, no I/O), `crates/daemon` (new `tasks.rs`), `crates/storage` (migration), `crates/protocol` (0.16, capability `task`), `crates/client`, `crates/cli`, docs (`docs/orchestration.md` § Tasks, `docs/protocol.md`). **No desktop UI** (M7-C: tasks panel, diff review, merge).

## Contract
- **Task** `{id, workspace_id, title, prompt?, state, depends_on: [TaskId], placement: {kind: main | worktree, branch?, base?}, run: {kind: terminal {argv} | chat {agent: "codex"} }?, sessions: [SessionId], created_at_ms, started_at_ms?, ended_at_ms?, outcome?}`. States: `todo → ready → running → (blocked) → done | failed | cancelled`; `ready` = every dependency `done` (computed, not stored as truth); `blocked` mirrors the running session's agent being blocked/awaiting approval. A cycle in `depends_on` is refused (`bad_request`, names the cycle). Deleting a task that others depend on is refused unless `cascade`.
- `task.create {workspace_id, title, prompt?, depends_on?, placement?, run?}` → `{task}`; `task.list {workspace_id?, state?}` → `{tasks}` (topological order, ties by creation); `task.update {id, title?, prompt?, depends_on?, placement?, run?, state?: cancelled | todo}` → `{task}`; `task.delete {id, cascade?}`; `task.start {id}` → `{task, session}`: refuses unless `ready`; creates the worktree when the placement asks for one (branch default `task/<slug-of-title>`, via M7-A), then starts the session in it: `terminal` runs `argv` (env `HUTAN_TASK_ID`), `chat` creates a Codex chat session and sends `prompt` as the first message. Placement `main` warns (in the result, `warnings: [..]`) when another live agent already works in that checkout (reuse `shared_with`).
- **Finish detection**: a task's session exiting 0 / agent root finishing (`agent_finished` rules from `notify.rs`, incl. plain idle after a working stretch) / chat turn completing → `done`; non-zero exit, failed outcome → `failed`; `task.update {state: …}` can override (`done` via CLI `task done`). Dependants become `ready` and, when `auto_start` (per task, default false) is set, start automatically, bounded by `[tasks] max_parallel` in `daemon.toml` (default 3; excess stay `ready`, started FIFO as slots free).
- Events: `task.updated {task}` on every change (state, sessions, fields), `task.removed {id}`. Notification intent `task_done` / `task_failed` (new kinds in the M6 intent system, same gate and dedup).
- Persistence: migration (`tasks`, `task_deps`, `task_sessions`); after a daemon restart running tasks whose sessions were restored keep `running`, others become `failed` with outcome `interrupted`.
- **CLI**: `hutan task add "<title>" [--ws] [--after <task>]… [--worktree [<branch>]] [--prompt "<text>"] [--codex | -- <argv>] [--auto]`, `hutan task ls [--ws] [--json]` (tree/indent by dependency, state glyphs), `hutan task start|done|cancel|rm <task>`, `hutan task show <task>`; targets by id, unique prefix ≥ 4, or exact title.

## Acceptance
- Core unit tests: state machine transitions, readiness, cycle detection, topological order, auto-start scheduling with `max_parallel`.
- Daemon tests (temp git repos, fake Codex app-server fixtures for `chat`, `/bin/sh -c` for `terminal`): create/list/update/delete incl. refused cycle and cascade; start in main and in a new worktree; finish detection for exit 0 / non-zero / agent idle after working / chat turn completed; dependants become ready and auto-start within the limit; restart behavior; `task_done` intents.
- CLI end-to-end for add/ls/start/done/cancel/rm/show.
- Golden fixtures; docs updated; `just check` green; committed on `slice/M7-B`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; git only in temp repos the tests create; no real Codex (fixtures only). No push, no hard reset; `git grep -n '^<<<<<<<'` empty after any rebase; `pgrep -x hutand` clean before finishing. Known load-sensitive tests (also failing on master under load): `ack_pacing_keeps_floods_bounded_and_the_daemon_responsive`, `a_short_reconnect_during_an_approval_keeps_the_card_and_answers_afterwards` — report, don't touch. Do not edit docs/status.md or docs/roadmap.md.
