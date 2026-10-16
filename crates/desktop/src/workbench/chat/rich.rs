//! Rich input of the composer (protocol 0.23, capability `chat.rich_input`): what a file or an
//! image the user dropped, picked or pasted, and a long paste, become — a chip of the next
//! message, drawn with a thumbnail, an icon and a size, that the daemon reads when the message is
//! sent (`docs/agents.md` § Rich input).
//!
//! A chip that cannot be sent (too big, a folder, not text, a format no model reads) is still a
//! chip: it carries the reason and says so before the user presses Send. Only a file's first
//! bytes and its size are read here, to tell text from binary; the daemon reads the rest.

use std::fs::File;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use hutan_protocol::assistant::{
    Attachment, IMAGE_INLINE_MAX_BYTES, IMAGE_MAX_BYTES, PASTED_TEXT_MAX_BYTES,
};

use super::context::{ContextChip, Thumb};

/// A pasted text longer than this many characters becomes a chip.
pub const LONG_PASTE_CHARS: usize = 2000;
/// A pasted text with more lines than this becomes a chip.
pub const LONG_PASTE_LINES: usize = 40;
/// Bytes of a file read to tell text from binary.
const SNIFF_BYTES: usize = 8 * 1024;

/// Whether pasting `text` folds it into a chip instead of typing it into the composer.
pub fn is_long_paste(text: &str) -> bool {
    text.chars().count() > LONG_PASTE_CHARS || text.lines().count() > LONG_PASTE_LINES
}

/// `1.5 MiB`, `120 KiB`, `800 B`.
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} KiB", bytes.div_ceil(KIB))
    } else {
        format!("{bytes} B")
    }
}

/// The media type of an image file by its extension: only what the daemon accepts.
fn image_type_of(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Image formats that exist but that no model of Hutan's backends takes.
fn is_other_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "svg" | "bmp" | "tif" | "tiff" | "ico" | "heic" | "heif" | "avif"
            )
        })
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The chip of a file the user dropped, picked or pasted from Finder: an image chip for a PNG,
/// JPEG, GIF or WebP file, else a file chip (the daemon reads it from where it is). The reason
/// it cannot be sent, when there is one, is on the chip.
pub fn chip_for_path(path: &Path) -> ContextChip {
    let name = name_of(path);
    let Some(text) = path.to_str() else {
        return placeholder_file(path, &name, "the path is not valid UTF-8");
    };
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return placeholder_file(path, &name, &format!("cannot read it: {error}"));
        }
    };
    if metadata.is_dir() {
        return placeholder_file(path, &name, "folders cannot be attached, only files");
    }
    if !metadata.is_file() {
        return placeholder_file(path, &name, "not a regular file");
    }
    let size = metadata.len();
    if let Some(media_type) = image_type_of(path) {
        let mut chip = ContextChip::new(
            Attachment::Image {
                path: Some(text.to_owned()),
                data_base64: None,
                media_type: media_type.to_owned(),
            },
            name,
            format_size(size),
        );
        chip.thumb = Some(Thumb::Path(path.to_path_buf()));
        let limit = u64::try_from(IMAGE_MAX_BYTES).unwrap_or(u64::MAX);
        if size > limit {
            return chip.refused(format!(
                "{} is too big: images go up to {}",
                format_size(size),
                format_size(limit)
            ));
        }
        return chip;
    }
    let chip = ContextChip::new(
        Attachment::File {
            path: text.to_owned(),
            range: None,
            picked: true,
        },
        name,
        format_size(size),
    );
    if is_other_image(path) {
        return chip.refused("only PNG, JPEG, GIF and WebP images can be attached");
    }
    match looks_like_text(path) {
        Ok(true) => chip,
        Ok(false) => chip.refused("this is not a text file"),
        Err(error) => chip.refused(format!("cannot read it: {error}")),
    }
}

/// A file chip that is refused from the start.
fn placeholder_file(path: &Path, name: &str, reason: &str) -> ContextChip {
    ContextChip::new(
        Attachment::File {
            path: path.to_string_lossy().into_owned(),
            range: None,
            picked: true,
        },
        name.to_owned(),
        String::new(),
    )
    .refused(reason)
}

/// Whether the start of the file is UTF-8 text (no NUL byte; a character cut by the end of what
/// was read does not count against it).
fn looks_like_text(path: &Path) -> std::io::Result<bool> {
    let mut head = vec![0u8; SNIFF_BYTES];
    let mut filled = 0;
    let mut file = File::open(path)?;
    while filled < head.len() {
        let read = file.read(&mut head[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    head.truncate(filled);
    if head.contains(&0) {
        return Ok(false);
    }
    Ok(match std::str::from_utf8(&head) {
        Ok(_) => true,
        // A multi-byte character cut by the end of the sample is not an error.
        Err(error) => error.error_len().is_none() && filled == SNIFF_BYTES,
    })
}

/// The chip of an image on the clipboard: the bytes go inline, so they must fit one message
/// frame ([`IMAGE_INLINE_MAX_BYTES`]).
pub fn chip_for_image_data(image: &gpui::Image) -> ContextChip {
    use gpui::ImageFormat;
    let media_type = match image.format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::Webp => "image/webp",
        other => {
            return ContextChip::new(
                Attachment::Image {
                    path: None,
                    data_base64: Some(String::new()),
                    media_type: "image/png".to_owned(),
                },
                "pasted image".to_owned(),
                String::new(),
            )
            .refused(format!(
                "a {} image cannot be attached: only PNG, JPEG, GIF and WebP",
                other.extension().to_ascii_uppercase()
            ));
        }
    };
    let size = image.bytes.len();
    let mut chip = if size > IMAGE_INLINE_MAX_BYTES {
        // Never encoded: the chip is refused and nothing is sent.
        ContextChip::new(
            Attachment::Image {
                path: None,
                data_base64: Some(String::new()),
                media_type: media_type.to_owned(),
            },
            "pasted image".to_owned(),
            format_size(size as u64),
        )
        .refused(format!(
            "{} is too big to paste: save it to a file and drop that (up to {})",
            format_size(size as u64),
            format_size(IMAGE_MAX_BYTES as u64)
        ))
    } else {
        ContextChip::new(
            Attachment::Image {
                path: None,
                data_base64: Some(base64::engine::general_purpose::STANDARD.encode(&image.bytes)),
                media_type: media_type.to_owned(),
            },
            "pasted image".to_owned(),
            format_size(size as u64),
        )
    };
    chip.thumb = Some(Thumb::Image(Arc::new(image.clone())));
    chip
}

/// The chip of a long paste: `Pasted · 312 lines`, open on a click.
pub fn chip_for_text(text: String) -> ContextChip {
    let lines = text.lines().count();
    let noun = if lines == 1 { "line" } else { "lines" };
    let too_long = text.len() > PASTED_TEXT_MAX_BYTES;
    let size = text.len();
    let chip = ContextChip::new(
        Attachment::PastedText { text },
        "Pasted".to_owned(),
        format!("{lines} {noun}"),
    );
    if too_long {
        chip.refused(format!(
            "{} is too long to paste: at most {}; save it to a file and attach that",
            format_size(size as u64),
            format_size(PASTED_TEXT_MAX_BYTES as u64)
        ))
    } else {
        chip
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::assistant::AttachmentKind;

    #[test]
    fn long_pastes_are_by_characters_or_lines() {
        assert!(!is_long_paste(&"x".repeat(LONG_PASTE_CHARS)));
        assert!(is_long_paste(&"x".repeat(LONG_PASTE_CHARS + 1)));
        assert!(!is_long_paste(&"a\n".repeat(LONG_PASTE_LINES)));
        assert!(is_long_paste(&"a\n".repeat(LONG_PASTE_LINES + 1)));
        assert!(!is_long_paste("one short line"));
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(format_size(12), "12 B");
        assert_eq!(format_size(2048), "2 KiB");
        assert_eq!(format_size(1536), "2 KiB");
        assert_eq!(format_size(3 * 1024 * 1024 / 2), "1.5 MiB");
    }

    #[test]
    fn a_folded_paste_says_how_many_lines_and_holds_its_text() {
        let text = "a\nb\nc".to_owned();
        let chip = chip_for_text(text.clone());
        assert_eq!(chip.label(), "Pasted · 3 lines");
        assert_eq!(chip.attachment, Attachment::PastedText { text });
        assert!(chip.refused.is_none());
        let huge = chip_for_text("x".repeat(PASTED_TEXT_MAX_BYTES + 1));
        assert!(huge.refused.as_deref().unwrap().contains("too long"));
    }

    #[test]
    fn paths_become_image_or_file_chips_and_say_what_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.PNG");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        let chip = chip_for_path(&png);
        assert_eq!(chip.kind(), AttachmentKind::Image);
        assert!(chip.refused.is_none());
        assert!(matches!(chip.thumb, Some(Thumb::Path(_))));
        assert_eq!(chip.label(), "image · shot.PNG · 8 B");

        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "héllo\n").unwrap();
        let chip = chip_for_path(&notes);
        assert_eq!(chip.kind(), AttachmentKind::File);
        assert!(chip.refused.is_none());
        assert_eq!(
            chip.attachment,
            Attachment::File {
                path: notes.display().to_string(),
                range: None,
                picked: true
            }
        );

        let binary = dir.path().join("blob.bin");
        std::fs::write(&binary, [0u8, 1, 2]).unwrap();
        assert!(
            chip_for_path(&binary)
                .refused
                .unwrap()
                .contains("not a text")
        );
        let svg = dir.path().join("logo.svg");
        std::fs::write(&svg, "<svg/>").unwrap();
        assert!(chip_for_path(&svg).refused.unwrap().contains("PNG, JPEG"));
        assert!(
            chip_for_path(dir.path())
                .refused
                .unwrap()
                .contains("folders")
        );
        assert!(
            chip_for_path(&dir.path().join("missing"))
                .refused
                .unwrap()
                .contains("cannot read")
        );
    }

    #[test]
    fn an_image_over_the_limit_is_refused_on_its_chip() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.jpg");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(IMAGE_MAX_BYTES as u64 + 1).unwrap();
        let chip = chip_for_path(&big);
        assert!(chip.refused.unwrap().contains("too big"));
    }

    #[test]
    fn clipboard_images_go_inline_when_they_fit() {
        let png = gpui::Image::from_bytes(gpui::ImageFormat::Png, b"\x89PNG\r\n\x1a\nxyz".to_vec());
        let chip = chip_for_image_data(&png);
        assert!(chip.refused.is_none());
        assert_eq!(
            chip.attachment,
            Attachment::Image {
                path: None,
                data_base64: Some("iVBORw0KGgp4eXo=".to_owned()),
                media_type: "image/png".to_owned()
            }
        );
        let tiff = gpui::Image::from_bytes(gpui::ImageFormat::Tiff, vec![1, 2, 3]);
        assert!(chip_for_image_data(&tiff).refused.unwrap().contains("TIFF"));
        let huge =
            gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![0; IMAGE_INLINE_MAX_BYTES + 1]);
        let chip = chip_for_image_data(&huge);
        assert!(chip.refused.unwrap().contains("too big to paste"));
    }
}
