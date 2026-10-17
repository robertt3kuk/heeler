# UX-B: fixes from the user test — safe Start, real shell commands, hooks opt-in, Changes toolbar, rename

Agent: implementer   Depends on: master `15dc5e1` (protocol 0.27)   Branch: slice/UX-B

## Context
Read AGENTS.md, `docs/spikes/2026-10-17-user-test.md` (findings **F2, F3, F4, F5, F9, F11**; evidence under `/Users/workie/proj/hutan-usertest/scratch/`), `docs/briefs/S-1.md` (launcher), `docs/briefs/D-2.md` (setup, integrations on launch), `docs/briefs/DL-1.md` + `RC-1.md` (Changes view), `docs/design.md`. Code: `workbench/launcher.rs`, `launcher_ui.rs`, `sessions/launch.rs`, `workbench/setup.rs`, `workbench/integrations.rs`, `settings.rs` (`integrations.auto_install`), `workbench/changes*.rs`, sidebar rename (`panels/sessions.rs`, `projects.rs`).

## Scope
`crates/desktop`, small `crates/daemon` change only if Command execution needs it (see F4), docs.

## Contract
- **F2 — no accidental model spend**:
  - The launcher opens on the **last used** What/Agent/Model/Where (persisted in desktop state), first ever: **Plain terminal**.
  - The Start button and the sheet footer show a one-line summary of what will happen ("Start OMP · claude-opus-5-5 chat in hutan (main)" / "Open a terminal in …").
  - Starting an **agent chat with a prompt** (it will run a model turn at once) needs an explicit confirmation the first time per agent+model ("This sends your prompt to OMP now — Start / Cancel", "Don't ask again for this model"), stored in settings `launcher.confirm_agent_start` (default true, per model memory in desktop state).
  - Keyboard: Tab/Shift-Tab move between rows; ↑/↓ change the value only inside an opened row (Enter/Space opens a row's choices); the focused row is visibly highlighted; ⌘↩ never fires while a row's choice list is open.
- **F3 — hooks opt-in**: `integrations.auto_install` default **false**; the setup's Agents step offers "Install hooks for OMP / Claude Code / Codex" as explicit checkboxes (unchecked) with what each writes and where (paths); auto-install only runs after the user turned it on. Existing users who never chose: treat as off (no install on next launch). With a custom `HUTAN_HOME` the step says the hooks go into the agents' real config dirs.
- **F4 — Command runs in a shell**: the launcher's Command kind (and `hutan task add --run` if it has the same issue) runs the text through the user's login shell (`$SHELL -lc "<text>"` with the B-1 login environment) so `;`, `&&`, pipes, quotes and globs work; the field hint says "Runs in your shell". Verify `printf 'X\n'; pwd` prints both.
- **F5**: the Changes view toolbar never clips actions: wrap to a second row or collapse secondary actions into a "…" menu at narrow widths; Merge stays visible; test at 640 px wide.
- **F9**: Rename (session, project, workspace) opens with the whole current name selected so typing or paste replaces it.
- **F11**: switching the launcher's Agent clears the previous agent's install/login error.

## Acceptance
- GPUI tests for each item (default Plain terminal on first open, last-used restore, summary text, confirmation shown once then remembered, keyboard rules, hooks default off + explicit opt-in installs only selected ones via the fake installer, Command via shell runs composite commands in a real PTY, Changes toolbar at 640 px shows Merge, rename selection, error cleared).
- `just check` green; committed on `slice/UX-B`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run `hutand`/`hutan-desktop` without it. No paid turns — use the fake app-server/ACP fixtures. No push, no hard reset. Do not edit docs/status.md or docs/roadmap.md. Concurrent: UX-A (terminal reattach, ui dispatch, settings labels), P-1 (performance) — rebase late. The user's `/Applications/Hutan.app`: never touch it.
