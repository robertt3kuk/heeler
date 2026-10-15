# M6-C: keep running (launch agent), menu bar status, Dock badge, global shortcut (roadmap M6.4)

Agent: implementer   Depends on: master `da31af4` (M6-A bundle, M6-B native notifications)   Branch: slice/M6-C

## Context
Read AGENTS.md, then ADR 0007 (accepted), `docs/packaging.md` (bundle layout, inert `Contents/Library/LaunchAgents/dev.hutan.daemon.plist`, `hutand` signed as `dev.hutan.Hutan`), `docs/design.md` § Daemon lifecycle (`app.on_quit`, Daemon menu) and § Settings, `docs/spikes/2026-10-15-notifications.md` (a click while the daemon is down is lost: keep running fixes that). Code: `crates/desktop/src/app.rs` (menus, quit policy, daemon controls), `crates/desktop/src/workbench/daemon.rs`, `crates/desktop/src/sessions/attention.rs` + `store.rs` (attention counts, approvals), `crates/desktop/src/workbench/actions.rs` (`OpenAssistant` ⌥Space), `crates/settings` (namespaces), `crates/client` (daemon status).

**Touches the user's system**: registering a login item shows a system notice and appears in System Settings → General → Login Items. Tests never call `SMAppService`, `NSStatusBar`, the Dock or hotkey APIs; all of it sits behind traits with fakes. The integrator runs the real demo with the user.

## Scope
`crates/desktop` (new `macos/` module: `login_item.rs`, `status_item.rs`, `dock.rs`, `hotkey.rs`, each behind a small trait), settings schema, workspace deps (objc2-app-kit 0.3.2 for NSStatusBar/NSStatusItem/NSMenu/NSDockTile/NSApplication with only the needed features; `objc2-service-management` from crates.io for `SMAppService`, or a minimal `msg_send!` binding against the ServiceManagement framework if that crate is unavailable; Carbon `RegisterEventHotKey` via a minimal FFI or the `global-hotkey` crate (MIT/Apache) for the shortcut: no Accessibility permission may be required), `docs/design.md`, `docs/packaging.md`. No daemon or protocol changes unless the launch agent plist needs a fix (then `assets/macos/dev.hutan.daemon.plist` + the xtask test).

## Contract
- **Keep running** (`app.keep_running`, default false; Settings → General and the Daemon menu): on → `SMAppService.agent(plistName: "dev.hutan.daemon.plist").register()`; off → `unregister()`. Status shown next to the toggle: `enabled`, `requires approval` (button "Open Login Items" → `SMAppService.openSystemSettingsLoginItems()`), `not registered`, `not found`. Available only when the desktop runs from the bundle (`NSBundle.mainBundle` is `dev.hutan.Hutan`); otherwise the toggle is disabled with "available in Hutan.app". When enabled, quitting the app never stops the daemon (the quit dialog is skipped) and the desktop connects to the launchd-started daemon instead of spawning one; check the plist so launchd starts `Contents/MacOS/hutand` with `HUTAN_*` unset and logs in the standard location. Toggling off while sessions run keeps the current daemon running until the next quit (then the usual `app.on_quit` policy applies).
- **Menu bar status item** (`app.menu_bar = true`): the orangutan glyph as a template image (monochrome, from `assets/brand/05-mascot/svg/favicon.svg` rendered at 18/36 px into a committed PNG pair, or drawn with NSImage symbol fallback), plus a count when sessions need attention. Menu: daemon state (connected / stopped / reconnecting), "N need you" items (blocked agents, pending approvals; click focuses the session), Open Hutan, New Terminal, Keep Running (checkbox), Quit Hutan. Removed when the setting turns off; hot-reloads.
- **Dock badge** (`app.dock_badge = true`): `NSApp.dockTile.badgeLabel` = number of sessions needing attention (blocked + pending approvals + unread failures), empty at zero.
- **Global shortcut** (`assistant.global_shortcut = "alt-space"`, empty disables): system-wide while the app runs; brings the app forward and opens the Assistant (existing `OpenAssistant`). Conflict (registration fails) → one notice in the app, setting left as is.
- Settings namespaces get schema entries and appear in the settings sheet.

## Acceptance
- Unit/GPUI tests with fakes: keep-running toggle calls register/unregister and shows each status; quit skips the dialog when enabled; status-item model (menu entries and count) follows the attention model; Dock badge count transitions; hotkey registration success/conflict paths; settings hot reload adds/removes the status item.
- `just check` green; `just bundle` builds and verifies.
- Never call the real APIs in tests or by running the app/bundle; never register a login item. The integrator does the demo with the user.

## Out of scope
Notarization, a Developer ID, auto-update, remote clients.

## Report
Summary, files changed, commands run with results, what the integrator must click in the demo, open questions. Commit on `slice/M6-C`. Do not edit docs/status.md or docs/roadmap.md.
