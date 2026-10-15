//! The system-wide Assistant shortcut (`assistant.global_shortcut`).
//!
//! The real implementation registers a Carbon hot key (`RegisterEventHotKey`), which needs no
//! Accessibility permission and works while another app is in front. It is behind
//! [`GlobalHotkey`]; tests use [`FakeHotkey`].
//!
//! Shortcuts are written like GPUI keystrokes: `alt-space`, `cmd-shift-space`, `ctrl-alt-k`.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// Why a shortcut could not be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    /// The setting is not a shortcut this module understands.
    Invalid(String),
    /// Another app (or the system) owns the shortcut.
    Taken,
    /// The system refused with this status.
    Failed(i32),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyError::Invalid(why) => f.write_str(why),
            HotkeyError::Taken => f.write_str("another app already uses it"),
            HotkeyError::Failed(status) => write!(f, "the system refused it (status {status})"),
        }
    }
}

impl std::error::Error for HotkeyError {}

/// Carbon modifier bits (`cmdKey`, `shiftKey`, `optionKey`, `controlKey`).
const CMD: u32 = 1 << 8;
const SHIFT: u32 = 1 << 9;
const OPTION: u32 = 1 << 11;
const CONTROL: u32 = 1 << 12;

/// A parsed shortcut: a macOS virtual key code and Carbon modifier bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub key_code: u32,
    pub modifiers: u32,
    /// How to show it: `⌥Space`.
    pub display: String,
}

/// `(name, virtual key code, label)` of every key a shortcut may use.
const KEYS: &[(&str, u32, &str)] = &[
    ("a", 0x00, "A"),
    ("s", 0x01, "S"),
    ("d", 0x02, "D"),
    ("f", 0x03, "F"),
    ("h", 0x04, "H"),
    ("g", 0x05, "G"),
    ("z", 0x06, "Z"),
    ("x", 0x07, "X"),
    ("c", 0x08, "C"),
    ("v", 0x09, "V"),
    ("b", 0x0B, "B"),
    ("q", 0x0C, "Q"),
    ("w", 0x0D, "W"),
    ("e", 0x0E, "E"),
    ("r", 0x0F, "R"),
    ("y", 0x10, "Y"),
    ("t", 0x11, "T"),
    ("1", 0x12, "1"),
    ("2", 0x13, "2"),
    ("3", 0x14, "3"),
    ("4", 0x15, "4"),
    ("6", 0x16, "6"),
    ("5", 0x17, "5"),
    ("9", 0x19, "9"),
    ("7", 0x1A, "7"),
    ("8", 0x1C, "8"),
    ("0", 0x1D, "0"),
    ("o", 0x1F, "O"),
    ("u", 0x20, "U"),
    ("i", 0x22, "I"),
    ("p", 0x23, "P"),
    ("enter", 0x24, "Return"),
    ("l", 0x25, "L"),
    ("j", 0x26, "J"),
    ("k", 0x28, "K"),
    ("n", 0x2D, "N"),
    ("m", 0x2E, "M"),
    ("tab", 0x30, "Tab"),
    ("space", 0x31, "Space"),
    ("backspace", 0x33, "Delete"),
    ("escape", 0x35, "Escape"),
    ("f1", 0x7A, "F1"),
    ("f2", 0x78, "F2"),
    ("f3", 0x63, "F3"),
    ("f4", 0x76, "F4"),
    ("f5", 0x60, "F5"),
    ("f6", 0x61, "F6"),
    ("f7", 0x62, "F7"),
    ("f8", 0x64, "F8"),
    ("f9", 0x65, "F9"),
    ("f10", 0x6D, "F10"),
    ("f11", 0x67, "F11"),
    ("f12", 0x6F, "F12"),
    ("left", 0x7B, "←"),
    ("right", 0x7C, "→"),
    ("down", 0x7D, "↓"),
    ("up", 0x7E, "↑"),
];

impl Shortcut {
    /// Parses `alt-space`, `cmd-shift-k`, … (case-insensitive). At least one of `cmd`, `ctrl`,
    /// `alt` is required: a bare key or `shift` plus a key would swallow typing everywhere.
    pub fn parse(text: &str) -> Result<Self, HotkeyError> {
        let invalid = |why: String| HotkeyError::Invalid(why);
        let lowered = text.trim().to_lowercase();
        let mut parts: Vec<&str> = lowered.split('-').collect();
        // `cmd--` style is not supported; an empty part means a stray dash.
        let Some(key) = parts.pop().filter(|k| !k.is_empty()) else {
            return Err(invalid(format!("\"{text}\" does not end in a key")));
        };
        let mut modifiers = 0;
        let mut symbols = String::new();
        let mut seen = [false; 4];
        for part in parts {
            let (index, bit, symbol) = match part {
                "ctrl" | "control" => (0, CONTROL, '⌃'),
                "alt" | "option" | "opt" => (1, OPTION, '⌥'),
                "shift" => (2, SHIFT, '⇧'),
                "cmd" | "command" | "super" => (3, CMD, '⌘'),
                other => return Err(invalid(format!("\"{other}\" is not a modifier"))),
            };
            if std::mem::replace(&mut seen[index], true) {
                return Err(invalid(format!("\"{part}\" is repeated")));
            }
            modifiers |= bit;
            symbols.push(symbol);
        }
        if modifiers & (CMD | CONTROL | OPTION) == 0 {
            return Err(invalid("needs cmd, ctrl or alt".to_owned()));
        }
        // Mac order of the symbols: ⌃⌥⇧⌘.
        let order = ['⌃', '⌥', '⇧', '⌘'];
        let mut display: String = order.into_iter().filter(|s| symbols.contains(*s)).collect();
        let key = if key == "return" { "enter" } else { key };
        let key = if key == "esc" { "escape" } else { key };
        let key = if key == "delete" { "backspace" } else { key };
        let Some((_, key_code, label)) = KEYS.iter().find(|(name, _, _)| *name == key) else {
            return Err(invalid(format!("\"{key}\" is not a key I can register")));
        };
        display.push_str(label);
        Ok(Self {
            key_code: *key_code,
            modifiers,
            display,
        })
    }
}

/// The system-wide hot key.
pub trait GlobalHotkey {
    /// Makes `shortcut` fire [`MacEvent::Hotkey`](super::MacEvent::Hotkey) system-wide,
    /// replacing the one registered before.
    fn register(&self, shortcut: &Shortcut) -> Result<(), HotkeyError>;
    /// Gives the shortcut up.
    fn unregister(&self);
}

/// Does nothing (no hot key integration).
#[derive(Debug, Default, Clone, Copy)]
pub struct Nothing;

impl GlobalHotkey for Nothing {
    fn register(&self, _shortcut: &Shortcut) -> Result<(), HotkeyError> {
        Ok(())
    }

    fn unregister(&self) {}
}

#[derive(Debug, Default)]
struct FakeState {
    active: Option<Shortcut>,
    /// Shortcuts (by display) another app owns.
    taken: Vec<String>,
    attempts: Vec<String>,
    unregistered: usize,
}

/// A hot key that records registrations and refuses the shortcuts it is told are taken.
#[derive(Debug, Clone, Default)]
pub struct FakeHotkey {
    state: Rc<RefCell<FakeState>>,
}

impl FakeHotkey {
    pub fn new() -> Self {
        Self::default()
    }

    /// Another app owns `shortcut` (`⌥Space`, as [`Shortcut::display`] shows it).
    pub fn take(&self, shortcut: &str) {
        self.state.borrow_mut().taken.push(shortcut.to_owned());
    }

    /// The other app lets go of `shortcut`.
    pub fn release(&self, shortcut: &str) {
        self.state.borrow_mut().taken.retain(|s| s != shortcut);
    }

    /// The shortcut registered now.
    pub fn active(&self) -> Option<String> {
        self.state
            .borrow()
            .active
            .as_ref()
            .map(|s| s.display.clone())
    }

    /// Every registration attempt (successful or not), oldest first.
    pub fn attempts(&self) -> Vec<String> {
        self.state.borrow().attempts.clone()
    }

    /// How many times the shortcut was given up.
    pub fn unregistered(&self) -> usize {
        self.state.borrow().unregistered
    }
}

impl GlobalHotkey for FakeHotkey {
    fn register(&self, shortcut: &Shortcut) -> Result<(), HotkeyError> {
        let mut state = self.state.borrow_mut();
        state.attempts.push(shortcut.display.clone());
        // Like the system: a new registration replaces ours, a refused one leaves nothing.
        state.active = None;
        if state.taken.contains(&shortcut.display) {
            return Err(HotkeyError::Taken);
        }
        state.active = Some(shortcut.clone());
        Ok(())
    }

    fn unregister(&self) {
        let mut state = self.state.borrow_mut();
        state.active = None;
        state.unregistered += 1;
    }
}

#[cfg(target_os = "macos")]
pub use carbon::SystemHotkey;

/// Carbon's hot key API: `InstallEventHandler` once, `RegisterEventHotKey` per shortcut. The
/// handler runs on the main thread inside the application's event loop and only queues a
/// [`MacEvent::Hotkey`](super::MacEvent::Hotkey).
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // minimal Carbon FFI; each block says why it is sound.
mod carbon {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::ptr;

    use super::{GlobalHotkey, HotkeyError, Shortcut};
    use crate::macos::{EventSender, MacEvent};

    type OsStatus = i32;
    type EventTargetRef = *mut c_void;
    type EventHandlerRef = *mut c_void;
    type EventHotKeyRef = *mut c_void;
    type EventRef = *mut c_void;
    type EventHandlerCallRef = *mut c_void;
    type EventHandlerUpp = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> EventTargetRef;
        fn InstallEventHandler(
            target: EventTargetRef,
            handler: EventHandlerUpp,
            count: u32,
            types: *const EventTypeSpec,
            user_data: *mut c_void,
            out: *mut EventHandlerRef,
        ) -> OsStatus;
        fn RemoveEventHandler(handler: EventHandlerRef) -> OsStatus;
        fn RegisterEventHotKey(
            key_code: u32,
            modifiers: u32,
            id: EventHotKeyId,
            target: EventTargetRef,
            options: u32,
            out: *mut EventHotKeyRef,
        ) -> OsStatus;
        fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
    }

    /// `'keyb'`.
    const EVENT_CLASS_KEYBOARD: u32 = 0x6B65_7962;
    /// `kEventHotKeyPressed`.
    const EVENT_HOT_KEY_PRESSED: u32 = 5;
    /// `'hutn'`.
    const SIGNATURE: u32 = 0x6875_746E;
    /// `eventHotKeyExistsErr`.
    const HOT_KEY_EXISTS: OsStatus = -9878;
    const NO_ERR: OsStatus = 0;

    extern "C" fn on_hot_key(
        _call: EventHandlerCallRef,
        _event: EventRef,
        user_data: *mut c_void,
    ) -> OsStatus {
        // SAFETY: `user_data` is the `EventSender` that `SystemHotkey::handler` boxed and keeps
        // alive until it has removed this handler (in `Drop`); it is only read.
        let sender = unsafe { &*(user_data.cast::<EventSender>()) };
        // A full queue means the workbench already has a press to handle.
        let _ = sender.try_send(MacEvent::Hotkey);
        NO_ERR
    }

    /// The Carbon hot key of this process.
    pub struct SystemHotkey {
        sender: Box<EventSender>,
        handler: Cell<EventHandlerRef>,
        hot_key: RefCell<Option<EventHotKeyRef>>,
    }

    impl SystemHotkey {
        /// Nothing is installed until the first `register`.
        pub fn new(sender: EventSender) -> Self {
            Self {
                sender: Box::new(sender),
                handler: Cell::new(ptr::null_mut()),
                hot_key: RefCell::new(None),
            }
        }

        fn ensure_handler(&self) -> Result<(), HotkeyError> {
            if !self.handler.get().is_null() {
                return Ok(());
            }
            let spec = EventTypeSpec {
                event_class: EVENT_CLASS_KEYBOARD,
                event_kind: EVENT_HOT_KEY_PRESSED,
            };
            let mut handler: EventHandlerRef = ptr::null_mut();
            let user_data = ptr::from_ref::<EventSender>(&self.sender)
                .cast_mut()
                .cast::<c_void>();
            // SAFETY: the target is the application's; `spec` outlives the call; `user_data`
            // points into the box owned by `self`, which removes the handler before the box
            // is freed; `handler` is a valid out-pointer.
            let status = unsafe {
                InstallEventHandler(
                    GetApplicationEventTarget(),
                    on_hot_key,
                    1,
                    &raw const spec,
                    user_data,
                    &raw mut handler,
                )
            };
            if status != NO_ERR {
                return Err(HotkeyError::Failed(status));
            }
            self.handler.set(handler);
            Ok(())
        }
    }

    impl std::fmt::Debug for SystemHotkey {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SystemHotkey")
                .field("registered", &self.hot_key.borrow().is_some())
                .finish_non_exhaustive()
        }
    }

    impl GlobalHotkey for SystemHotkey {
        fn register(&self, shortcut: &Shortcut) -> Result<(), HotkeyError> {
            self.unregister();
            self.ensure_handler()?;
            let mut hot_key: EventHotKeyRef = ptr::null_mut();
            // SAFETY: plain value arguments and a valid out-pointer; the handler installed
            // above receives the press.
            let status = unsafe {
                RegisterEventHotKey(
                    shortcut.key_code,
                    shortcut.modifiers,
                    EventHotKeyId {
                        signature: SIGNATURE,
                        id: 1,
                    },
                    GetApplicationEventTarget(),
                    0,
                    &raw mut hot_key,
                )
            };
            match status {
                NO_ERR => {
                    *self.hot_key.borrow_mut() = Some(hot_key);
                    Ok(())
                }
                HOT_KEY_EXISTS => Err(HotkeyError::Taken),
                other => Err(HotkeyError::Failed(other)),
            }
        }

        fn unregister(&self) {
            if let Some(hot_key) = self.hot_key.borrow_mut().take() {
                // SAFETY: `hot_key` came from `RegisterEventHotKey` and is released once.
                unsafe { UnregisterEventHotKey(hot_key) };
            }
        }
    }

    impl Drop for SystemHotkey {
        fn drop(&mut self) {
            self.unregister();
            let handler = self.handler.replace(ptr::null_mut());
            if !handler.is_null() {
                // SAFETY: installed by `ensure_handler` and removed once, before `sender`
                // (its user data) is dropped.
                unsafe { RemoveEventHandler(handler) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_space_is_option_and_the_space_bar() {
        let shortcut = Shortcut::parse("alt-space").unwrap();
        assert_eq!(shortcut.key_code, 0x31);
        assert_eq!(shortcut.modifiers, OPTION);
        assert_eq!(shortcut.display, "⌥Space");
    }

    #[test]
    fn modifiers_combine_in_mac_order_and_names_are_forgiving() {
        let shortcut = Shortcut::parse(" Cmd-Shift-ALT-k ").unwrap();
        assert_eq!(shortcut.modifiers, CMD | SHIFT | OPTION);
        assert_eq!(shortcut.key_code, 0x28);
        assert_eq!(shortcut.display, "⌥⇧⌘K");
        assert_eq!(Shortcut::parse("ctrl-f12").unwrap().key_code, 0x6F);
        assert_eq!(Shortcut::parse("option-return").unwrap().display, "⌥Return");
    }

    #[test]
    fn what_would_swallow_typing_or_is_not_understood_is_refused() {
        for text in [
            "",
            "space",
            "shift-space",
            "alt-",
            "alt-alt-space",
            "hyper-space",
            "alt-pagedown",
        ] {
            assert!(
                matches!(Shortcut::parse(text), Err(HotkeyError::Invalid(_))),
                "{text:?} was accepted"
            );
        }
    }

    #[test]
    fn the_fake_refuses_what_another_app_owns_and_replaces_its_own() {
        let hotkey = FakeHotkey::new();
        hotkey.take("⌥Space");
        let alt_space = Shortcut::parse("alt-space").unwrap();
        assert_eq!(hotkey.register(&alt_space), Err(HotkeyError::Taken));
        assert_eq!(hotkey.active(), None);
        let other = Shortcut::parse("cmd-alt-space").unwrap();
        hotkey.register(&other).unwrap();
        assert_eq!(hotkey.active().as_deref(), Some("⌥⌘Space"));
        hotkey.unregister();
        assert_eq!(hotkey.active(), None);
        assert_eq!(hotkey.attempts(), ["⌥Space", "⌥⌘Space"]);
    }
}
