//! Clipboard text → PTY bytes (GPUI-free).

const BRACKETED_START: &str = "\x1b[200~";
const BRACKETED_END: &str = "\x1b[201~";

/// Bytes to send for pasted `text`.
///
/// With bracketed paste on, the text is wrapped in `ESC[200~ … ESC[201~` and every embedded
/// `ESC[201~` is removed first (repeatedly, so removal cannot assemble a new one): clipboard
/// content must not be able to end the paste early and run as typed input. Without bracketed
/// paste, line breaks become carriage returns like typed Enter.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut clean = text.to_owned();
        while clean.contains(BRACKETED_END) {
            clean = clean.replace(BRACKETED_END, "");
        }
        let mut out = Vec::with_capacity(clean.len() + 12);
        out.extend_from_slice(BRACKETED_START.as_bytes());
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(BRACKETED_END.as_bytes());
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_wraps_the_text() {
        assert_eq!(paste_bytes("ls\nls", true), b"\x1b[200~ls\nls\x1b[201~");
    }

    #[test]
    fn bracketed_paste_strips_embedded_terminators() {
        assert_eq!(
            paste_bytes("a\x1b[201~rm -rf /\n", true),
            b"\x1b[200~arm -rf /\n\x1b[201~"
        );
        // Removing one terminator must not assemble another.
        assert_eq!(
            paste_bytes("\x1b[2\x1b[201~01~x", true),
            b"\x1b[200~x\x1b[201~"
        );
    }

    #[test]
    fn plain_paste_turns_line_breaks_into_returns() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("héllo", false), "héllo".as_bytes());
    }

    #[test]
    fn plain_paste_leaves_terminators_alone() {
        assert_eq!(paste_bytes("\x1b[201~", false), b"\x1b[201~");
    }
}
