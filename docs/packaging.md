# Packaging: the app bundle (roadmap M6.2)

`cargo run -p xtask -- bundle` (or `just bundle`) assembles `Hutan.app` from the three real executables of the workspace and signs it ad hoc. It does **not** launch the bundle, register anything with the system (no `SMAppService`, no `launchctl`), call a notification API, or touch the Keychain. The desktop registers the LaunchAgent itself, only when the user turns on "start at login" (§ Start at login). The bundled daemon posts native notifications (M6-B, ADR 0007 accepted, § Native notifications below); trying that out is done by hand with the user present.

Code: `xtask/src/bundle.rs`, `xtask/src/icon.rs`. Assets: `assets/macos/`, brand sources in `assets/brand/`.

## Layout

```text
Hutan.app/Contents/
  Info.plist                                   bundle id dev.hutan.Hutan, version of the workspace,
                                               LSMinimumSystemVersion 13.0, NSHighResolutionCapable,
                                               LSMultipleInstancesProhibited true
  MacOS/hutan-desktop                         the GPUI desktop (CFBundleExecutable)
  MacOS/hutand                                the daemon (signed with the app's identifier dev.hutan.Hutan)
  MacOS/hutan                                 the CLI
  Library/LaunchAgents/dev.hutan.daemon.plist BundleProgram Contents/MacOS/hutand, RunAtLoad true,
                                               KeepAlive {Crashed: true}; inert until "keep running"
                                               registers it (M6-C)
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

`xtask`'s `bundle` tests run in `just test`: one assembles a bundle from the debug binaries `cargo build --workspace --bins` leaves in `target/debug` into a temporary folder, signs and verifies it, parses both plists with the `plist` crate (identifier, version, minimum system version, high-resolution flag, `LSMultipleInstancesProhibited`, icon file, executable and `BundleProgram` targets, `RunAtLoad` true, `KeepAlive` a dictionary with only `Crashed`, no environment, arguments or log paths), checks the layout (executables are Mach-O, names distinct ignoring case, no `Helpers`, the icon and the scripts in `Resources`), reads the signature back (`Signature=adhoc`, `hutand` carries `Identifier=dev.hutan.Hutan`), proves verification fails after a byte of `hutand` changes, and checks that no process runs from the bundle. It takes about two seconds, so it stays in the gate; `just bundle` itself is not part of `just check`. CI notes: run `just bundle --release` as a build artifact step on macOS runners; no step may open the result.

## Native notifications (M6-B)

Code: `crates/daemon/src/notify_native.rs` and `notify_native/`; the wire side is `docs/protocol.md` § Notification intents → Native delivery.

- **`hutand` is signed with the app's identifier `dev.hutan.Hutan`, not `dev.hutan.daemon`.** The system gives notification permission to an app identity: a second executable inside `Contents/MacOS` that is signed with its own identifier is refused (`com.apple.private.usernotifications.bundle-identifiers` is required), one signed with the app's identifier shares the app's permission and its notifications are Hutan's (spike `docs/spikes/2026-10-15-notifications.md`, facts 2–3). The same identity serves the Keychain (ADR 0008). The cost: `codesign -dv hutand` and the app show one identifier, so tools that tell them apart do it by path.
- **Only from the bundle.** The daemon builds the real poster only when its executable is inside a `.app` (`Contents/MacOS/<exe>`) and the main bundle's identifier is `dev.hutan.Hutan`. A daemon started from `cargo run`, `target/debug`, a test, or a bundle in `/tmp` (rejected by `usernoted`, spike fact 1; keep the bundle in a normal location such as the home folder) posts nothing. The first thing it does is log, once, `native notifications are off: not running from Hutan.app`; nothing from the notification framework runs before the first post.
- **Run loop.** In the bundle the main thread of `hutand` is the Cocoa run loop and tokio runs on the thread `hutand-runtime` (`crates/daemon/src/main.rs`); the delegate's callbacks (permission answer, clicks) arrive on the main thread. Outside the bundle the main thread just joins the runtime thread. The `main_thread_loop` test (its own `main`, `harness = false`) runs the loop on the real main thread of a test process, without any notification API.
- **Permission.** The first post makes macOS ask once ("Hutan would like to send you notifications"). If the user says no, the daemon says so once in `hutand.log`, posts nothing more that run (it asks again after a restart), and the intents stay in `hutan notify ls|watch`. `daemon.toml [notify] native = false` turns posting off without touching the permission.
- **What a click needs.** The daemon must be running when the user clicks: a click while it is down is dropped by the system. Approvals then stay pending and are answered in the app or the CLI. "Start at login" or "Keep running" is the remedy.
- **Trying it by hand** (not part of any test; needs the user at the machine): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory (not `/tmp`), start its `hutand` with `HUTAN_HOME` set to a scratch folder, let a fake Codex raise an approval, click *Approve*, *Open* and the body, then kill the daemon and click again (the click is lost, the approval pending). Never run it against the real data folder.

## Start at login (M6-C, D-2)

This was "keep running" before D-2. Keep running (`app.keep_running`) now only leaves the daemon running after quit and registers nothing; both are off by default and the first-run setup offers them (`docs/design.md` § First-run setup).

Code: `crates/desktop/src/macos/login_item.rs`, `crates/desktop/src/workbench/mac.rs`; behavior in `docs/design.md` § macOS integration. The desktop registers `Contents/Library/LaunchAgents/dev.hutan.daemon.plist` with `SMAppService.agent(plistName:)` (`register()` / `unregister()`), which adds *Hutan* to System Settings → General → Login Items (macOS shows a notice the first time, and the user may have to allow it: status `requires approval`, button *Open Login Items*). Registration needs the app to run from the bundle; a `cargo run` desktop disables the toggle.

The agent plist, and why:

- `BundleProgram` `Contents/MacOS/hutand`, no `ProgramArguments`, no `EnvironmentVariables`: launchd starts the signed bundled daemon with no `HUTAN_*` variables, so it uses the standard data folder, socket and `hutand.log` (it writes its own log; no `StandardOutPath` is set, so nothing else is created). The desktop of the same user finds that socket by the same default.
- `RunAtLoad` **true**: the daemon starts when the agent is registered and at every login, so a notification click always has a daemon to reach (the point of starting at login, ADR 0007).
- `KeepAlive` `{ Crashed: true }`: launchd restarts the daemon after a crash and **only** then. `hutan daemon stop` and "Stop Daemon" end `hutand` with exit 0, and a daemon that finds another one holding the lock exits 2; neither may bring one back, which a bare `KeepAlive true` would. The xtask bundle test pins all three.
- `AssociatedBundleIdentifiers` `dev.hutan.Hutan`: the Login Items entry is shown as Hutan.

While start at login is active (registered and approved) the desktop starts with a connection that only dials, waits 4 s for a launchd-started daemon, and only then starts `hutand` itself; quitting the app never touches the daemon. The race that matters is a login-time one: the desktop and launchd both starting `hutand`. The daemon's lock makes the loser exit 2, so it cannot corrupt anything, but the desktop avoids the second start altogether.

**Trying it by hand** (not part of any test; needs the user at the machine; never against the real data folder unless the user wants to): `just bundle`, copy `target/bundle/Hutan.app` to a folder under the home directory, start it, turn *Start daemon at login* on in Settings (`app`) or the Daemon menu, approve it in Login Items if asked, quit the app and check `launchctl print gui/$(id -u)/dev.hutan.daemon` and `hutan daemon status`; turn it off and check that the entry is gone and what happened to the running daemon.

## One desktop per data folder

`LSMultipleInstancesProhibited` in `Info.plist` makes Launch Services reactivate the running Hutan when it is opened again (Dock, Finder, `open`); the xtask bundle test checks the key. It does not cover `open -n` or running `Contents/MacOS/hutan-desktop` directly, so `hutan-desktop` enforces it too (`crates/desktop/src/instance.rs`, behavior in `docs/design.md` § Daemon lifecycle): before any window opens it takes an exclusive `flock` on **`desktop.lock`** in the data folder (`$HUTAN_HOME`, else `~/Library/Application Support/Hutan`) and listens on **`desktop.sock`** next to it (mode `0600`, peers of another user dropped, a stale socket file from a crash replaced under the lock). A second desktop sends `{"cmd":"activate"}` to that socket and exits 0; the first one brings its window forward. Two desktops with different `HUTAN_HOME`s do not know each other, which is what tests and `just bench` rely on.

## Menu bar glyph

`assets/macos/MenuBarIcon.png` (18 px) and `MenuBarIcon@2x.png` (36 px) are committed, black template images (only the alpha channel counts; macOS tints them for light, dark and highlighted states). `just menubar-icon` (`cargo run -p xtask -- menubar-icon`) renders them from `assets/brand/05-mascot/svg/favicon.svg` without its background tile and cuts the eyes and nostrils out; an xtask test checks sizes, that every pixel is black, and that rendering again reproduces the committed files. The desktop embeds them (`include_bytes!`), so the executable needs no resource folder.

## What is still open

- The Keychain behavior across rebuilds.
- A stable signing identity, and notarization when there is a team.
