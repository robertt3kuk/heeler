# Packaging: the app bundle (roadmap M6.2)

`cargo run -p xtask -- bundle` (or `just bundle`) assembles `Hutan.app` from the three real executables of the workspace and signs it ad hoc. It does **not** launch the bundle, register anything with the system (no `SMAppService`, no `launchctl`), call a notification API, or touch the Keychain. Those steps are M6-B and run with the user present; ADR 0007 stays *proposed* until they have been exercised.

Code: `xtask/src/bundle.rs`, `xtask/src/icon.rs`. Assets: `assets/macos/`, brand sources in `assets/brand/`.

## Layout

```text
Hutan.app/Contents/
  Info.plist                                   bundle id dev.hutan.Hutan, version of the workspace,
                                               LSMinimumSystemVersion 13.0, NSHighResolutionCapable
  MacOS/hutan-desktop                         the GPUI desktop (CFBundleExecutable)
  MacOS/hutand                                the daemon
  MacOS/hutan                                 the CLI
  Library/LaunchAgents/dev.hutan.daemon.plist BundleProgram Contents/MacOS/hutand, KeepAlive false,
                                               RunAtLoad false (inert until M6-B registers it)
  Resources/Hutan.icns                         the app icon (CFBundleIconFile)
  Resources/shell-integration/…                the OSC 133 / OSC 7 scripts (zsh, bash, fish)
```

- **The desktop executable is `hutan-desktop`, not `Hutan` (ADR 0007's sketch).** The default macOS volume is case-insensitive, so `Contents/MacOS/Hutan` and the CLI `Contents/MacOS/hutan` would be one file. The name users see is still *Hutan* (`CFBundleName`, `CFBundleDisplayName`, the folder name); the xtask test fails if two executables ever collide ignoring case.
- No `Contents/Helpers/Hutan Notifier.app`: native notification delivery (a helper or the daemon posting under the bundle id, ADR 0007's open question) is M6-B and enters the bundle only once it is real.
- The icon is the orangutan mascot `assets/brand/05-mascot/svg/icon-dark.svg`. `cargo run -p xtask -- icon` (or `just icon`) renders it into the 824-pixel tile of Apple's 1024 grid at every `.iconset` size and runs `iconutil` to write `assets/macos/Hutan.icns`, which is committed; rerun it after changing the SVG.
- The scripts in `Resources/` are for inspection and for the installer to come; the daemon still installs its own embedded copy into `$HUTAN_HOME` (`docs/shell-integration.md`).

## Building and signing

```sh
just bundle                       # debug binaries → target/bundle/Hutan.app
just bundle --release             # release binaries
cargo run -p xtask -- bundle --out /tmp/x --bin-dir target/debug    # bundle binaries that are already built
```

`bundle` runs `cargo build --locked -p hutan-desktop -p hutan-daemon -p hutan-cli` (unless `--bin-dir` is given), copies the binaries (mode 0755), fills `@VERSION@` into `assets/macos/Info.plist`, then signs: `codesign --force --sign - --timestamp=none` on `hutand` (identifier `dev.hutan.daemon`) and `hutan` (`dev.hutan.cli`), then on the bundle (which signs `hutan-desktop` with the bundle identifier `dev.hutan.Hutan`). It ends with `codesign --verify --deep --strict` and prints the bundle's path; any failure is an error. An existing `Hutan.app` in the output folder is replaced.

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

`xtask`'s `bundle` tests run in `just test`: one assembles a bundle from the debug binaries `cargo build --workspace --bins` leaves in `target/debug` into a temporary folder, signs and verifies it, parses both plists with the `plist` crate (identifier, version, minimum system version, high-resolution flag, icon file, executable and `BundleProgram` targets, `KeepAlive`/`RunAtLoad` false), checks the layout (executables are Mach-O, names distinct ignoring case, no `Helpers`, the icon and the scripts in `Resources`), reads the signature back (`Signature=adhoc`, identifiers), proves verification fails after a byte of `hutand` changes, and checks that no process runs from the bundle. It takes about two seconds, so it stays in the gate; `just bundle` itself is not part of `just check`. CI notes: run `just bundle --release` as a build artifact step on macOS runners; no step may open the result.

## What M6-B has to do

- Native delivery of the intents of protocol 0.13 (`docs/protocol.md` § Notification intents): the Notifier (an `LSUIElement` helper) or the daemon posting under `dev.hutan.Hutan`, deciding ADR 0007's open question with the user present; it consumes `notify.intent` and answers with `notify.act`.
- Registering `dev.hutan.daemon.plist` with `SMAppService` from the desktop, and the Keychain behavior across rebuilds.
- A stable signing identity, and notarization when there is a team.
