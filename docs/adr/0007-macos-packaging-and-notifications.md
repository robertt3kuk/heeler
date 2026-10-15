# 0007. macOS packaging, launch, and notifications

- Status: proposed (accept after M6 spike)
- Date: 2026-10-13
- Milestone: M6

## Context
- `UNUserNotificationCenter` requires the posting process to have a bundle identifier and a code signature (ad-hoc is enough); NSUserNotification is deprecated and click actions are broken on macOS 26 (per terminal-notifier replacements). Actions are delivered on the main thread run loop of the posting process.
- Background operation needs a per-user launch agent; `SMAppService.agent(plistName:)` registers a LaunchAgent plist shipped inside the app bundle (macOS 13+).
- Keychain items are bound to the code-signing identity of the accessing binary.

## Decision (proposed)
Bundle layout:

```text
Hutan.app/
  Contents/MacOS/Hutan                      # GPUI desktop
  Contents/MacOS/hutand                     # daemon (signed, same team)
  Contents/MacOS/hutan                      # CLI (symlinked to /usr/local/bin by user action)
  Contents/Library/LaunchAgents/dev.hutan.daemon.plist
  Contents/Helpers/Hutan Notifier.app       # LSUIElement helper, own bundle id
```

- Daemon is started by the desktop/CLI on demand (spawn + wait for socket), or by launchd when the user enables "keep agents running" (SMAppService registration done from the desktop).
- Notifications: daemon emits `notification.intent` events; **Hutan Notifier** (a protocol client with AppKit run loop) posts them and sends `approval.decide` / `ui.open` commands back. When the desktop is running, it may display in-app UI instead; dedup by intent ID.
- Development builds: `cargo-bundle` or an `xtask bundle` producing an ad-hoc-signed bundle.

## Open questions (M6 spike)
- Can `hutand` inside `Contents/MacOS` post notifications directly under the app's bundle id? `[UNVERIFIED]` If yes, the Notifier helper may be unnecessary.
- Keychain access prompts for `hutand` vs app identity across rebuilds during development.

## Evidence
- mac-usernotifications bundling requirement: <https://docs.rs/mac-usernotifications/0.3.1/mac_usernotifications/>
