//! Mouse reporting (GPUI-free): SGR (1006) and legacy X10-style encodings, plus the
//! alternate-scroll arrow keys.

use heeler_protocol::render::MouseMode;

use crate::keys::Mods;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    fn code(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press(Button),
    Release(Button),
    /// Pointer motion; the button is the one held, if any.
    Motion(Option<Button>),
    Wheel(WheelDirection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub action: MouseAction,
    /// Only shift, ctrl and alt are reported.
    pub mods: Mods,
    /// Zero-based cell.
    pub col: u16,
    pub row: u16,
}

/// Largest 1-based coordinate the legacy encoding can express.
const LEGACY_MAX: u32 = 223;

/// Bytes to send for `event`, or `None` when the mode does not report it (or the legacy encoding
/// cannot express the position).
pub fn encode_mouse(event: &MouseEvent, mode: MouseMode, sgr: bool) -> Option<Vec<u8>> {
    let reported = !matches!(
        (mode, event.action),
        (MouseMode::None, _)
            | (MouseMode::Click, MouseAction::Motion(_))
            | (MouseMode::Drag, MouseAction::Motion(None))
    );
    if !reported {
        return None;
    }
    let mut code = match event.action {
        MouseAction::Press(b) | MouseAction::Release(b) => b.code(),
        MouseAction::Motion(Some(b)) => b.code() + 32,
        MouseAction::Motion(None) => 3 + 32,
        MouseAction::Wheel(WheelDirection::Up) => 64,
        MouseAction::Wheel(WheelDirection::Down) => 65,
        MouseAction::Wheel(WheelDirection::Left) => 66,
        MouseAction::Wheel(WheelDirection::Right) => 67,
    };
    if event.mods.shift {
        code += 4;
    }
    if event.mods.alt {
        code += 8;
    }
    if event.mods.ctrl {
        code += 16;
    }
    let x = u32::from(event.col) + 1;
    let y = u32::from(event.row) + 1;
    let release = matches!(event.action, MouseAction::Release(_));
    if sgr {
        let last = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{last}").into_bytes());
    }
    if x > LEGACY_MAX || y > LEGACY_MAX {
        return None;
    }
    if release {
        // Legacy encoding cannot say which button was released.
        code = (code & !3) | 3;
    }
    // Every operand is at most 255 after the bounds check.
    let byte = |v: u32| u8::try_from(v + 32).ok();
    Some(vec![0x1b, b'[', b'M', byte(code)?, byte(x)?, byte(y)?])
}

/// Arrow keys sent for wheel scrolling on the alternate screen (`alternate_scroll`).
pub fn alternate_scroll(up: bool, lines: usize, app_cursor: bool) -> Vec<u8> {
    let key: &[u8] = match (up, app_cursor) {
        (true, false) => b"\x1b[A",
        (false, false) => b"\x1b[B",
        (true, true) => b"\x1bOA",
        (false, true) => b"\x1bOB",
    };
    key.repeat(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(action: MouseAction, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            action,
            mods: Mods::NONE,
            col,
            row,
        }
    }

    fn enc(e: &MouseEvent, mode: MouseMode, sgr: bool) -> Option<String> {
        encode_mouse(e, mode, sgr).map(|b| b.iter().map(|&c| char::from(c)).collect())
    }

    #[test]
    fn sgr_press_and_release() {
        let press = ev(MouseAction::Press(Button::Left), 0, 0);
        assert_eq!(enc(&press, MouseMode::Click, true).unwrap(), "\x1b[<0;1;1M");
        let release = ev(MouseAction::Release(Button::Left), 9, 4);
        assert_eq!(
            enc(&release, MouseMode::Click, true).unwrap(),
            "\x1b[<0;10;5m"
        );
        let right = ev(MouseAction::Press(Button::Right), 299, 99);
        assert_eq!(
            enc(&right, MouseMode::Click, true).unwrap(),
            "\x1b[<2;300;100M"
        );
        let middle = ev(MouseAction::Release(Button::Middle), 1, 1);
        assert_eq!(
            enc(&middle, MouseMode::Click, true).unwrap(),
            "\x1b[<1;2;2m"
        );
    }

    #[test]
    fn sgr_modifiers_add_bits() {
        let mut e = ev(MouseAction::Press(Button::Left), 2, 3);
        e.mods = Mods {
            shift: true,
            alt: true,
            ctrl: true,
            super_key: true,
        };
        assert_eq!(enc(&e, MouseMode::Click, true).unwrap(), "\x1b[<28;3;4M");
    }

    #[test]
    fn sgr_drag_and_motion() {
        let drag = ev(MouseAction::Motion(Some(Button::Left)), 5, 6);
        assert_eq!(enc(&drag, MouseMode::Drag, true).unwrap(), "\x1b[<32;6;7M");
        assert_eq!(
            enc(&drag, MouseMode::Motion, true).unwrap(),
            "\x1b[<32;6;7M"
        );
        let hover = ev(MouseAction::Motion(None), 5, 6);
        assert_eq!(
            enc(&hover, MouseMode::Motion, true).unwrap(),
            "\x1b[<35;6;7M"
        );
        assert_eq!(enc(&hover, MouseMode::Drag, true), None);
        assert_eq!(enc(&drag, MouseMode::Click, true), None);
    }

    #[test]
    fn sgr_wheel() {
        let up = ev(MouseAction::Wheel(WheelDirection::Up), 1, 2);
        assert_eq!(enc(&up, MouseMode::Click, true).unwrap(), "\x1b[<64;2;3M");
        let down = ev(MouseAction::Wheel(WheelDirection::Down), 1, 2);
        assert_eq!(enc(&down, MouseMode::Click, true).unwrap(), "\x1b[<65;2;3M");
        let left = ev(MouseAction::Wheel(WheelDirection::Left), 0, 0);
        assert_eq!(
            enc(&left, MouseMode::Motion, true).unwrap(),
            "\x1b[<66;1;1M"
        );
        let right = ev(MouseAction::Wheel(WheelDirection::Right), 0, 0);
        assert_eq!(
            enc(&right, MouseMode::Motion, true).unwrap(),
            "\x1b[<67;1;1M"
        );
    }

    #[test]
    fn legacy_encoding() {
        let press = ev(MouseAction::Press(Button::Left), 0, 0);
        assert_eq!(
            encode_mouse(&press, MouseMode::Click, false).unwrap(),
            b"\x1b[M !!"
        );
        let right = ev(MouseAction::Press(Button::Right), 9, 19);
        assert_eq!(
            encode_mouse(&right, MouseMode::Click, false).unwrap(),
            [0x1b, b'[', b'M', 32 + 2, 32 + 10, 32 + 20]
        );
        // Release reports button 3 whichever button was released.
        let release = ev(MouseAction::Release(Button::Right), 9, 19);
        assert_eq!(
            encode_mouse(&release, MouseMode::Click, false).unwrap(),
            [0x1b, b'[', b'M', 32 + 3, 32 + 10, 32 + 20]
        );
        let drag = ev(MouseAction::Motion(Some(Button::Left)), 0, 0);
        assert_eq!(
            encode_mouse(&drag, MouseMode::Drag, false).unwrap(),
            [0x1b, b'[', b'M', 32 + 32, 33, 33]
        );
        let wheel = ev(MouseAction::Wheel(WheelDirection::Down), 0, 0);
        assert_eq!(
            encode_mouse(&wheel, MouseMode::Click, false).unwrap(),
            [0x1b, b'[', b'M', 32 + 65, 33, 33]
        );
    }

    #[test]
    fn legacy_encoding_drops_positions_it_cannot_express() {
        let far = ev(MouseAction::Press(Button::Left), 223, 0);
        assert_eq!(encode_mouse(&far, MouseMode::Click, false), None);
        let edge = ev(MouseAction::Press(Button::Left), 222, 222);
        assert_eq!(
            encode_mouse(&edge, MouseMode::Click, false).unwrap(),
            [0x1b, b'[', b'M', 32, 255, 255]
        );
        assert!(encode_mouse(&far, MouseMode::Click, true).is_some());
    }

    #[test]
    fn mode_none_reports_nothing() {
        let press = ev(MouseAction::Press(Button::Left), 0, 0);
        assert_eq!(encode_mouse(&press, MouseMode::None, true), None);
    }

    #[test]
    fn alternate_scroll_sends_arrows() {
        assert_eq!(alternate_scroll(true, 3, false), b"\x1b[A\x1b[A\x1b[A");
        assert_eq!(alternate_scroll(false, 1, false), b"\x1b[B");
        assert_eq!(alternate_scroll(true, 2, true), b"\x1bOA\x1bOA");
        assert_eq!(alternate_scroll(false, 0, true), b"");
    }
}
