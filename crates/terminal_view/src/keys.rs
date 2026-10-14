//! Keyboard encoding (GPUI-free): legacy xterm sequences and the kitty keyboard protocol.
//!
//! Printable keys without Ctrl/Meta return `None` from [`encode_key`]: they travel through the
//! platform's text input (IME) path instead, so composition, dead keys and non-Latin layouts
//! work. Everything else is encoded here.

/// Kitty flag 1: disambiguate escape codes.
pub const KITTY_DISAMBIGUATE: u8 = 1;
/// Kitty flag 8: report all keys as escape codes.
pub const KITTY_REPORT_ALL: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    /// Option on macOS.
    pub alt: bool,
    /// Command on macOS.
    pub super_key: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
        super_key: false,
    };

    pub fn any(self) -> bool {
        self.shift || self.ctrl || self.alt || self.super_key
    }

    /// The xterm modifier parameter: 1 + shift 1 + alt 2 + ctrl 4 + super 8.
    fn param(self) -> u8 {
        1 + u8::from(self.shift)
            + 2 * u8::from(self.alt)
            + 4 * u8::from(self.ctrl)
            + 8 * u8::from(self.super_key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character key, as the unshifted key (`a` for Shift+A on a US layout).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// `F(1)` … `F(20)`.
    F(u8),
    /// Numeric keypad key (`0`–`9`, `+ - * / . , =`). GPUI does not report keypad keys apart
    /// from the main block, so the view never produces it; the encoding exists for owners that
    /// can tell.
    Keypad(char),
    KeypadEnter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    pub mods: Mods,
}

/// Terminal state that changes how keys are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyOptions {
    pub app_cursor: bool,
    pub app_keypad: bool,
    /// Kitty progressive-enhancement flags currently set by the application.
    pub kitty_flags: u8,
    /// Option acts as Meta: Alt + character sends an ESC prefix instead of composing.
    pub option_as_meta: bool,
}

/// Bytes for `input`, or `None` when the key should go through text input (or is not ours).
pub fn encode_key(input: &KeyInput, opts: KeyOptions) -> Option<Vec<u8>> {
    let mods = input.mods;
    let kitty = opts.kitty_flags & (KITTY_DISAMBIGUATE | KITTY_REPORT_ALL) != 0;
    let report_all = opts.kitty_flags & KITTY_REPORT_ALL != 0;
    match input.key {
        Key::Char(c) => encode_char(c, mods, opts, kitty, report_all),
        Key::Enter | Key::KeypadEnter => {
            if kitty && (report_all || mods.any()) {
                return Some(csi_u(13, mods));
            }
            if opts.app_keypad && input.key == Key::KeypadEnter && !mods.any() {
                return Some(b"\x1bOM".to_vec());
            }
            Some(meta(mods.alt, b"\r"))
        }
        Key::Tab => {
            if kitty && (report_all || mods.any()) {
                return Some(csi_u(9, mods));
            }
            if mods.shift {
                return Some(meta(mods.alt, b"\x1b[Z"));
            }
            Some(meta(mods.alt, b"\t"))
        }
        Key::Backspace => {
            if kitty && (report_all || mods.any()) {
                return Some(csi_u(127, mods));
            }
            let byte: &[u8] = if mods.ctrl { b"\x08" } else { b"\x7f" };
            Some(meta(mods.alt, byte))
        }
        Key::Escape => {
            if kitty {
                return Some(csi_u(27, mods));
            }
            Some(meta(mods.alt, b"\x1b"))
        }
        Key::Up => Some(cursor_key(b'A', mods, opts, report_all)),
        Key::Down => Some(cursor_key(b'B', mods, opts, report_all)),
        Key::Right => Some(cursor_key(b'C', mods, opts, report_all)),
        Key::Left => Some(cursor_key(b'D', mods, opts, report_all)),
        Key::Home => Some(cursor_key(b'H', mods, opts, report_all)),
        Key::End => Some(cursor_key(b'F', mods, opts, report_all)),
        Key::Insert => Some(tilde(2, mods)),
        Key::Delete => Some(tilde(3, mods)),
        Key::PageUp => Some(tilde(5, mods)),
        Key::PageDown => Some(tilde(6, mods)),
        Key::F(n) => function_key(n, mods, kitty, report_all),
        Key::Keypad(c) => keypad(c, mods, opts),
    }
}

fn encode_char(
    c: char,
    mods: Mods,
    opts: KeyOptions,
    kitty: bool,
    report_all: bool,
) -> Option<Vec<u8>> {
    if mods.super_key {
        return None;
    }
    // Option without Meta composes characters: text input handles it.
    let alt = mods.alt && opts.option_as_meta;
    if mods.alt && !alt && !mods.ctrl {
        return None;
    }
    let key = ascii_equivalent(c);
    if kitty {
        if !(report_all || mods.ctrl || alt) {
            return None;
        }
        let mods = Mods { alt, ..mods };
        let lower = key.to_lowercase().next().unwrap_or(key);
        return Some(csi_u(u32::from(lower), mods));
    }
    if mods.ctrl {
        let byte = ctrl_byte(key)?;
        return Some(meta(alt, &[byte]));
    }
    if alt {
        let shown = if mods.shift {
            key.to_uppercase().next().unwrap_or(key)
        } else {
            key
        };
        let mut buf = [0u8; 4];
        return Some(meta(true, shown.encode_utf8(&mut buf).as_bytes()));
    }
    None
}

/// Prefixes ESC when `alt` is set.
fn meta(alt: bool, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' | 'A'..='Z' => c as u8 & 0x1f,
        ' ' | '@' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '`' => 0x1e,
        '_' | '7' | '/' | '-' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// `CSI code [; mods] u`.
fn csi_u(code: u32, mods: Mods) -> Vec<u8> {
    let m = mods.param();
    if m == 1 {
        format!("\x1b[{code}u").into_bytes()
    } else {
        format!("\x1b[{code};{m}u").into_bytes()
    }
}

/// Arrows, Home, End: `CSI final`, `SS3 final` in application cursor mode, `CSI 1;m final` with
/// modifiers.
fn cursor_key(final_byte: u8, mods: Mods, opts: KeyOptions, report_all: bool) -> Vec<u8> {
    let f = char::from(final_byte);
    if mods.any() {
        format!("\x1b[1;{}{f}", mods.param()).into_bytes()
    } else if opts.app_cursor && !report_all {
        format!("\x1bO{f}").into_bytes()
    } else {
        format!("\x1b[{f}").into_bytes()
    }
}

/// `CSI n ~` / `CSI n ; m ~`.
fn tilde(n: u8, mods: Mods) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[{n};{}~", mods.param()).into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}

fn function_key(n: u8, mods: Mods, kitty: bool, report_all: bool) -> Option<Vec<u8>> {
    match n {
        1..=4 => {
            let f = char::from(b'P' + (n - 1));
            // Kitty moves F3 to `CSI 13 ~`: `CSI R` collides with cursor position reports.
            if kitty && n == 3 {
                return Some(tilde(13, mods));
            }
            Some(if mods.any() {
                format!("\x1b[1;{}{f}", mods.param()).into_bytes()
            } else if report_all {
                format!("\x1b[{f}").into_bytes()
            } else {
                format!("\x1bO{f}").into_bytes()
            })
        }
        5..=20 => {
            const CODES: [u8; 16] = [
                15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34,
            ];
            Some(tilde(CODES[usize::from(n - 5)], mods))
        }
        _ => None,
    }
}

fn keypad(c: char, mods: Mods, opts: KeyOptions) -> Option<Vec<u8>> {
    if !opts.app_keypad || mods.any() {
        let mut buf = [0u8; 4];
        return Some(c.encode_utf8(&mut buf).as_bytes().to_vec());
    }
    let final_byte = match c {
        '0'..='9' => b'p' + (c as u8 - b'0'),
        '*' => b'j',
        '+' => b'k',
        ',' => b'l',
        '-' => b'm',
        '.' => b'n',
        '/' => b'o',
        '=' => b'X',
        _ => return None,
    };
    Some(vec![0x1b, b'O', final_byte])
}

/// The Latin key at the same physical position for the Russian (ЙЦУКЕН) layout. GPUI reports
/// the layout's own character for plain and Ctrl keystrokes, but control codes and kitty key
/// codes refer to the key, so Ctrl-С on a Russian layout must act like Ctrl-C.
pub(crate) fn ascii_equivalent(c: char) -> char {
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
    if c.is_ascii() {
        return c;
    }
    let lower = c.to_lowercase().next().unwrap_or(c);
    match RU.chars().position(|r| r == lower) {
        Some(i) => EN.chars().nth(i).unwrap_or(c),
        None => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: KeyOptions = KeyOptions {
        app_cursor: false,
        app_keypad: false,
        kitty_flags: 0,
        option_as_meta: true,
    };

    fn key(key: Key) -> KeyInput {
        KeyInput {
            key,
            mods: Mods::NONE,
        }
    }

    fn with(key: Key, shift: bool, ctrl: bool, alt: bool) -> KeyInput {
        KeyInput {
            key,
            mods: Mods {
                shift,
                ctrl,
                alt,
                super_key: false,
            },
        }
    }

    fn enc(input: &KeyInput, opts: KeyOptions) -> Option<String> {
        encode_key(input, opts).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    fn kitty(flags: u8) -> KeyOptions {
        KeyOptions {
            kitty_flags: flags,
            ..LEGACY
        }
    }

    #[test]
    fn arrows_follow_the_cursor_mode() {
        for (k, f) in [
            (Key::Up, 'A'),
            (Key::Down, 'B'),
            (Key::Right, 'C'),
            (Key::Left, 'D'),
        ] {
            assert_eq!(enc(&key(k), LEGACY).unwrap(), format!("\x1b[{f}"));
            let app = KeyOptions {
                app_cursor: true,
                ..LEGACY
            };
            assert_eq!(enc(&key(k), app).unwrap(), format!("\x1bO{f}"));
            // Modified arrows always use the CSI form.
            assert_eq!(
                enc(&with(k, false, true, false), app).unwrap(),
                format!("\x1b[1;5{f}")
            );
        }
        assert_eq!(
            enc(&with(Key::Up, true, false, false), LEGACY).unwrap(),
            "\x1b[1;2A"
        );
        assert_eq!(
            enc(&with(Key::Left, false, false, true), LEGACY).unwrap(),
            "\x1b[1;3D"
        );
        assert_eq!(
            enc(&with(Key::Right, true, true, true), LEGACY).unwrap(),
            "\x1b[1;8C"
        );
        assert_eq!(enc(&key(Key::Home), LEGACY).unwrap(), "\x1b[H");
        assert_eq!(enc(&key(Key::End), LEGACY).unwrap(), "\x1b[F");
        let app = KeyOptions {
            app_cursor: true,
            ..LEGACY
        };
        assert_eq!(enc(&key(Key::Home), app).unwrap(), "\x1bOH");
        assert_eq!(
            enc(&with(Key::End, false, true, false), app).unwrap(),
            "\x1b[1;5F"
        );
    }

    #[test]
    fn editing_keys() {
        assert_eq!(enc(&key(Key::Insert), LEGACY).unwrap(), "\x1b[2~");
        assert_eq!(enc(&key(Key::Delete), LEGACY).unwrap(), "\x1b[3~");
        assert_eq!(enc(&key(Key::PageUp), LEGACY).unwrap(), "\x1b[5~");
        assert_eq!(enc(&key(Key::PageDown), LEGACY).unwrap(), "\x1b[6~");
        assert_eq!(
            enc(&with(Key::Delete, false, false, true), LEGACY).unwrap(),
            "\x1b[3;3~"
        );
        assert_eq!(
            enc(&with(Key::PageUp, true, false, false), LEGACY).unwrap(),
            "\x1b[5;2~"
        );
    }

    #[test]
    fn function_keys() {
        let expected = [
            (1, "\x1bOP"),
            (2, "\x1bOQ"),
            (3, "\x1bOR"),
            (4, "\x1bOS"),
            (5, "\x1b[15~"),
            (6, "\x1b[17~"),
            (7, "\x1b[18~"),
            (8, "\x1b[19~"),
            (9, "\x1b[20~"),
            (10, "\x1b[21~"),
            (11, "\x1b[23~"),
            (12, "\x1b[24~"),
            (13, "\x1b[25~"),
            (20, "\x1b[34~"),
        ];
        for (n, seq) in expected {
            assert_eq!(enc(&key(Key::F(n)), LEGACY).unwrap(), seq, "F{n}");
        }
        assert_eq!(
            enc(&with(Key::F(1), true, false, false), LEGACY).unwrap(),
            "\x1b[1;2P"
        );
        assert_eq!(
            enc(&with(Key::F(4), false, true, false), LEGACY).unwrap(),
            "\x1b[1;5S"
        );
        assert_eq!(
            enc(&with(Key::F(5), false, true, false), LEGACY).unwrap(),
            "\x1b[15;5~"
        );
        assert_eq!(
            enc(&with(Key::F(12), true, false, true), LEGACY).unwrap(),
            "\x1b[24;4~"
        );
        assert_eq!(encode_key(&key(Key::F(21)), LEGACY), None);
    }

    #[test]
    fn control_characters() {
        let ctrl = |c| with(Key::Char(c), false, true, false);
        assert_eq!(encode_key(&ctrl('c'), LEGACY).unwrap(), [0x03]);
        assert_eq!(encode_key(&ctrl('a'), LEGACY).unwrap(), [0x01]);
        assert_eq!(encode_key(&ctrl('z'), LEGACY).unwrap(), [0x1a]);
        assert_eq!(encode_key(&ctrl(' '), LEGACY).unwrap(), [0x00]);
        assert_eq!(encode_key(&ctrl('['), LEGACY).unwrap(), [0x1b]);
        assert_eq!(encode_key(&ctrl('\\'), LEGACY).unwrap(), [0x1c]);
        assert_eq!(encode_key(&ctrl(']'), LEGACY).unwrap(), [0x1d]);
        assert_eq!(encode_key(&ctrl('/'), LEGACY).unwrap(), [0x1f]);
        assert_eq!(encode_key(&ctrl('8'), LEGACY).unwrap(), [0x7f]);
        assert_eq!(
            encode_key(&with(Key::Char('c'), true, true, false), LEGACY).unwrap(),
            [0x03]
        );
        assert_eq!(encode_key(&ctrl('.'), LEGACY), None);
    }

    #[test]
    fn control_characters_on_a_russian_layout() {
        // The key GPUI reports for the physical C key is `с` (Cyrillic).
        assert_eq!(
            encode_key(&with(Key::Char('с'), false, true, false), LEGACY).unwrap(),
            [0x03]
        );
        assert_eq!(ascii_equivalent('й'), 'q');
        assert_eq!(ascii_equivalent('Я'), 'z');
        assert_eq!(ascii_equivalent('x'), 'x');
        assert_eq!(ascii_equivalent('ß'), 'ß');
    }

    #[test]
    fn alt_prefixes_escape_when_option_is_meta() {
        assert_eq!(
            enc(&with(Key::Char('x'), false, false, true), LEGACY).unwrap(),
            "\x1bx"
        );
        assert_eq!(
            enc(&with(Key::Char('x'), true, false, true), LEGACY).unwrap(),
            "\x1bX"
        );
        assert_eq!(
            enc(&with(Key::Char(' '), false, false, true), LEGACY).unwrap(),
            "\x1b "
        );
        assert_eq!(
            encode_key(&with(Key::Char('a'), false, true, true), LEGACY).unwrap(),
            [0x1b, 0x01]
        );
        assert_eq!(
            enc(&with(Key::Enter, false, false, true), LEGACY).unwrap(),
            "\x1b\r"
        );
        assert_eq!(
            enc(&with(Key::Backspace, false, false, true), LEGACY).unwrap(),
            "\x1b\x7f"
        );
        assert_eq!(
            enc(&with(Key::Escape, false, false, true), LEGACY).unwrap(),
            "\x1b\x1b"
        );
    }

    #[test]
    fn option_without_meta_composes_text() {
        let no_meta = KeyOptions {
            option_as_meta: false,
            ..LEGACY
        };
        assert_eq!(
            encode_key(&with(Key::Char('a'), false, false, true), no_meta),
            None
        );
        // Ctrl still wins.
        assert_eq!(
            encode_key(&with(Key::Char('a'), false, true, true), no_meta).unwrap(),
            [0x01]
        );
    }

    #[test]
    fn plain_and_shifted_text_goes_through_the_input_method() {
        assert_eq!(encode_key(&key(Key::Char('a')), LEGACY), None);
        assert_eq!(
            encode_key(&with(Key::Char('a'), true, false, false), LEGACY),
            None
        );
        assert_eq!(encode_key(&key(Key::Char('я')), LEGACY), None);
        assert_eq!(encode_key(&key(Key::Char(' ')), LEGACY), None);
    }

    #[test]
    fn command_chords_are_not_encoded() {
        let cmd = KeyInput {
            key: Key::Char('c'),
            mods: Mods {
                super_key: true,
                ..Mods::NONE
            },
        };
        assert_eq!(encode_key(&cmd, LEGACY), None);
        assert_eq!(encode_key(&cmd, kitty(KITTY_REPORT_ALL)), None);
    }

    #[test]
    fn enter_tab_backspace_escape() {
        assert_eq!(enc(&key(Key::Enter), LEGACY).unwrap(), "\r");
        assert_eq!(
            enc(&with(Key::Enter, true, false, false), LEGACY).unwrap(),
            "\r"
        );
        assert_eq!(enc(&key(Key::Tab), LEGACY).unwrap(), "\t");
        assert_eq!(
            enc(&with(Key::Tab, true, false, false), LEGACY).unwrap(),
            "\x1b[Z"
        );
        assert_eq!(enc(&key(Key::Backspace), LEGACY).unwrap(), "\x7f");
        assert_eq!(
            enc(&with(Key::Backspace, false, true, false), LEGACY).unwrap(),
            "\x08"
        );
        assert_eq!(enc(&key(Key::Escape), LEGACY).unwrap(), "\x1b");
    }

    #[test]
    fn keypad_modes() {
        assert_eq!(enc(&key(Key::Keypad('5')), LEGACY).unwrap(), "5");
        let app = KeyOptions {
            app_keypad: true,
            ..LEGACY
        };
        assert_eq!(enc(&key(Key::Keypad('0')), app).unwrap(), "\x1bOp");
        assert_eq!(enc(&key(Key::Keypad('9')), app).unwrap(), "\x1bOy");
        assert_eq!(enc(&key(Key::Keypad('+')), app).unwrap(), "\x1bOk");
        assert_eq!(enc(&key(Key::Keypad('.')), app).unwrap(), "\x1bOn");
        assert_eq!(enc(&key(Key::KeypadEnter), app).unwrap(), "\x1bOM");
        assert_eq!(enc(&key(Key::KeypadEnter), LEGACY).unwrap(), "\r");
    }

    #[test]
    fn kitty_disambiguate_flag_1() {
        let k = kitty(KITTY_DISAMBIGUATE);
        assert_eq!(enc(&key(Key::Escape), k).unwrap(), "\x1b[27u");
        assert_eq!(
            enc(&with(Key::Escape, false, false, true), k).unwrap(),
            "\x1b[27;3u"
        );
        assert_eq!(
            enc(&with(Key::Char('c'), false, true, false), k).unwrap(),
            "\x1b[99;5u"
        );
        assert_eq!(
            enc(&with(Key::Char('a'), false, false, true), k).unwrap(),
            "\x1b[97;3u"
        );
        assert_eq!(
            enc(&with(Key::Char('a'), true, true, false), k).unwrap(),
            "\x1b[97;6u"
        );
        assert_eq!(
            enc(&with(Key::Char('i'), false, true, false), k).unwrap(),
            "\x1b[105;5u"
        );
        // Text keys keep using the text path.
        assert_eq!(encode_key(&key(Key::Char('a')), k), None);
        assert_eq!(
            encode_key(&with(Key::Char('A'), true, false, false), k),
            None
        );
        // Enter, Tab and Backspace stay legacy until modified.
        assert_eq!(enc(&key(Key::Enter), k).unwrap(), "\r");
        assert_eq!(enc(&key(Key::Tab), k).unwrap(), "\t");
        assert_eq!(enc(&key(Key::Backspace), k).unwrap(), "\x7f");
        assert_eq!(
            enc(&with(Key::Enter, true, false, false), k).unwrap(),
            "\x1b[13;2u"
        );
        assert_eq!(
            enc(&with(Key::Tab, true, false, false), k).unwrap(),
            "\x1b[9;2u"
        );
        assert_eq!(
            enc(&with(Key::Backspace, false, true, false), k).unwrap(),
            "\x1b[127;5u"
        );
        // Functional keys keep their legacy forms, except F3.
        assert_eq!(enc(&key(Key::Up), k).unwrap(), "\x1b[A");
        assert_eq!(
            enc(&with(Key::Up, false, true, false), k).unwrap(),
            "\x1b[1;5A"
        );
        assert_eq!(enc(&key(Key::F(1)), k).unwrap(), "\x1bOP");
        assert_eq!(enc(&key(Key::F(3)), k).unwrap(), "\x1b[13~");
        assert_eq!(
            enc(&with(Key::F(3), false, true, false), k).unwrap(),
            "\x1b[13;5~"
        );
        assert_eq!(enc(&key(Key::F(5)), k).unwrap(), "\x1b[15~");
    }

    #[test]
    fn kitty_report_all_flag_8() {
        let k = kitty(KITTY_REPORT_ALL);
        assert_eq!(enc(&key(Key::Char('a')), k).unwrap(), "\x1b[97u");
        assert_eq!(
            enc(&with(Key::Char('a'), true, false, false), k).unwrap(),
            "\x1b[97;2u"
        );
        assert_eq!(
            enc(&with(Key::Char('A'), true, false, false), k).unwrap(),
            "\x1b[97;2u"
        );
        assert_eq!(enc(&key(Key::Char(' ')), k).unwrap(), "\x1b[32u");
        assert_eq!(enc(&key(Key::Enter), k).unwrap(), "\x1b[13u");
        assert_eq!(enc(&key(Key::Tab), k).unwrap(), "\x1b[9u");
        assert_eq!(enc(&key(Key::Backspace), k).unwrap(), "\x1b[127u");
        assert_eq!(enc(&key(Key::Escape), k).unwrap(), "\x1b[27u");
        assert_eq!(
            enc(&with(Key::Char('c'), false, true, false), k).unwrap(),
            "\x1b[99;5u"
        );
        // Application cursor mode does not change the report form.
        let app = KeyOptions {
            app_cursor: true,
            ..k
        };
        assert_eq!(enc(&key(Key::Up), app).unwrap(), "\x1b[A");
        assert_eq!(enc(&key(Key::F(1)), k).unwrap(), "\x1b[P");
        // Option composing text still goes through the input method.
        let no_meta = KeyOptions {
            option_as_meta: false,
            ..k
        };
        assert_eq!(
            encode_key(&with(Key::Char('a'), false, false, true), no_meta),
            None
        );
    }

    #[test]
    fn other_kitty_flags_encode_as_if_unset() {
        for flags in [2, 4, 16, 2 | 4 | 16] {
            let k = kitty(flags);
            assert_eq!(enc(&key(Key::Escape), k).unwrap(), "\x1b", "flags {flags}");
            assert_eq!(encode_key(&key(Key::Char('a')), k), None);
            assert_eq!(
                encode_key(&with(Key::Char('c'), false, true, false), k).unwrap(),
                [3]
            );
            assert_eq!(enc(&key(Key::F(3)), k).unwrap(), "\x1bOR");
        }
        let both = kitty(KITTY_DISAMBIGUATE | 2);
        assert_eq!(enc(&key(Key::Escape), both).unwrap(), "\x1b[27u");
    }
}
