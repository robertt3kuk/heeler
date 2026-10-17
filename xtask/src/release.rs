//! Local release preparation only: build, archive, checksum and render the tap.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::bundle;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const CASK: &str = include_str!("hutan.cask.rb.in");
const FORMULA: &str = include_str!("hutan.formula.rb.in");

pub fn run(tap: PathBuf) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "release requires an Apple Silicon Mac"
    );
    let root = bundle::workspace_root();
    let tap = tap.canonicalize().context("opening --tap directory")?;
    ensure!(tap.is_dir(), "--tap must be an existing directory");
    let status = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(&root)
        .output()
        .context("checking release source")?;
    ensure!(status.status.success(), "git status failed");
    ensure!(
        status.stdout.is_empty(),
        "commit source changes before release: the source archive is made from HEAD"
    );
    let out = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), PathBuf::from)
        .join("release-artifacts");
    fs::create_dir_all(&out)?;
    let out = out.canonicalize()?;
    let source = out.join(format!("Hutan-{VERSION}-source.tar.gz"));
    checked(
        Command::new("git")
            .current_dir(&root)
            .args(["archive", "--format=tar.gz"])
            .arg(format!("--prefix=Hutan-{VERSION}/"))
            .arg("--output")
            .arg(&source)
            .arg("HEAD"),
        "archiving committed release source",
    )?;
    // The deployment target is part of the release contract, not the build host's default.
    // The child Cargo build inherits it; it also covers GPUI's native compilation.
    let app = bundle::build_bundle(true, Some(out.clone()))?;
    let zip = out.join(format!("Hutan-{VERSION}-arm64.zip"));
    checked(
        Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&app)
            .arg(&zip),
        "zipping release bundle",
    )?;
    let app_sha = sha256(&zip)?;
    let source_sha = sha256(&source)?;
    for (directory, template, checksum) in
        [("Casks", CASK, &app_sha), ("Formula", FORMULA, &source_sha)]
    {
        fs::create_dir_all(tap.join(directory))?;
        fs::write(
            tap.join(directory).join("hutan.rb"),
            render(template, VERSION, checksum),
        )?;
    }
    println!("{app_sha}  {}", zip.display());
    println!("{source_sha}  {}", source.display());
    println!("Updated {}/{{Casks,Formula}}/hutan.rb", tap.display());
    println!("Nothing published. Upload both archives to GitHub release v{VERSION}.");
    Ok(())
}

fn checked(command: &mut Command, description: &str) -> Result<()> {
    let status = command.status().with_context(|| description.to_owned())?;
    ensure!(status.success(), "{description} failed: {status}");
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .context("computing SHA-256")?;
    ensure!(output.status.success(), "shasum failed");
    let text = String::from_utf8(output.stdout).context("decoding shasum output")?;
    let digest = text.split_whitespace().next().context("missing SHA-256")?;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid SHA-256"
    );
    Ok(digest.to_owned())
}

fn render(template: &str, version: &str, checksum: &str) -> String {
    template
        .replace("@VERSION@", version)
        .replace("@SHA256@", checksum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_pin_both_release_assets() {
        for (template, asset) in [(CASK, "arm64.zip"), (FORMULA, "source.tar.gz")] {
            let text = render(template, "1.2.3", &"a".repeat(64));
            assert!(text.contains("version \"1.2.3\""));
            assert!(text.contains(&format!("sha256 \"{}\"", "a".repeat(64))));
            assert!(text.contains(asset));
            assert!(!text.contains("@VERSION@"));
            assert!(!text.contains("@SHA256@"));
        }
    }
}
