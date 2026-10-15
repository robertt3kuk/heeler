//! `cargo run -p xtask -- menubar-icon`: renders the menu bar glyph (`docs/packaging.md`,
//! `docs/design.md` § Menu bar status item) from the brand favicon into the committed PNG pair
//! `assets/macos/MenuBarIcon.png` (18 px) and `MenuBarIcon@2x.png` (36 px).
//!
//! The glyph is a *template image*: macOS tints it for the menu bar, so only the alpha channel
//! matters. The favicon's tile is dropped and the mascot is drawn as a black silhouette; the
//! darkest features (eyes, nostrils) are cut out of it.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

/// The brand source, relative to the workspace root.
pub const SOURCE: &str = "assets/brand/05-mascot/svg/favicon.svg";
/// The committed files, relative to the workspace root: `(path, pixels)`.
pub const OUTPUTS: [(&str, u32); 2] = [
    ("assets/macos/MenuBarIcon.png", 18),
    ("assets/macos/MenuBarIcon@2x.png", 36),
];
/// Share of the canvas the mascot covers; the artwork touches its viewbox edges.
const FILL: f32 = 0.94;

pub fn run(root: &Path) -> Result<()> {
    let svg = fs::read_to_string(root.join(SOURCE)).with_context(|| format!("reading {SOURCE}"))?;
    let tree = Tree::from_str(&without_tile(&svg)?, &Options::default())
        .context("parsing the favicon SVG")?;
    for (path, pixels) in OUTPUTS {
        let mut pixmap = render(&tree, pixels)?;
        to_template(&mut pixmap);
        let out = root.join(path);
        pixmap
            .save_png(&out)
            .with_context(|| format!("writing {path}"))?;
        println!("{}", out.display());
    }
    Ok(())
}

/// The SVG without its first element, the full-bleed background tile.
fn without_tile(svg: &str) -> Result<String> {
    let start = svg
        .find("<rect")
        .context("the favicon has no background tile")?;
    let end = start
        + svg[start..]
            .find("/>")
            .context("the background tile is not closed")?
        + 2;
    Ok(format!("{}{}", &svg[..start], &svg[end..]))
}

fn render(tree: &Tree, pixels: u32) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(pixels, pixels).context("allocating the glyph")?;
    #[allow(clippy::cast_precision_loss)] // glyph sizes are tiny
    let canvas = pixels as f32;
    let side = canvas * FILL;
    let scale = side / tree.size().width().max(tree.size().height());
    let offset = (canvas - side) / 2.0;
    resvg::render(
        tree,
        Transform::from_scale(scale, scale).post_translate(offset, offset),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

/// Black pixels whose alpha is the artwork's, minus the darkest features.
fn to_template(pixmap: &mut Pixmap) {
    for pixel in pixmap.pixels_mut() {
        let alpha = f32::from(pixel.alpha()) / 255.0;
        if alpha == 0.0 {
            continue;
        }
        // The pixmap is premultiplied: undo it to read the color.
        let channel = |value: u8| f32::from(value) / 255.0 / alpha;
        let luma = 0.2126 * channel(pixel.red())
            + 0.7152 * channel(pixel.green())
            + 0.0722 * channel(pixel.blue());
        let keep = ((luma - 0.1) / 0.08).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=255
        let alpha = (alpha * keep * 255.0).round() as u8;
        *pixel =
            resvg::tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, alpha).unwrap_or(*pixel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        super::super::bundle::workspace_root()
    }

    #[test]
    fn the_committed_glyphs_are_templates_of_the_right_size() {
        for (path, pixels) in OUTPUTS {
            let pixmap = Pixmap::load_png(root().join(path))
                .unwrap_or_else(|e| panic!("{path} is not a PNG: {e}"));
            assert_eq!(
                (pixmap.width(), pixmap.height()),
                (pixels, pixels),
                "{path}"
            );
            assert!(
                pixmap
                    .pixels()
                    .iter()
                    .all(|p| p.red() == 0 && p.green() == 0 && p.blue() == 0),
                "{path}: a template image is black; the alpha channel is the glyph"
            );
            let opaque = pixmap.pixels().iter().filter(|p| p.alpha() == 255).count();
            let clear = pixmap.pixels().iter().filter(|p| p.alpha() == 0).count();
            assert!(
                opaque > 0 && clear > 0,
                "{path}: a glyph, not a block or nothing"
            );
        }
    }

    #[test]
    fn rendering_again_reproduces_the_committed_files() {
        let svg = fs::read_to_string(root().join(SOURCE)).unwrap();
        let tree = Tree::from_str(&without_tile(&svg).unwrap(), &Options::default()).unwrap();
        for (path, pixels) in OUTPUTS {
            let mut fresh = render(&tree, pixels).unwrap();
            to_template(&mut fresh);
            let committed = Pixmap::load_png(root().join(path)).unwrap();
            assert_eq!(
                fresh.data(),
                committed.data(),
                "{path} is stale: run `just menubar-icon`"
            );
        }
    }
}
