# UX-A: fixes from the user test — terminals after a restart, Quit/Assistant via `hutan ui`, settings language, times

Agent: implementer   Depends on: master `15dc5e1` (protocol 0.27)   Branch: slice/UX-A

## Context
Read AGENTS.md, `docs/spikes/2026-10-17-user-test.md` (findings **F1, F6, F7, F8, F10**, with steps and evidence; screenshots under `/Users/workie/proj/hutan-usertest/scratch/`), `docs/design.md`, `docs/protocol.md` § Recovery (RR-1), § Control (`ui.*`). Code: `crates/desktop/src/sessions/terminal.rs`/`store.rs` (TerminalSession attach, reconnect, epoch), `crates/terminal_view`, `crates/client` (`Connection`, reconnect), `workbench/remote.rs` (`ui.run` executor), `workbench/settings_schema.rs`/`settings_sheet.rs`, recovery chips/summary (RR-1).

## Scope
`crates/desktop`, `crates/terminal_view`, `crates/client` (only if the reconnect path needs it), `crates/cli` (F7 output), docs. No protocol change (report a gap otherwise).

## Contract
- **F1**: after the daemon restarts (new epoch) every open terminal tab whose session was respawned with the same session id reattaches to the session's **new terminal**: fresh snapshot, input goes to the live shell, the "session is gone" state clears; the RR-1 "respawned" chip shows. A tab whose session really ended shows the ended state as today. Works for tabs in every pane/dock and in parked workspace layouts once shown.
- **F6**: `hutan ui run workbench::OpenAssistant` (and every action listed by `hutan ui actions`) dispatches; add a test that runs **every** listed action name through the remote executor without "unknown action" (actions needing arguments get them from their schema or are skipped with a reason).
- **F7**: `hutan ui run workbench::Quit` (and any action that closes the window) reports success (`{"ran": …, "window_closed": true}`), not "window closed before it answered".
- **F8**: the settings sheet shows user-facing titles and help for every field (no Rust/type names, no internal ids); enum values have labels; docs in `docs/design.md` § Settings stay the source of the wording.
- **F10**: times shown to users use one convention (relative "3 min ago" with absolute local time on hover/tooltip; CLI prints local time with zone), in the recovery summary, history, tasks and sidebar; README/docs claims the report flags as outdated are corrected.

## Acceptance
- GPUI test: a terminal tab survives a daemon restart (in-process daemon restarted on the same home) and accepts input that reaches the respawned shell; ended session still shows ended.
- The every-action dispatch test; F7 CLI test; settings-label test (no field title contains `::` or snake_case ids).
- `just check` green; committed on `slice/UX-A`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run `hutand`/`hutan-desktop` without it. No paid turns. No push, no hard reset. Do not edit docs/status.md or docs/roadmap.md. Concurrent: UX-B (launcher/changes/sidebar/hooks), P-1 (performance) — rebase late. The user's `/Applications/Hutan.app`: never touch it.
