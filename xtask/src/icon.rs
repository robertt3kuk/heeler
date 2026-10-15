//! `cargo run -p xtask -- icon`: renders the app icon from the brand SVG into
//! `assets/macos/Hutan.icns` (`docs/packaging.md`). The source is a full-bleed tile; it is drawn
//! into the 824-pixel macOS icon grid of a 1024 canvas, transparent around it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

/// The brand icon the app ships, relative to the workspace root.
pub const SOURCE: &str = "assets/brand/05-mascot/svg/icon-dark.svg";
/// The generated icon file, relative to the workspace root; the bundle copies it.
pub const ICNS: &str = "assets/macos/Hutan.icns";
/// Share of the canvas the tile covers (Apple's grid: 824 of 1024).
const TILE: f32 = 824.0 / 1024.0;
/// Every size an `.iconset` holds: `(points, scale)`.
const SIZES: [(u32, u32); 10] = [
    (16, 1),
    (16, 2),
    (32, 1),
    (32, 2),
    (128, 1),
    (128, 2),
    (256, 1),
    (256, 2),
    (512, 1),
    (512, 2),
];

pub fn run(root: &Path) -> Result<()> {
    let svg = fs::read(root.join(SOURCE)).with_context(|| format!("reading {SOURCE}"))?;
    let tree = Tree::from_data(&svg, &Options::default()).context("parsing the icon SVG")?;
    let work = tempfile::tempdir()?;
    let iconset = work.path().join("Hutan.iconset");
    fs::create_dir(&iconset)?;
    for (points, scale) in SIZES {
        let pixels = points * scale;
        let name = if scale == 1 {
            format!("icon_{points}x{points}.png")
        } else {
            format!("icon_{points}x{points}@{scale}x.png")
        };
        render(&tree, pixels)?
            .save_png(iconset.join(&name))
            .with_context(|| format!("writing {name}"))?;
    }
    let out: PathBuf = root.join(ICNS);
    let status = Command::new("iconutil")
        .args(["--convert", "icns", "--output"])
        .arg(&out)
        .arg(&iconset)
        .status()
        .context("running iconutil")?;
    ensure!(status.success(), "iconutil failed");
    println!("{}", out.display());
    Ok(())
}

/// The icon at `pixels` square: the tile centered on the grid, transparent margin.
fn render(tree: &Tree, pixels: u32) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(pixels, pixels).context("allocating the icon")?;
    #[allow(clippy::cast_precision_loss)] // icon sizes are at most 1024
    let canvas = pixels as f32;
    let side = canvas * TILE;
    let scale = side / tree.size().width().max(tree.size().height());
    let offset = (canvas - side) / 2.0;
    resvg::render(
        tree,
        Transform::from_scale(scale, scale).post_translate(offset, offset),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}
