# Packaging: the app bundle (roadmap M6.2)

`cargo run -p xtask -- bundle` (or `just bundle`) assembles `Hutan.app` from the three real executables of the workspace and signs it ad hoc. It does **not** launch the bundle, register anything with the system (no `SMAppService`, no `launchctl`), call a notification API, or touch the Keychain. The bundled daemon posts native notifications (M6-B, ADR 0007 accepted, § Native notifications below); trying that out is done by hand with the user present.

Code: `xtask/src/bundle.rs`, `xtask/src/icon.rs`. Assets: `assets/macos/`, brand sources in `assets/brand/`.

## Layout

```text
Hutan.app/Contents/
  Info.plist                                   bundle id dev.hutan.Hutan, version of the workspace,
                                               LSMinimumSystemVersion 13.0, NSHighResolutionCapable
  MacOS/hutan-desktop                         the GPUI desktop (CFBundleExecutable)
  MacOS/hutand                                the daemon (signed with the app's identifier dev.hutan.Hutan)
  MacOS/hutan                                 the CLI
  Library/LaunchAgents/dev.hutan.daemon.plist BundleProgram Contents/MacOS/hutand, KeepAlive false,
                                               RunAtLoad false (inert until M6-B registers it)
  Resources/Hutan.icns                         the app icon (CFBundleIconFile)
  Resources/shell-integration/…                the OSC 133 / OSC 7 scripts (zsh, bash, fish)
```

- **The desktop executable is `hutan-desktop`, not `Hutan` (ADR 0007's sketch).** The default macOS volume is case-insensitive, so `Contents/MacOS/Hutan` and the CLI `Contents/MacOS/hutan` would be one file. The name users see is still *Hutan* (`CFBundleName`, `CFBundleDisplayName`, the folder name); the xtask test fails if two executables ever collide ignoring case.
- No `Contents/Helpers/Hutan Notifier.app`: the daemon posts the notifications itself under the bundle id (ADR 0007, § Native notifications); the xtask test fails if a `Helpers` folder appears.
- The icon is the orangutan mascot `assets/brand/05-mascot/svg/icon-dark.svg`. `cargo run -p xtask -- icon` (or `just icon`) renders it into the 824-pixel tile of Apple's 1024 grid at every `.iconset` size and runs `iconutil` to write `assets/macos/Hutan.icns`, which is committed; rerun it after changing the SVG.
- The scripts in `Resources/` are for inspection and for the installer to come; the daemon still installs its own embedded copy into `$HUTAN_HOME` (`docs/shell-integration.md`).

## Building and signing

```sh
just bundle                       # debug binaries → target/bundle/Hutan.app
just bundle --release             # release binaries
cargo run -p xtask -- bundle --out /tmp/x --bin-dir target/debug    # bundle binaries that are already built
```

`bundle` runs `cargo build --locked -p hutan-desktop -p hutan-daemon -p hutan-cli` (unless `--bin-dir` is given), copies the binaries (mode 0755), fills `@VERSION@` into `assets/macos/Info.plist`, then signs: `codesign --force --sign - --timestamp=none` on `hutand` (identifier `dev.hutan.Hutan`, **the app's**: see below) and `hutan` (`dev.hutan.cli`), then on the bundle (which signs `hutan-desktop` with the bundle identifier `dev.hutan.Hutan`). It ends with `codesign --verify --deep --strict` and prints the bundle's path; any failure is an error. An existing `Hutan.app` in the output folder is replaced.

Ad-hoc signatures have no team identity: Gatekeeper treats a copy that was downloaded or AirDropped as unsigned, and the designated requirement of an ad-hoc signature is the code hash, so **every rebuild is a new identity** for the Keychain (ADR 0008: items are bound to the signing identity; expect prompts after each rebuild until a stable identity is used). Neither is a problem for a bundle built and run locally.

## Where the executables find each other

Everything resolves by location, so the bundle works wherever it is put:

| Who | Looks for | Where |
|---|---|---|
| `hutan-desktop` | `hutand` to autostart | beside its own executable (`desktop::app::hutand_beside`), else `PATH` |
| `hutan` | `hutand` to autostart (`hutan daemon start` and the commands that start it) | beside itself, as invoked and with symlinks resolved (so a `/usr/local/bin/hutan` link into the bundle finds the bundled daemon), else `PATH` |
| `hutand` | `hutan` for `HUTAN_BIN` in sessions | beside itself (also through symlinks), else `PATH` |

A unit test per crate builds a fake bundle tree (`Hutan.app/Contents/MacOS/{hutan-desktop,hutand,hutan}`, plus a symlinked CLI) and checks each rule.

CLI startup is explicit: `hutan daemon start` (and `hutan index enable`) requests autostart.
Queries such as `hutan notify ls` do not start a daemon that is down.

## Tests and the gate

`xtask`'s `bundle` tests run in `just test`: one assembles a bundle from the debug binaries `cargo build --workspace --bins` leaves in `target/debug` into a temporary folder, signs and verifies it, parses both plists with the `plist` crate (identifier, version, minimum system version, high-resolution flag, icon file, executable and `BundleProgram` targets, `KeepAlive`/`RunAtLoad` false), checks the layout (executables are Mach-O, names distinct ignoring case, no `Helpers`, the icon and the scripts in `Resources`), reads the signature back (`Signature=adhoc`, `hutand` carries `Identifier=dev.hutan.Hutan`), proves verification fails after a byte of `hutand` changes, and checks that no process runs from the bundle. It takes about two seconds, so it stays in the gate; `just bundle` itself is not part of `just check`. CI notes: run `just bundle --release` as a build artifact step on macOS runners; no step may open the result.

## Native notifications (M6-B)

Code: `crates/daemon/src/notify_native.rs` and `notify_native/`; the wire side is `docs/protocol.md` § Notification intents → Native delivery.

- **`hutand` is signed with the app's identifier `dev.hutan.Hutan`, not `dev.hutan.daemon`.** The system gives notification permission to an app identity: a second executable inside `Contents/MacOS` that is signed with its own identifier is refused (`com.apple.private.usernotifications.bundle-identifiers` is required), one signed with the app's identifier shares the app's permission and its notifications are Hutan's (spike `docs/spikes/2026-10-15-notifications.md`, facts 2–3). The same identity serves the Keychain (ADR 0008). The cost: `codesign -dv hutand` and the app show one identifier, so tools that tell them apart do it by path.
- **Only from the bundle.** The daemon builds the real poster only when its executable is inside a `.app` (`Contents/MacOS/<exe>`) and the main bundle's identifier is `dev.hutan.Hutan`. A daemon started from `cargo run`, `target/debug`, a test, or a bundle in `/tmp` (rejected by `usernoted`, spike fact 1; keep the bundle in a normal location such as the home folder) posts nothing. The first thing it does is log, once, `native notifications are off: not running from Hutan.app`; nothing from the notification framework runs before the first post.
- **Run loop.** In the bundle the main thread of `hutand` is the Cocoa run loop and tokio runs on the thread `hutand-runtime` (`crates/daemon/src/main.rs`); the delegate's callbacks (permission answer, clicks) arrive on the main thread. Outside the bundle the main thread just joins the runtime thread. The `main_thread_loop` test (its own `main`, `harness = false`) runs the loop on the real main thread of a test process, without any notification API.
- **Permission.** The first post makes macOS ask once ("Hutan would like to send you notifications"). If the user says no, the daemon says so once in `hutand.log`, posts nothing more that run (it asks again after a restart), and the intents stay in `hutan notify ls|watch`. `daemon.toml [notify] native = false` turns posting off without touching the permission.
- **What a click needs.** The daemon must be running when the user clicks: a click while it is down is dropped by the system. Approvals then stay pending and are answered in the app or the CLI. "Keep running" (M6-C) is the remedy.
- **Trying it by hand** (not part of any test; needs the user at the machine): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory (not `/tmp`), start its `hutand` with `HUTAN_HOME` set to a scratch folder, let a fake Codex raise an approval, click *Approve*, *Open* and the body, then kill the daemon and click again (the click is lost, the approval pending). Never run it against the real data folder.

## What is still open (M6-C and later)

- Registering `dev.hutan.daemon.plist` with `SMAppService` from the desktop ("keep running"), the menu bar item, Dock badge and global shortcut (M6-C), and the Keychain behavior across rebuilds.
- A stable signing identity, and notarization when there is a team.
