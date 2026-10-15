# Spike: native notifications from the daemon vs a helper (M6-B, ADR 0007)

Date: 2026-10-15. macOS 26.6.2 (arm64), ad-hoc signatures, user present for every prompt and click.

## Question
ADR 0007 left open whether `hutand` inside `Hutan.app/Contents/MacOS` can post `UNUserNotificationCenter` notifications with actions under the app's bundle id, or whether a separate `LSUIElement` Notifier helper is needed; and which process receives action clicks.

## Setup
A throwaway Rust program (objc2 0.6.5, objc2-user-notifications 0.3.2) in a test bundle `HutanSpike.app` (`dev.hutan.spike`, `LSUIElement`), copied three times: the main executable `Contents/MacOS/spike-main`, a second executable `Contents/MacOS/spike-daemon`, and a helper app in `Contents/Helpers`. It sets a delegate, registers a category with Approve (background), Deny (destructive) and Open (foreground) actions, requests authorization, posts one notification and logs every delegate callback while its main run loop runs. Facts come from that log and from `usernoted`/`usernotificationsd` in the unified log. The spike bundle was removed afterwards.

## Facts

| # | Setup | Result |
|---|---|---|
| 1 | Bundle under `/tmp` | `usernoted`: "Failed to find or validate client"; authorization denied without a prompt. Bundles must live in a normal location (home folder, `/Applications`). |
| 2 | `spike-daemon` signed with its **own** identifier `dev.hutan.spike.daemon` | `usernotificationsd`: "Entitlement 'com.apple.private.usernotifications.bundle-identifiers' required"; every call refused. |
| 3 | `spike-daemon` signed with the **app's** identifier `dev.hutan.spike`, started directly (not through LaunchServices) | Permission prompt shown for the app; after Allow, `authorization granted=true`, notification posted. |
| 4 | Click on the banner body (default action) while the poster runs | Delivered to the poster (`ACTION com.apple.UNNotificationDefaultActionIdentifier`); LaunchServices also launched the main executable. |
| 5 | **Approve** (background action) while the poster runs | Delivered to the poster (`ACTION approve`); nothing else launched. |
| 6 | **Approve** after the poster exited, nothing running | `usernoted`: "Failed to find appropriate application to launch … Failed to find app for … response"; the notification is removed and the response is **dropped**. |

## Decision (ADR 0007)
- **The daemon posts.** No Notifier helper. `hutand` is signed with the app's identifier `dev.hutan.Hutan` (not `dev.hutan.daemon`), so it shares the app's notification permission and its notifications show as Hutan. Posting works only from the signed bundle; a `cargo run` daemon keeps intents in `hutan notify watch`.
- **The daemon must be running to receive action clicks** (fact 6). A click while it is down is lost; that is safe (an approval stays pending and is answered in the app or CLI), and "keep running" (the `SMAppService` launch agent, M6.4) is the remedy. Foreground actions (Open) also bring up the app through LaunchServices (fact 4).
- Delegate callbacks need a Cocoa run loop: the daemon runs one on a dedicated thread (`NSRunLoop` of that thread, or the main thread with tokio moved off it), verified in B2.

## Open
- Whether delegate callbacks arrive on a non-main run-loop thread (fact 5 used the main thread) — verify first in B2.
- Keychain prompts now that `hutand` carries the app identifier (ADR 0008): expected to match the app's items; check with the user.
