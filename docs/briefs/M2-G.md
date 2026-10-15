# M2-G: daemon lifecycle with the app, daemon controls (menu, status-bar popover, ⌘K), clean quit

Agent: implementer   Depends on: master   Branch: slice/M2-G

## Context
Read `AGENTS.md`, `docs/design.md` § Daemon lifecycle (binding: flowchart, `app.on_quit`, daemon controls in three places, clean quit), § Settings, `docs/architecture.md` § Process topology, `crates/desktop` (`app.rs`, `sessions/`, status items, menus, palette actions, settings namespaces + settings sheet), `crates/client` (`Connection`, `ConnectionState`, autostart), `crates/cli` (`daemon start|status|stop`), `crates/daemon` (`daemon.shutdown { stop_sessions }`, `daemon.status`).

A separate slice (M2-F, performance) is fixing a release-build crash in the desktop concurrently. You own quit/lifecycle code; do not refactor rendering/terminal_view code.

## Scope
`crates/desktop`, `crates/cli` (add `daemon restart`), `crates/client` only if a small additive API is required (e.g. `Connection::set_autostart(bool)` so a user-stopped daemon is not immediately respawned). No protocol changes.

## Changes
1. **Settings**: register namespace `app` with `on_quit: ask | keep_running | stop_daemon` (default `ask`), shown in the settings sheet.
2. **Quit flow** (⌘Q, app menu Quit, closing the last window): query live sessions from the store; none → `daemon.shutdown { stop_sessions: false }` then quit; some → per `app.on_quit`: `ask` = modal dialog (Keep sessions running · Stop sessions & daemon · Cancel, checkbox "Don't ask again" writing `app.on_quit`), `keep_running` = quit leaving the daemon, `stop_daemon` = `daemon.shutdown { stop_sessions: true }` then quit. While disconnected, quit immediately. Always: flush layout state, close the connection (`Connection::shutdown`), exit code 0.
3. **SIGTERM / SIGINT / SIGHUP** to the desktop: same as `keep_running` (save, disconnect, exit 0) — never ask, never stop the daemon.
4. **Daemon controls** in three places, one shared implementation:
   - `Daemon` menu in the menu bar,
   - popover anchored to the `connection` status item (click),
   - ⌘K actions `daemon::Status`, `daemon::Restart`, `daemon::Stop`, `daemon::Start`, `daemon::OpenLogs`.
   Content per design.md: status fields (`daemon.status` + socket/log paths), actions with confirmation when sessions are live, and the **working mode** segmented control bound to `app.on_quit` (writes settings, reflects hot reload).
   - Stop Daemon: `daemon.shutdown { stop_sessions: true }`; the app stays open; disable autostart so the connection stays `Stopped`/disconnected with a "Daemon stopped — Start" banner in the status popover and on terminal items.
   - Start Daemon: re-enable autostart / spawn `staffyd` and reconnect.
   - Restart Daemon: stop as above then start; terminals show ended/re-attach states correctly (epoch change).
   - Open Logs: reveal the daemon log file in Finder (`open -R`), no other side effects.
5. **CLI**: `staffy daemon restart [--stop-sessions]` = stop (refuse with exit 1 and a hint if sessions are live and the flag is absent) then start.
6. **Panic hook** (if M2-F has not landed one when you rebase): log panic message + backtrace to the desktop log before the process exits.

## Acceptance
- GPUI tests with an in-process or spawned daemon (temp `STAFFY_HOME`): quit with no live sessions stops the daemon; quit with live sessions under each `on_quit` value (dialog choice simulated) does the right thing; "Don't ask again" writes the setting; SIGTERM path keeps the daemon; Stop/Start/Restart from the popover and palette; mode control writes `app.on_quit` and reflects external edits.
- CLI test for `daemon restart` (with and without live sessions).
- Manual: launch `staffy-desktop` (temp `STAFFY_HOME`), quit via ⌘Q with and without sessions — exit code 0, no crash report in `~/Library/Logs/DiagnosticReports/` for `staffy-desktop` newer than the run start. No screenshots / permission prompts.
- `just check` green; committed on `slice/M2-G`.

## Report
Summary, files changed, commands run with results, deviations, open questions.
