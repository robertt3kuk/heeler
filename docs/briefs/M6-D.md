# M6-D: the daemon is always visible in the menu bar while it runs

Agent: implementer   Depends on: master `64d8e0a` (D-2: keep running / start at login split)   Branch: slice/M6-D

## Context
Read AGENTS.md, ADR 0007 (accepted: the daemon posts notifications from Hutan.app and runs the Cocoa run loop on its main thread), `docs/design.md` § macOS integration and § Daemon lifecycle, `docs/packaging.md`. Code: `crates/daemon/src/main.rs` (main-thread run loop host), `crates/daemon/src/notify_native/{mod,cocoa,fake}.rs`, `crates/desktop/src/macos/status_item.rs` (the desktop's menu bar item), `crates/desktop/src/settings.rs` (`app.menu_bar`, `keep_running`, `start_at_login`), `assets/macos/MenuBarIcon*.png`.

The user wants to always see that the daemon is on: today the menu bar item belongs to the desktop, so with "keep running" or "start at login" the daemon runs invisibly after the app quits.

## Scope
`crates/daemon` (a Cocoa status item next to the notification poster), `crates/desktop` (hand-off rule), `crates/protocol` only if a field is needed (prefer none: the daemon knows its desktop clients), docs.

## Contract
- **Exactly one Hutan icon in the menu bar while the daemon runs from Hutan.app**:
  - Desktop connected (a `ClientKind::Desktop` client is attached): the desktop's existing item shows (it is richer: needs-you list, toggles). The daemon shows none.
  - No desktop connected: `hutand` shows its own item (same template glyph, a small dot when something needs the user: blocked agents / pending approvals / failed sessions it knows of). Menu: "Hutan daemon — running · N sessions", "N need you" entries (open Hutan focused on that session via the existing `ui.open` path / launching Hutan.app with the session), "Open Hutan", separator, "Stop daemon…" (confirmation when live sessions: Stop Sessions / Cancel), "Start at login" checkmark reflecting `app.start_at_login` (read-only display; clicking opens Hutan's settings).
  - The swap is immediate on desktop connect/disconnect (no flicker gap > 1 s, no double icon).
  - `app.menu_bar = false` hides the desktop's item only while the app runs; the daemon's own item is **not** optional (the user asked to always see the daemon), document it.
- Outside Hutan.app (dev `target/debug/hutand`, tests): no status item, no Cocoa (same rule as notifications).
- The daemon's item runs on the main-thread run loop that already hosts notifications; bounded channel between tokio and Cocoa; shutdown removes the item.

## Acceptance
- Unit/integration tests with a fake status-item seam (like `notify_native::fake`): shown with no desktop client, hidden while a desktop is connected, back after disconnect, count/dot updates, menu actions route (open, stop with confirmation outcome, settings).
- Manual demo is the integrator's: build the bundle (`just bundle`), run with a temp `HUTAN_HOME`, quit the desktop with keep running → icon stays (daemon's), reopen → one icon.
- `just check` green; committed on `slice/M6-D`.

## Rules
Inline `HUTAN_HOME=<temp dir>`; never run `hutand`/`hutan-desktop` without it; never launch the built `.app` or call real AppKit status APIs in tests. No push, no hard reset. Do not edit docs/status.md or docs/roadmap.md. The user runs `/Applications/Hutan.app` on the real home: never touch it.
