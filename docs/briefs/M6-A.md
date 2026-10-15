# M6-A: signed app bundle and daemon notification-intent API (roadmap M6.2; native notification delivery is M6-B)

Agent: implementer   Depends on: M16-A merged (protocol 0.12; this slice takes **0.13**), M13-B (`AgentAttentionChanged`)   Branch: slice/M6-A

## Context
Read `AGENTS.md`, ADR 0007 (proposed — packaging, launch agent, notifications), ADR 0008 (Keychain bound to code-signing identity), `docs/roadmap.md` § M6, `docs/integrations.md`, `docs/agents.md` (approvals), M13 observation tree, `xtask/`.

**This slice must not trigger any macOS permission prompt or register anything with the system**: no `UNUserNotificationCenter` calls, no `SMAppService` registration, no launching the built bundle, no Keychain access. Those are M6-B and run with the user present.

## Scope (contract changes authorized)
`xtask` (bundle command), `justfile` (`just bundle`), `assets/macos/` (Info.plist and LaunchAgent plist), `crates/daemon` (notification intents), `crates/protocol` (0.13, capability `notify.intents`), `crates/client`, `crates/cli`, minimal desktop focus reporting / `ui.open` handling, `docs/packaging.md` (new), `docs/protocol.md`.

## Contract
- **`cargo run -p xtask -- bundle [--release] [--out target/bundle]`** produces a runnable `Staffy.app`: `Contents/MacOS/{staffy-desktop, staffyd, staffy}` (the desktop cannot be named `Staffy`: it collides with the CLI `staffy` on case-insensitive volumes), `Contents/Info.plist` (bundle id `dev.staffy.Staffy`, version from the workspace, `LSMinimumSystemVersion` 13.0, `NSHighResolutionCapable`), `Contents/Library/LaunchAgents/dev.staffy.daemon.plist` (`BundleProgram` = `Contents/MacOS/staffyd`, `KeepAlive` false, `RunAtLoad` false — registered only in M6-B), and `Contents/Resources/` (shell-integration scripts). Ad-hoc sign binaries then the app with `codesign --force --sign - --timestamp=none`; `codesign --verify --deep --strict` must pass. `just bundle` wraps it. Do not ship a stub Notifier helper or placeholder icon: native notification delivery/helper belongs to M6-B and must be real before it enters the bundle. ADR 0007 stays proposed until its notification/signing questions are exercised with the user present.
- The desktop and CLI already find `staffyd` beside themselves — verify that holds inside the bundle (path resolution unit test on a fake bundle tree); `staffy` CLI inside the bundle autostarts the bundled `staffyd`.
- **Notification intents** (daemon): `NotificationIntent { id, kind: approval_required | agent_blocked | agent_finished | session_exited_with_error | chat_turn_finished, session_id, workspace_id?, title, body (≤ 240 chars, redacted), actions: [{id, label}] (approval options for approvals; "Open" otherwise), created_at_ms, expires_at_ms? }`, emitted on: pending approval (M4-A), a node becoming blocked (M13-A tree), a root agent going working → idle with outcome completed after ≥ `notify.min_turn_secs` (default 20), a session exiting non-zero, a chat turn finishing while no client has the session focused (clients report focus with a new `client.focus { session_id? }` request). Dedup by (kind, session, node/approval id); rate-limit per session (≤ 1 per 10 s except approvals). Settings `daemon.toml [notify] enabled = true, min_turn_secs = 20, kinds = [...]`.
- **Protocol 0.13**: event `notify.intent`, request `notify.list` (pending/unacknowledged), `notify.ack { id }`, `notify.act { id, action_id }` (routes approval actions to `approval.respond`; "Open" → `ui.open` event for clients), `client.focus`. Golden fixtures.
- Desktop focus reports follow the actual focused session and are cleared on disconnect/window deactivation; `ui.open` focuses the requested live session using the existing terminal/chat item machinery. Keep these changes minimal and reconcile with the concurrent assistant panel at rebase.
- **CLI**: `staffy notify ls|ack|act`, `staffy notify watch` (prints intents — handy until the Notifier posts real notifications).

## Acceptance
- xtask test builds a debug bundle into a temp dir and asserts the layout, plist keys (parse with `plist` crate), executables present, `codesign --verify --deep --strict` OK — and that nothing was launched or registered.
- Daemon tests for each intent trigger, dedup, rate limit, focus suppression, `notify.act` routing an approval decision (fake app-server fixtures from M4-A), ack.
- `just check` green (keep `just bundle` out of the default gate unless it is fast; add it to CI notes); committed on `slice/M6-A`.

## Rules
Inline `STAFFY_HOME=<temp dir>`; never launch the built `.app`, never call notification/launch-agent/Keychain APIs, never touch `/tmp/hh-*` or the real Staffy home. No push, no hard reset; `git grep -n '^<<<<<<<'` empty after any rebase; `pgrep -x staffyd` clean before finishing.
