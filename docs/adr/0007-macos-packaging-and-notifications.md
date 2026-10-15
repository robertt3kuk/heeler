# 0007. macOS packaging, launch, and notifications

- Status: accepted (2026-10-15, after the M6-B spike `docs/spikes/2026-10-15-notifications.md`)
- Date: 2026-10-13
- Milestone: M6

## Context
- `UNUserNotificationCenter` requires the posting process to have a bundle identifier and a code signature (ad-hoc is enough); NSUserNotification is deprecated and click actions are broken on macOS 26 (per terminal-notifier replacements).
- Background operation needs a per-user launch agent; `SMAppService.agent(plistName:)` registers a LaunchAgent plist shipped inside the app bundle (macOS 13+).
- Keychain items are bound to the code-signing identity of the accessing binary.
- Spike facts: a second executable in `Contents/MacOS` signed with the **app's** identifier gets the app's notification permission, posts, and receives both background and default action clicks while it runs; signed with its own identifier it is refused (private entitlement); a click while the poster is not running is dropped by `usernoted`; bundles under `/tmp` are rejected.

## Decision
Bundle layout (M6-A, `docs/packaging.md`):

```text
Hutan.app/
  Contents/MacOS/hutan-desktop               # GPUI desktop (main executable; not `Hutan`: case-insensitive clash with `hutan`)
  Contents/MacOS/hutand                      # daemon, signed with the app identifier dev.hutan.Hutan
  Contents/MacOS/hutan                       # CLI (symlinked to /usr/local/bin by user action)
  Contents/Library/LaunchAgents/dev.hutan.daemon.plist
  Contents/Resources/Hutan.icns
```

- Daemon is started by the desktop/CLI on demand (spawn + wait for socket), or by launchd when the user enables "keep running" (SMAppService registration done from the desktop).
- **Notifications are posted by `hutand` itself** under `dev.hutan.Hutan` from its `notify.intent` stream; there is no Notifier helper. Action clicks reach the daemon's delegate and go through the same path as `notify.act` (approval → `approval.respond`, Open → `ui.open`, which also brings up the app). A daemon that does not run from the signed bundle (e.g. `cargo run`) posts nothing; intents remain available through `notify.list` / `hutan notify watch`.
- A click while the daemon is not running is lost; approvals then stay pending (safe). "Keep running" is the remedy.
- Development builds: `xtask bundle` producing an ad-hoc-signed bundle in a non-`/tmp` location.

## Consequences
- `hutand` and the app share one code identity: one notification permission entry, one Keychain identity (ADR 0008).
- The daemon hosts a Cocoa run loop for the notification delegate (thread placement verified in M6-B).

## Evidence
- `docs/spikes/2026-10-15-notifications.md`
- mac-usernotifications bundling requirement: <https://docs.rs/mac-usernotifications/0.3.1/mac_usernotifications/>
