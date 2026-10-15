# M6-B: native notifications posted by the daemon (roadmap M6.3)

Agent: implementer   Depends on: master (protocol 0.13 intents; spike `docs/spikes/2026-10-15-notifications.md`, ADR 0007 accepted)   Branch: slice/M6-B

## Context
Read AGENTS.md, then ADR 0007 (accepted: **the daemon posts**, no helper), the spike report (facts 1–6), ADR 0008, `docs/packaging.md`, `docs/protocol.md` § Notification intents. Reuse: `crates/daemon/src/notify.rs` (`NotifyService`: intents, `ack`, `act`, expiry), `xtask/src/bundle.rs` (signing), `crates/daemon/src/main.rs` (process start), `assets/brand/` (artwork if a notification attachment helps; optional).

The spike code (objc2 0.6.5, objc2-foundation 0.3.2, objc2-user-notifications 0.3.2 with `default-features = false` and only the needed features, block2 0.6.2) is in the spike report's terms: `define_class!` delegate with `willPresent` (Banner|List|Sound) and `didReceiveNotificationResponse`, a category per intent kind, `requestAuthorizationWithOptions`, `addNotificationRequest`. All crates are in the local cargo cache.

## Scope
`crates/daemon` (new `notify_native.rs` behind a `NativePoster` trait, macOS only), `xtask` (sign `hutand` with identifier `dev.hutan.Hutan`; adjust the bundle test), workspace deps (objc2 family, macOS target only), `docs/packaging.md`, `docs/protocol.md` § Notification intents. No desktop changes (menu bar, Dock badge, login item, shortcut are M6-C). No protocol change.

## Contract
- `NativePoster` trait: `authorize()`, `post(intent)`, `remove(intent_id)`, plus a channel of `NativeResponse {intent_id, action_id}`. Implementations: `UserNotifications` (real; only when `NSBundle.mainBundle` is `dev.hutan.Hutan` and the executable runs from a `.app`) and a test fake. Outside the bundle the daemon logs once at info level and uses no poster.
- The Cocoa side runs on one dedicated thread with its own run loop (verify that delegate callbacks arrive there; if they only arrive on the main thread, move the tokio runtime off the main thread in `main.rs` and run the run loop on main). Bounded channels both ways; no blocking of the tokio runtime.
- The Cocoa side runs on the **main thread** (the spike proved delegate delivery there): `hutand`'s `main.rs` builds the tokio runtime on a dedicated thread and, when a real poster exists, runs `NSRunLoop.mainRunLoop` on the main thread until shutdown; without a poster the main thread just joins the runtime thread. Shutdown (signals, `daemon.shutdown`) must still end the process cleanly. Bounded channels both ways; nothing blocks the tokio runtime.
- Authorization is requested lazily before the first post, once per daemon run; denied → no further posts, intents stay in `notify.list`, one warning in the log. Settings `daemon.toml [notify] native = true` (false disables posting).
- Removal: an intent that is acked, acted on, expired or whose approval resolved elsewhere removes its delivered notification. A daemon restart re-posts nothing (intents are in memory).
- Bundle: `hutand` is signed with `--identifier dev.hutan.Hutan`; the xtask test asserts it.

## Acceptance
- Unit tests with the fake poster: category/action mapping per kind, option truncation, response routing (approval option → `approval.respond` with `decided_by` notifier; default → `ui.open`; dismiss → ack), removal on ack/act/expire/resolve, authorization-denied path, `native = false`.
- A test that the real poster is not constructed outside a `.app` (pure check on bundle path/identifier).
- `just check` green; `just bundle` builds and verifies.
- **Do not** call `UNUserNotificationCenter` from any test or run the bundled daemon: the manual demo with the user is done by the integrator (post approval from a fake Codex fixture → click Approve → turn continues; Open → desktop focuses the session; kill the daemon → click is lost, approval still pending).

## Out of scope
SMAppService "keep running", menu bar, Dock badge, global shortcut (M6-C); notarization; remote clients.

## Report
Summary, the run-loop thread finding, files changed, commands run with results, open questions. Commit on `slice/M6-B`. Do not edit docs/status.md or docs/roadmap.md.
