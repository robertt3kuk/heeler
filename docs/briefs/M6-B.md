# M6-B: native notifications, launch-at-login, menu bar (roadmap M6.1, M6.3 rest, M6.4)

Agent: implementer (B2) after the parent's interactive spike (B1)   Depends on: M6-A (`6a9fb8e`, protocol 0.13)   Branch: slice/M6-B

## Context
Read AGENTS.md, then ADR 0007 (proposed), ADR 0008 (Keychain bound to signing identity), `docs/packaging.md`, `docs/protocol.md` § Notification intents, `docs/design.md` § Daemon lifecycle. Reuse: `crates/daemon/src/notify.rs` (intents, `notify.act`), `crates/client` (`notify_list`, `notify_act`, events), `xtask/src/bundle.rs` (layout, signing), `crates/desktop/src/app.rs` (daemon autostart, quit policy), `crates/desktop/src/workbench/notify.rs` (`ui.open`).

**This slice touches the user's system**: a notification permission prompt, a login item. Every step that triggers one runs only with the user present; tests never call `UNUserNotificationCenter` or `SMAppService`.

## B1 — spike (parent, with the user; decides ADR 0007)
Throwaway binary inside an ad-hoc signed test bundle in a temp folder (bundle id `dev.staffy.spike`, never `dev.staffy.Staffy`):
1. Can a non-main executable in `Contents/MacOS` (like `staffyd`) request authorization and post with `UNUserNotificationCenter` under the bundle's id?
2. With that process alive, are action clicks (category with Approve/Deny + Open) delivered to its delegate? With it not running, what does a click launch?
3. Same for an `LSUIElement` helper app in `Contents/Helpers`.
Record facts in `docs/spikes/<date>-notifications.md`, accept or amend ADR 0007, and choose: **daemon posts** (no helper) or **Notifier helper** (protocol client). Remove the spike bundle and its notification permission entry afterwards (System Settings → Notifications), with the user.

## B2 — Scope (after B1)
`crates/notifier` (new; only if B1 picks the helper) or a `notify_native` module where B1 says posting happens; `crates/desktop` (settings + menu bar + Dock badge); `xtask` bundle (helper, LaunchAgent unchanged); `crates/daemon` only for wiring a native poster if the daemon posts; `docs/packaging.md`, ADR 0007, `docs/design.md`. Protocol changes only if B1 proves one necessary (then 0.14, golden fixtures).

## Contract
- Every `notify.intent` becomes one native notification: title/body from the intent (already redacted and bounded), thread per session, category per kind. Approval actions = the intent's offered options (at most the first 3 plus Open); a click routes through `notify.act` (approval → `approval.respond`, Open → `ui.open`, which launches/focuses the desktop). Acknowledged/acted/expired intents remove their delivered notification. Dedup by intent id across reconnects (`notify.list` replay).
- Permission: requested once, on first need, never at startup without an intent; denied → intents stay in `staffy notify ls` and the desktop shows a one-line hint in Settings.
- `daemon.keep_running` setting (desktop Settings → General): on → `SMAppService.agent(plistName: "dev.staffy.daemon.plist")` register; off → unregister; status shown (enabled / requires approval in System Settings / not found). Only from the bundled app; a non-bundled `cargo run` shows "available in Staffy.app".
- Menu bar status item (setting `app.menu_bar = true`): daemon state, count of sessions needing attention, Open Staffy, Quit. Dock badge = attention count (blocked + pending approvals).
- Global shortcut ⌥Space opens the Assistant when the app runs (registered with the system only while the setting `assistant.global_shortcut` is on).

## Acceptance
- Unit/GPUI tests: intent → notification request mapping (fake poster trait), action → `notify.act` routing, dedup on replay, removal on ack/resolve, badge counts, setting toggles call a fake `LoginItem` trait. No real system calls in tests.
- `just check` green; `just bundle` verifies.
- Manual demo **with the user**: close the window, a TUI agent (fake `staffy agent hook claude` Stop after ≥ `min_turn_secs`) → native "finished" notification; a fake Codex approval → notification Approve unblocks the turn (`staffy approvals` empty); Keep running on → quit app → daemon alive, sessions alive; log out/in optional.

## Out of scope
Notarization and a Developer ID (open question 2), an icon (until designed), Keychain identity migration (document the prompts), remote clients.

## Report
Summary, B1 facts and the decision, files changed, commands run with results, what the user clicked, open questions. Commit on `slice/M6-B`. Do not edit docs/status.md or docs/roadmap.md.
