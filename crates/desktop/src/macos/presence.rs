//! How present the app is on the desktop while "closing the last window" keeps it in the menu
//! bar (`app.on_last_window_close = "menu_bar"`): without a Dock icon and application menu
//! (accessory activation policy) and with its windows hidden, until a window is wanted again.

use std::cell::RefCell;
use std::rc::Rc;

/// The application's activation policy and visibility.
pub trait Presence {
    /// `true`: no Dock icon and no application menu (`NSApplicationActivationPolicyAccessory`);
    /// `false`: a normal app again (`Regular`).
    fn set_accessory(&self, accessory: bool);
    /// Hides every window of the app (`NSApp.hide`).
    fn hide_app(&self);
    /// Shows them again (`NSApp.unhide`).
    fn show_app(&self);
}

/// Does nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct Nothing;

impl Presence for Nothing {
    fn set_accessory(&self, _accessory: bool) {}

    fn hide_app(&self) {}

    fn show_app(&self) {}
}

/// A call the desktop made to a [`FakePresence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceCall {
    Accessory(bool),
    Hide,
    Show,
}

/// Records what the desktop asked of the system.
#[derive(Debug, Clone, Default)]
pub struct FakePresence {
    calls: Rc<RefCell<Vec<PresenceCall>>>,
}

impl FakePresence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call so far, oldest first.
    pub fn calls(&self) -> Vec<PresenceCall> {
        self.calls.borrow().clone()
    }

    /// The last activation policy set (`true`: accessory); `false` before any call.
    pub fn accessory(&self) -> bool {
        self.calls
            .borrow()
            .iter()
            .rev()
            .find_map(|call| match call {
                PresenceCall::Accessory(on) => Some(*on),
                _ => None,
            })
            .unwrap_or(false)
    }
}

impl Presence for FakePresence {
    fn set_accessory(&self, accessory: bool) {
        self.calls
            .borrow_mut()
            .push(PresenceCall::Accessory(accessory));
    }

    fn hide_app(&self) {
        self.calls.borrow_mut().push(PresenceCall::Hide);
    }

    fn show_app(&self) {
        self.calls.borrow_mut().push(PresenceCall::Show);
    }
}

/// `NSApplication` of this process.
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPresence;

#[cfg(target_os = "macos")]
impl Presence for SystemPresence {
    fn set_accessory(&self, accessory: bool) {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        use objc2_foundation::MainThreadMarker;
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!("the activation policy was changed off the main thread; ignored");
            return;
        };
        let policy = if accessory {
            NSApplicationActivationPolicy::Accessory
        } else {
            NSApplicationActivationPolicy::Regular
        };
        if !NSApplication::sharedApplication(mtm).setActivationPolicy(policy) {
            tracing::warn!(accessory, "the system refused the activation policy");
        }
    }

    fn hide_app(&self) {
        use objc2_app_kit::NSApplication;
        use objc2_foundation::MainThreadMarker;
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).hide(None);
        }
    }

    fn show_app(&self) {
        use objc2_app_kit::NSApplication;
        use objc2_foundation::MainThreadMarker;
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).unhide(None);
        }
    }
}
