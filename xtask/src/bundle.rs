//! `cargo run -p xtask -- bundle [--release] [--out DIR]`: assembles `Heeler.app` and signs it
//! ad hoc (`docs/packaging.md`).
//!
//! ```text
//! Heeler.app/Contents/
//!   Info.plist                                   assets/macos/Info.plist, version filled in
//!   MacOS/heeler-desktop  heelerd  heeler        the three real executables of the workspace
//!   Library/LaunchAgents/dev.heeler.daemon.plist assets/macos/dev.heeler.daemon.plist
//!   Resources/shell-integration/…                the OSC 133 / OSC 7 scripts
//! ```
//!
//! Nothing here runs the bundle or registers it with the system: the only programs started are
//! `cargo build` (unless `--bin-dir` names binaries that are already built) and `codesign`.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

/// The folder name of the bundle; its `Contents/MacOS` executable names must stay distinct
/// ignoring case (the default macOS volume is case-insensitive).
pub const APP_NAME: &str = "Heeler.app";
/// The workspace version, written to `CFBundleShortVersionString` and `CFBundleVersion`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `(file name inside Contents/MacOS, binary name Cargo produces, code-signing identifier)`.
/// The desktop is the bundle's main executable, so signing the bundle gives it the bundle id.
const EXECUTABLES: [(&str, &str, &str); 3] = [
    ("heeler-desktop", "heeler-desktop", "dev.heeler.Heeler"),
    ("heelerd", "heelerd", "dev.heeler.daemon"),
    ("heeler", "heeler", "dev.heeler.cli"),
];
const PACKAGES: [&str; 3] = ["heeler-desktop", "heeler-daemon", "heeler-cli"];

#[derive(Debug)]
pub struct Options {
    pub release: bool,
    /// Where `Heeler.app` is created; default `target/bundle`.
    pub out: Option<PathBuf>,
    /// Use the binaries already built in this folder instead of running `cargo build`.
    pub bin_dir: Option<PathBuf>,
}

pub fn run(options: Options) -> Result<()> {
    let root = workspace_root();
    let bin_dir = match options.bin_dir {
        Some(dir) => dir,
        None => build(&root, options.release)?,
    };
    let out = options
        .out
        .unwrap_or_else(|| target_dir(&root).join("bundle"));
    let app = assemble(&root, &bin_dir, &out)?;
    sign(&app)?;
    verify(&app)?;
    println!("{}", app.display());
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

/// Builds the three binaries; returns the folder they are in.
fn build(root: &Path, release: bool) -> Result<PathBuf> {
    let profile = if release { "release" } else { "debug" };
    eprintln!("building the {profile} binaries");
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    cargo.args(["build", "--locked"]);
    if release {
        cargo.arg("--release");
    }
    for package in PACKAGES {
        cargo.args(["-p", package]);
    }
    let status = cargo
        .current_dir(root)
        .status()
        .context("running cargo build")?;
    ensure!(status.success(), "cargo build failed");
    Ok(target_dir(root).join(profile))
}

/// Creates `out/Heeler.app` from the binaries in `bin_dir` and the assets; replaces a bundle
/// that is already there. Returns the bundle's path. Does not sign.
pub fn assemble(root: &Path, bin_dir: &Path, out: &Path) -> Result<PathBuf> {
    let app = out.join(APP_NAME);
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let sources: Vec<PathBuf> = EXECUTABLES
        .iter()
        .map(|(_, binary, _)| bin_dir.join(binary))
        .collect();
    for source in &sources {
        ensure!(
            source.is_file(),
            "{} is missing; build it (`cargo build -p heeler-desktop -p heeler-daemon -p heeler-cli`) or pass --bin-dir",
            source.display()
        );
    }

    if app.exists() {
        fs::remove_dir_all(&app).with_context(|| format!("removing the old {}", app.display()))?;
    }
    let launch_agents = contents.join("Library/LaunchAgents");
    let resources = contents.join("Resources");
    for dir in [&macos, &launch_agents, &resources] {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    for ((name, _, _), source) in EXECUTABLES.iter().zip(&sources) {
        let target = macos.join(name);
        fs::copy(source, &target)
            .with_context(|| format!("copying {} to {}", source.display(), target.display()))?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
    }

    let assets = root.join("assets/macos");
    let info = fs::read_to_string(assets.join("Info.plist")).context("reading Info.plist")?;
    ensure!(
        info.contains("@VERSION@"),
        "assets/macos/Info.plist has no @VERSION@ placeholder"
    );
    fs::write(
        contents.join("Info.plist"),
        info.replace("@VERSION@", VERSION),
    )?;
    fs::copy(
        assets.join("dev.heeler.daemon.plist"),
        launch_agents.join("dev.heeler.daemon.plist"),
    )
    .context("copying the LaunchAgent plist")?;
    copy_tree(
        &root.join("crates/terminal/shell-integration"),
        &resources.join("shell-integration"),
    )?;
    Ok(app)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Ad-hoc signs the executables, then the bundle (`codesign --force --sign -
/// --timestamp=none`). No identity, no keychain, no network.
pub fn sign(app: &Path) -> Result<()> {
    let macos = app.join("Contents/MacOS");
    // The main executable is signed with the bundle, which gives it the bundle identifier.
    for (name, _, identifier) in EXECUTABLES.iter().skip(1) {
        codesign(&[
            "--identifier",
            identifier,
            &macos.join(name).display().to_string(),
        ])?;
    }
    codesign(&[&app.display().to_string()])
}

fn codesign(args: &[&str]) -> Result<()> {
    let output = Command::new("codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .args(args)
        .output()
        .context("running codesign (macOS command line tools)")?;
    if !output.status.success() {
        bail!(
            "codesign {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// `codesign --verify --deep --strict`: the signature is intact and covers everything.
pub fn verify(app: &Path) -> Result<()> {
    let output = Command::new("codesign")
        .args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(app)
        .output()
        .context("running codesign")?;
    if !output.status.success() {
        bail!(
            "{} does not verify: {}",
            app.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The binaries `cargo build --workspace --bins` (part of `just test`) left next to the test
    /// binaries; the test does not build them itself.
    fn built_binaries() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(Path::parent).unwrap().to_owned();
        for (_, binary, _) in EXECUTABLES {
            assert!(
                dir.join(binary).is_file(),
                "{binary} is not built in {}: run `cargo build --workspace --bins` (or `just test`)",
                dir.display()
            );
        }
        dir
    }

    fn dictionary(path: &Path) -> plist::Dictionary {
        plist::Value::from_file(path)
            .unwrap_or_else(|e| panic!("{} is not a plist: {e}", path.display()))
            .into_dictionary()
            .expect("a dictionary plist")
    }

    fn string<'a>(dict: &'a plist::Dictionary, key: &str) -> &'a str {
        dict.get(key)
            .and_then(plist::Value::as_string)
            .unwrap_or_else(|| panic!("{key} is missing or not a string"))
    }

    fn running_commands() -> String {
        let out = Command::new("ps")
            .args(["-axo", "command="])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn a_debug_bundle_has_the_layout_the_plists_and_a_valid_ad_hoc_signature() {
        let dir = tempfile::tempdir().unwrap();
        let root = workspace_root();
        let app = assemble(&root, &built_binaries(), dir.path()).unwrap();
        sign(&app).unwrap();
        verify(&app).unwrap();

        // Layout: the three real executables, nothing else in MacOS.
        let macos = app.join("Contents/MacOS");
        let names: BTreeSet<String> = fs::read_dir(&macos)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["heeler", "heeler-desktop", "heelerd"]
                .map(str::to_owned)
                .into_iter()
                .collect()
        );
        let lowercase: BTreeSet<String> = names.iter().map(|n| n.to_lowercase()).collect();
        assert_eq!(
            lowercase.len(),
            names.len(),
            "executable names collide on a case-insensitive volume"
        );
        for name in &names {
            let path = macos.join(name);
            let meta = fs::metadata(&path).unwrap();
            assert!(
                meta.is_file() && meta.permissions().mode() & 0o111 == 0o111,
                "{name}"
            );
            let head = fs::read(&path).unwrap();
            let magic = u32::from_be_bytes(head[..4].try_into().unwrap());
            assert!(
                [0xcffa_edfe, 0xcafe_babe].contains(&magic),
                "{name} is not a Mach-O executable ({magic:#x}): no placeholders in a bundle"
            );
        }

        // Info.plist.
        let info = dictionary(&app.join("Contents/Info.plist"));
        assert_eq!(string(&info, "CFBundleIdentifier"), "dev.heeler.Heeler");
        assert_eq!(string(&info, "CFBundleShortVersionString"), VERSION);
        assert_eq!(string(&info, "CFBundleVersion"), VERSION);
        assert_eq!(string(&info, "LSMinimumSystemVersion"), "13.0");
        assert_eq!(string(&info, "CFBundlePackageType"), "APPL");
        assert_eq!(
            info.get("NSHighResolutionCapable")
                .and_then(plist::Value::as_boolean),
            Some(true)
        );
        let executable = string(&info, "CFBundleExecutable");
        assert!(macos.join(executable).is_file(), "{executable}");
        assert!(
            !info.contains_key("CFBundleIconFile"),
            "no placeholder icon is shipped"
        );
        assert!(
            !fs::read_to_string(app.join("Contents/Info.plist"))
                .unwrap()
                .contains('@'),
            "every placeholder was filled in"
        );

        // The LaunchAgent: present, pointing into the bundle, and inert until registered.
        let agent = dictionary(&app.join("Contents/Library/LaunchAgents/dev.heeler.daemon.plist"));
        assert_eq!(string(&agent, "Label"), "dev.heeler.daemon");
        let program = string(&agent, "BundleProgram");
        assert_eq!(program, "Contents/MacOS/heelerd");
        assert!(app.join(program).is_file());
        for key in ["KeepAlive", "RunAtLoad"] {
            assert_eq!(
                agent.get(key).and_then(plist::Value::as_boolean),
                Some(false),
                "{key}"
            );
        }

        // Resources: the shell integration scripts.
        let scripts = app.join("Contents/Resources/shell-integration");
        for script in [
            "zsh/.zshenv",
            "zsh/heeler-integration.zsh",
            "bash/heeler.bash",
            "fish/vendor_conf.d/heeler-shell-integration.fish",
        ] {
            assert!(scripts.join(script).is_file(), "{script}");
        }

        // Nothing that belongs to M6-B.
        assert!(
            !app.join("Contents/Helpers").exists(),
            "no Notifier helper yet"
        );
        let resources: Vec<_> = fs::read_dir(app.join("Contents/Resources"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(resources, ["shell-integration"]);

        // Signature: ad hoc, with the identifiers we gave.
        let described = |path: &Path| {
            let out = Command::new("codesign")
                .args(["-dv", "--verbose=2"])
                .arg(path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stderr).into_owned()
        };
        let bundle = described(&app);
        assert!(bundle.contains("Signature=adhoc"), "{bundle}");
        assert!(bundle.contains("Identifier=dev.heeler.Heeler"), "{bundle}");
        let daemon = described(&macos.join("heelerd"));
        assert!(daemon.contains("Signature=adhoc"), "{daemon}");
        assert!(daemon.contains("Identifier=dev.heeler.daemon"), "{daemon}");
        let status = Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&app)
            .status()
            .unwrap();
        assert!(status.success());

        // The check is real: a changed executable no longer verifies.
        let mut tampered = fs::read(macos.join("heelerd")).unwrap();
        tampered.push(0);
        fs::write(macos.join("heelerd"), tampered).unwrap();
        assert!(verify(&app).is_err(), "a modified bundle must not verify");

        // Nothing was launched: no process runs anything from the bundle.
        let commands = running_commands();
        assert!(
            !commands.contains(&dir.path().display().to_string()),
            "something runs from the temporary bundle"
        );
    }

    #[test]
    fn assembling_again_replaces_the_bundle_and_missing_binaries_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = workspace_root();
        let bins = tempfile::tempdir().unwrap();
        let err = assemble(&root, bins.path(), dir.path()).unwrap_err();
        assert!(err.to_string().contains("heeler-desktop"), "{err}");

        for (_, binary, _) in EXECUTABLES {
            fs::write(bins.path().join(binary), b"not a program").unwrap();
        }
        let app = assemble(&root, bins.path(), dir.path()).unwrap();
        fs::write(app.join("stale"), "left over").unwrap();
        assemble(&root, bins.path(), dir.path()).unwrap();
        assert!(!app.join("stale").exists());
    }
}
