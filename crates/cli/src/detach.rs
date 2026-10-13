//! Detach key handling for `attach`: a tmux-style prefix, then a command key.

/// Ctrl+\ (FS).
pub const PREFIX: u8 = 0x1c;
/// Pressed after the prefix to detach.
pub const DETACH_KEY: u8 = b'd';

/// Filters host input on its way to the terminal.
///
/// `Ctrl+\` then `d` detaches. `Ctrl+\` twice sends one literal `Ctrl+\`. `Ctrl+\` followed by
/// any other byte forwards both bytes unchanged. A prefix at the end of a read is held back
/// until the next byte decides what it means, so sequences may be split across reads.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DetachDetector {
    prefix_pending: bool,
}

impl DetachDetector {
    pub const fn new() -> Self {
        Self {
            prefix_pending: false,
        }
    }

    /// Whether a prefix is held back waiting for the next byte.
    pub const fn prefix_pending(&self) -> bool {
        self.prefix_pending
    }

    /// Appends the bytes of `input` that belong to the terminal to `forward`. Returns `true`
    /// when the detach sequence was completed; the rest of `input` is then dropped.
    pub fn feed(&mut self, input: &[u8], forward: &mut Vec<u8>) -> bool {
        for &byte in input {
            if self.prefix_pending {
                self.prefix_pending = false;
                match byte {
                    DETACH_KEY => return true,
                    PREFIX => forward.push(PREFIX),
                    other => {
                        forward.push(PREFIX);
                        forward.push(other);
                    }
                }
            } else if byte == PREFIX {
                self.prefix_pending = true;
            } else {
                forward.push(byte);
            }
        }
        false
    }
}
