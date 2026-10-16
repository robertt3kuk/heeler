//! The notification permission, asked for by the first-run setup's "Allow notifications".
//!
//! The daemon posts the notifications itself (M6-B, `docs/packaging.md` § Native notifications)
//! under the app identifier `dev.hutan.Hutan`, and the system gives the permission to that
//! identity: the desktop is signed with the same one, so the answer it gets here is the
//! daemon's too. Nothing asks until the user presses the button: the permission prompt is the
//! system's, and the daemon asks by itself at its first post otherwise.
//!
//! Everything goes through [`NotificationPermission`]. The real implementation
//! ([`SystemNotifications`]) exists only on macOS and only when the process runs from
//! `Hutan.app`; tests and every other run use [`FakeNotifications`] or [`Unavailable`] and never
//! call the framework.

use std::cell::RefCell;
use std::rc::Rc;

use tokio::sync::oneshot;

/// What the system answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationAnswer {
    /// Hutan may post notifications.
    Allowed,
    /// The user said no (now, or earlier: the system does not ask twice). Only System Settings →
    /// Notifications changes it.
    Denied,
    /// The request could not be made; the text is the system's own.
    Failed(String),
}

/// Asking the system whether Hutan may post notifications.
pub trait NotificationPermission {
    /// Whether asking makes sense here: the desktop runs from `Hutan.app`.
    fn available(&self) -> bool;
    /// Asks. The system shows its prompt the first time; afterwards it answers at once with the
    /// earlier decision. The answer arrives on the receiver, never later than the system gives it.
    fn request(&self) -> oneshot::Receiver<NotificationAnswer>;
}

/// What runs outside the bundle: nothing is available and nothing is called.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unavailable;

impl NotificationPermission for Unavailable {
    fn available(&self) -> bool {
        false
    }

    fn request(&self) -> oneshot::Receiver<NotificationAnswer> {
        let (answer, receiver) = oneshot::channel();
        let _ = answer.send(NotificationAnswer::Failed(
            "notifications are available in Hutan.app".to_owned(),
        ));
        receiver
    }
}

#[derive(Debug)]
struct FakeState {
    answer: NotificationAnswer,
    requests: usize,
    /// The answers are held back until [`FakeNotifications::release`].
    held: Vec<(oneshot::Sender<NotificationAnswer>, NotificationAnswer)>,
    hold: bool,
}

/// A scripted permission for tests: counts the requests, answers as it is told.
#[derive(Debug, Clone)]
pub struct FakeNotifications {
    state: Rc<RefCell<FakeState>>,
}

impl FakeNotifications {
    /// Available; the user allows.
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(FakeState {
                answer: NotificationAnswer::Allowed,
                requests: 0,
                held: Vec::new(),
                hold: false,
            })),
        }
    }

    /// What the next request is answered with.
    pub fn set_answer(&self, answer: NotificationAnswer) {
        self.state.borrow_mut().answer = answer;
    }

    /// Keeps answers back (the prompt is up) until [`release`](Self::release).
    pub fn hold_answers(&self) {
        self.state.borrow_mut().hold = true;
    }

    /// The user answers the prompt: every held request gets its answer, and later ones are
    /// answered at once again.
    pub fn release(&self) {
        let held = {
            let mut state = self.state.borrow_mut();
            state.hold = false;
            std::mem::take(&mut state.held)
        };
        for (sender, answer) in held {
            let _ = sender.send(answer);
        }
    }

    /// How many times the desktop asked.
    pub fn requests(&self) -> usize {
        self.state.borrow().requests
    }
}

impl Default for FakeNotifications {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationPermission for FakeNotifications {
    fn available(&self) -> bool {
        true
    }

    fn request(&self) -> oneshot::Receiver<NotificationAnswer> {
        let (sender, receiver) = oneshot::channel();
        let mut state = self.state.borrow_mut();
        state.requests += 1;
        let answer = state.answer.clone();
        if state.hold {
            state.held.push((sender, answer));
        } else {
            let _ = sender.send(answer);
        }
        receiver
    }
}

#[cfg(target_os = "macos")]
pub use system::SystemNotifications;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // Objective-C calls; each block says why it is sound.
mod system {
    use std::sync::Mutex;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError};
    use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};
    use tokio::sync::oneshot;

    use super::{NotificationAnswer, NotificationPermission};
    use crate::macos::login_item::{BUNDLE_ID, inside_app_bundle};

    /// `UNUserNotificationCenter` of the app, asked on the main thread.
    #[derive(Debug)]
    pub struct SystemNotifications;

    impl SystemNotifications {
        /// The permission when this process runs from `Hutan.app` (its executable is in
        /// `Contents/MacOS` and the main bundle's identifier is `dev.hutan.Hutan`); `None`
        /// anywhere else, without touching the framework (outside a bundle it raises).
        pub fn new() -> Option<Self> {
            let exe = std::env::current_exe().ok()?;
            if !inside_app_bundle(&exe) {
                return None;
            }
            let identifier = NSBundle::mainBundle().bundleIdentifier()?;
            (identifier.to_string() == BUNDLE_ID).then_some(Self)
        }
    }

    impl NotificationPermission for SystemNotifications {
        fn available(&self) -> bool {
            true
        }

        fn request(&self) -> oneshot::Receiver<NotificationAnswer> {
            let (answer, receiver) = oneshot::channel();
            let answer = Mutex::new(Some(answer));
            // The system calls the block once, on a queue of its own: only the channel is used.
            let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
                // SAFETY: the system passes either null or a valid `NSError` for the call.
                let error =
                    unsafe { error.as_ref() }.map(|error| error.localizedDescription().to_string());
                let outcome = match error {
                    Some(error) => NotificationAnswer::Failed(error),
                    None if granted.as_bool() => NotificationAnswer::Allowed,
                    None => NotificationAnswer::Denied,
                };
                if let Ok(mut slot) = answer.lock()
                    && let Some(sender) = slot.take()
                {
                    let _ = sender.send(outcome);
                }
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .requestAuthorizationWithOptions_completionHandler(
                    UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                    &block,
                );
            receiver
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_counts_requests_and_answers_as_scripted() {
        let fake = FakeNotifications::new();
        let mut allowed = fake.request();
        assert_eq!(allowed.try_recv(), Ok(NotificationAnswer::Allowed));
        fake.set_answer(NotificationAnswer::Denied);
        let mut denied = fake.request();
        assert_eq!(denied.try_recv(), Ok(NotificationAnswer::Denied));
        assert_eq!(fake.requests(), 2);

        fake.hold_answers();
        let mut pending = fake.request();
        assert!(pending.try_recv().is_err(), "the prompt is still up");
        fake.release();
        assert_eq!(pending.try_recv(), Ok(NotificationAnswer::Denied));
    }

    #[test]
    fn outside_the_bundle_nothing_is_available() {
        assert!(!Unavailable.available());
        let mut answer = Unavailable.request();
        assert!(matches!(
            answer.try_recv(),
            Ok(NotificationAnswer::Failed(_))
        ));
    }
}
