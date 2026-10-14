//! GPUI event types → the GPUI-free input types.

use gpui::{Keystroke, Modifiers, MouseButton};

use crate::keys::{Key, KeyInput, Mods};
use crate::mouse::Button;

pub(crate) fn mods(m: Modifiers) -> Mods {
    Mods {
        shift: m.shift,
        ctrl: m.control,
        alt: m.alt,
        super_key: m.platform,
    }
}

pub(crate) fn button(b: MouseButton) -> Option<Button> {
    match b {
        MouseButton::Left => Some(Button::Left),
        MouseButton::Middle => Some(Button::Middle),
        MouseButton::Right => Some(Button::Right),
        MouseButton::Navigate(_) => None,
    }
}

/// The key of a GPUI keystroke; `None` for keys the terminal has no encoding for.
pub(crate) fn key_input(keystroke: &Keystroke) -> Option<KeyInput> {
    let key = match keystroke.key.as_str() {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "space" => Key::Char(' '),
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        name => {
            if let Some(n) = name
                .strip_prefix('f')
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| (1..=35).contains(n))
            {
                Key::F(n)
            } else {
                let mut chars = name.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Key::Char(c),
                    _ => return None,
                }
            }
        }
    };
    Some(KeyInput {
        key,
        mods: mods(keystroke.modifiers),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn named_and_character_keys() {
        assert_eq!(key_input(&ks("up")).unwrap().key, Key::Up);
        assert_eq!(key_input(&ks("pageup")).unwrap().key, Key::PageUp);
        assert_eq!(key_input(&ks("f5")).unwrap().key, Key::F(5));
        assert_eq!(key_input(&ks("space")).unwrap().key, Key::Char(' '));
        assert_eq!(key_input(&ks("a")).unwrap().key, Key::Char('a'));
        assert_eq!(key_input(&ks("enter")).unwrap().key, Key::Enter);
    }

    #[test]
    fn modifiers_map_across() {
        let input = key_input(&ks("ctrl-shift-alt-cmd-x")).unwrap();
        assert_eq!(
            input.mods,
            Mods {
                shift: true,
                ctrl: true,
                alt: true,
                super_key: true
            }
        );
    }

    #[test]
    fn unknown_multi_character_names_are_ignored() {
        let unknown = Keystroke {
            key: "capslock".into(),
            ..Keystroke::default()
        };
        assert_eq!(key_input(&unknown), None);
    }
}
