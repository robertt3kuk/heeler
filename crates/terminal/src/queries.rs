//! Terminal queries the VT engine does not answer by itself, and the colour tables behind the
//! colour replies.
//!
//! The engine already turns OSC 4/10/11/12 `?`, CSI 14 t and CSI 18 t into events; it ignores
//! CSI 16 t (cell size in pixels). [`CellSizeScanner`] finds that one sequence in the raw byte
//! stream, with state carried across reads, so [`crate::Emulator`] can answer it in stream order.

/// Longest sequence of digits in one CSI parameter that is still tracked exactly.
const PARAM_SATURATION: u32 = 1 << 20;

/// Colour (red, green, blue) as reported in `rgb:` replies.
pub(crate) type Rgb = (u8, u8, u8);

/// ANSI colours 0–15 as xterm reports them.
const ANSI: [Rgb; 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// Levels of the 6×6×6 colour cube (indices 16–231).
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The standard xterm 256-colour palette.
pub(crate) fn xterm_color(index: u8) -> Rgb {
    match index {
        0..=15 => ANSI[usize::from(index)],
        16..=231 => {
            let cube = usize::from(index - 16);
            (CUBE[cube / 36], CUBE[(cube / 6) % 6], CUBE[cube % 6])
        }
        232..=255 => {
            let gray = 8 + 10 * (index - 232);
            (gray, gray, gray)
        }
    }
}

/// True for an OSC payload that queries a colour: `4;N;?…`, `10;?…`, `11;?…`, `12;?…`.
pub(crate) fn is_color_query(payload: &[u8]) -> bool {
    let mut parts = payload.split(|&b| b == b';');
    matches!(parts.next(), Some(b"4" | b"10" | b"11" | b"12")) && parts.any(|part| part == b"?")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    /// Inside `ESC [`: `value` is the number read so far; `plain` stays true while the
    /// sequence consists of digits only (no `;`, `:`, private markers or intermediates).
    Csi {
        value: u32,
        plain: bool,
    },
}

/// Finds `CSI 1 6 t` in a byte stream.
#[derive(Debug)]
pub(crate) struct CellSizeScanner {
    state: State,
}

impl CellSizeScanner {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
        }
    }

    /// Appends to `ends` the offset just past every `CSI 16 t` completed in `bytes`.
    pub(crate) fn feed(&mut self, bytes: &[u8], ends: &mut Vec<usize>) {
        for (offset, &byte) in bytes.iter().enumerate() {
            self.state = match (self.state, byte) {
                // CAN and SUB abort a sequence; ESC restarts one.
                (_, 0x18 | 0x1a) => State::Ground,
                (_, 0x1b) => State::Escape,
                (State::Ground, _) => State::Ground,
                (State::Escape, b'[') => State::Csi {
                    value: 0,
                    plain: true,
                },
                (State::Escape, _) => State::Ground,
                (State::Csi { value, plain }, b'0'..=b'9') => State::Csi {
                    value: value
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'))
                        .min(PARAM_SATURATION),
                    plain,
                },
                (State::Csi { value, .. }, 0x20..=0x3f) => State::Csi {
                    value,
                    plain: false,
                },
                (State::Csi { value, plain }, b't') => {
                    if plain && value == 16 {
                        ends.push(offset + 1);
                    }
                    State::Ground
                }
                (State::Csi { .. }, 0x40..=0x7e) => State::Ground,
                // Other C0 controls and DEL are executed inside a sequence without ending it.
                (state, _) => state,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<Vec<usize>> {
        let mut scanner = CellSizeScanner::new();
        chunks
            .iter()
            .map(|chunk| {
                let mut ends = Vec::new();
                scanner.feed(chunk, &mut ends);
                ends
            })
            .collect()
    }

    #[test]
    fn finds_the_query_in_one_chunk_and_across_chunks() {
        assert_eq!(scan(&[b"ab\x1b[16tcd\x1b[16t"]), vec![vec![7, 14]]);
        assert_eq!(
            scan(&[b"\x1b[", b"1", b"6", b"t"]),
            vec![vec![], vec![], vec![], vec![1]]
        );
    }

    #[test]
    fn ignores_other_sequences() {
        for stream in [
            &b"\x1b[18t"[..],
            b"\x1b[?16t",
            b"\x1b[16;1t",
            b"\x1b[116t",
            b"\x1b[16 t",
            b"\x1b[16m",
            b"\x1b[16\x18t",
            b"\x1b]16t",
            b"[16t",
        ] {
            assert_eq!(scan(&[stream]), vec![Vec::<usize>::new()], "{stream:?}");
        }
    }

    #[test]
    fn escape_restarts_a_sequence() {
        assert_eq!(scan(&[b"\x1b[1\x1b[16t"]), vec![vec![8]]);
    }

    #[test]
    fn color_queries_are_recognized() {
        for query in [&b"4;1;?"[..], b"10;?", b"11;?", b"12;?", b"4;1;?;2;?"] {
            assert!(is_color_query(query), "{query:?}");
        }
        for other in [
            &b"4;1;rgb:ff/00/00"[..],
            b"10;#fff",
            b"104",
            b"8;;http://x/?",
            b"0;?",
            b"11",
        ] {
            assert!(!is_color_query(other), "{other:?}");
        }
    }

    #[test]
    fn xterm_palette_corners() {
        assert_eq!(xterm_color(1), (205, 0, 0));
        assert_eq!(xterm_color(15), (255, 255, 255));
        assert_eq!(xterm_color(16), (0, 0, 0));
        assert_eq!(xterm_color(21), (0, 0, 255));
        assert_eq!(xterm_color(196), (255, 0, 0));
        assert_eq!(xterm_color(231), (255, 255, 255));
        assert_eq!(xterm_color(232), (8, 8, 8));
        assert_eq!(xterm_color(255), (238, 238, 238));
    }
}
