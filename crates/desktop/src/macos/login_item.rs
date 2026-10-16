//! "Start at login" (`app.start_at_login`): registering the bundled launch agent
//! `dev.hutan.daemon.plist` with
//! `SMAppService` (macOS 13+), which makes it a Login Item the user can see and switch off in
//! System Settings → General → Login Items.
//!
//! Everything the desktop does with the system goes through [`LoginItem`]. The real
//! implementation ([`SystemLoginItem`]) exists only on macOS and only when the process runs
//! from `Hutan.app`; tests and every other run use [`FakeLoginItem`] or [`Unavailable`] and
//! never call the framework.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// File name of the agent plist inside `Contents/Library/LaunchAgents`.
pub const AGENT_PLIST: &str = "dev.hutan.daemon.plist";
/// The bundle identifier of Hutan.app (ADR 0007).
pub const BUNDLE_ID: &str = "dev.hutan.Hutan";

/// What the system says about the launch agent (`SMAppServiceStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemStatus {
    /// Registered and allowed: launchd runs the daemon at login.
    Enabled,
    /// Registered, but the user has not allowed it (or switched it off) in Login Items.
    RequiresApproval,
    /// Not registered.
    NotRegistered,
    /// The system cannot find the plist in the app bundle.
    NotFound,
}

impl LoginItemStatus {
    /// The words shown next to the toggle.
    pub fn label(self) -> &'static str {
        match self {
            LoginItemStatus::Enabled => "enabled",
            LoginItemStatus::RequiresApproval => "requires approval",
            LoginItemStatus::NotRegistered => "not registered",
            LoginItemStatus::NotFound => "not found",
        }
    }

    /// Registered with the system, approved or not.
    pub fn registered(self) -> bool {
        matches!(
            self,
            LoginItemStatus::Enabled | LoginItemStatus::RequiresApproval
        )
    }
}

/// The login item of the daemon.
pub trait LoginItem {
    /// Whether registering makes sense here: the desktop runs from `Hutan.app`.
    fn available(&self) -> bool;
    /// The current status; cheap, safe to call whenever the window activates.
    fn status(&self) -> LoginItemStatus;
    /// `SMAppService.register()`. The error is the system's own message.
    fn register(&self) -> Result<(), String>;
    /// `SMAppService.unregister()`.
    fn unregister(&self) -> Result<(), String>;
    /// `SMAppService.openSystemSettingsLoginItems()`.
    fn open_login_items_settings(&self);
}

/// What runs outside the bundle: nothing is available and nothing is called.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unavailable;

impl LoginItem for Unavailable {
    fn available(&self) -> bool {
        false
    }

    fn status(&self) -> LoginItemStatus {
        LoginItemStatus::NotRegistered
    }

    fn register(&self) -> Result<(), String> {
        Err("starting at login is available in Hutan.app".to_owned())
    }

    fn unregister(&self) -> Result<(), String> {
        Ok(())
    }

    fn open_login_items_settings(&self) {}
}

/// A call the desktop made to a [`FakeLoginItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemCall {
    Register,
    Unregister,
    OpenSettings,
}

#[derive(Debug)]
struct FakeState {
    status: LoginItemStatus,
    /// The status a successful `register` leaves behind (a fresh registration of an
    /// unapproved item is `RequiresApproval`).
    after_register: LoginItemStatus,
    register_error: Option<String>,
    unregister_error: Option<String>,
    calls: Vec<LoginItemCall>,
}

/// A scripted login item for tests: records the calls, answers with the statuses it is told.
#[derive(Debug, Clone)]
pub struct FakeLoginItem {
    state: Rc<RefCell<FakeState>>,
}

impl FakeLoginItem {
    /// Available, not registered; registering makes it `Enabled`.
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(FakeState {
                status: LoginItemStatus::NotRegistered,
                after_register: LoginItemStatus::Enabled,
                register_error: None,
                unregister_error: None,
                calls: Vec::new(),
            })),
        }
    }

    /// The system changes the status behind the app's back (the user answers in Login Items).
    pub fn set_status(&self, status: LoginItemStatus) {
        self.state.borrow_mut().status = status;
    }

    /// The status a successful `register` leaves behind.
    pub fn set_after_register(&self, status: LoginItemStatus) {
        self.state.borrow_mut().after_register = status;
    }

    /// Makes `register` fail with `message`.
    pub fn fail_register(&self, message: &str) {
        self.state.borrow_mut().register_error = Some(message.to_owned());
    }

    /// Makes `unregister` fail with `message`.
    pub fn fail_unregister(&self, message: &str) {
        self.state.borrow_mut().unregister_error = Some(message.to_owned());
    }

    /// Every call so far, oldest first.
    pub fn calls(&self) -> Vec<LoginItemCall> {
        self.state.borrow().calls.clone()
    }
}

impl Default for FakeLoginItem {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginItem for FakeLoginItem {
    fn available(&self) -> bool {
        true
    }

    fn status(&self) -> LoginItemStatus {
        self.state.borrow().status
    }

    fn register(&self) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        state.calls.push(LoginItemCall::Register);
        if let Some(error) = state.register_error.clone() {
            return Err(error);
        }
        state.status = state.after_register;
        Ok(())
    }

    fn unregister(&self) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        state.calls.push(LoginItemCall::Unregister);
        if let Some(error) = state.unregister_error.clone() {
            return Err(error);
        }
        state.status = LoginItemStatus::NotRegistered;
        Ok(())
    }

    fn open_login_items_settings(&self) {
        self.state
            .borrow_mut()
            .calls
            .push(LoginItemCall::OpenSettings);
    }
}

/// Whether `exe` is `<Something>.app/Contents/MacOS/<exe>`.
pub fn inside_app_bundle(exe: &Path) -> bool {
    let Some(macos) = exe.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let Some(app) = contents.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && app.extension().is_some_and(|e| e == "app")
}

#[cfg(target_os = "macos")]
pub use system::SystemLoginItem;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // Objective-C calls; each block says why it is sound.
mod system {
    use objc2::rc::Retained;
    use objc2_foundation::{NSBundle, NSString};
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    use super::{AGENT_PLIST, BUNDLE_ID, LoginItem, LoginItemStatus, inside_app_bundle};

    /// `SMAppService.agent(plistName:)` of the bundled daemon agent.
    pub struct SystemLoginItem {
        service: Retained<SMAppService>,
    }

    impl SystemLoginItem {
        /// The login item when this process runs from `Hutan.app` (its executable is in
        /// `Contents/MacOS` and the main bundle's identifier is `dev.hutan.Hutan`); `None`
        /// anywhere else, without touching the framework.
        pub fn new() -> Option<Self> {
            let exe = std::env::current_exe().ok()?;
            if !inside_app_bundle(&exe) {
                return None;
            }
            let identifier = NSBundle::mainBundle().bundleIdentifier()?;
            if identifier.to_string() != BUNDLE_ID {
                return None;
            }
            let plist = NSString::from_str(AGENT_PLIST);
            // SAFETY: a class method taking a valid NSString; it only creates the proxy object.
            let service = unsafe { SMAppService::agentServiceWithPlistName(&plist) };
            Some(Self { service })
        }
    }

    impl std::fmt::Debug for SystemLoginItem {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SystemLoginItem").finish_non_exhaustive()
        }
    }

    impl LoginItem for SystemLoginItem {
        fn available(&self) -> bool {
            true
        }

        fn status(&self) -> LoginItemStatus {
            // SAFETY: `status` is a plain getter on a live service object.
            let status = unsafe { self.service.status() };
            if status == SMAppServiceStatus::Enabled {
                LoginItemStatus::Enabled
            } else if status == SMAppServiceStatus::RequiresApproval {
                LoginItemStatus::RequiresApproval
            } else if status == SMAppServiceStatus::NotFound {
                LoginItemStatus::NotFound
            } else {
                LoginItemStatus::NotRegistered
            }
        }

        fn register(&self) -> Result<(), String> {
            // SAFETY: the NSError out-parameter is handled by the generated binding.
            unsafe { self.service.registerAndReturnError() }
                .map_err(|error| error.localizedDescription().to_string())
        }

        fn unregister(&self) -> Result<(), String> {
            // SAFETY: as for `register`.
            unsafe { self.service.unregisterAndReturnError() }
                .map_err(|error| error.localizedDescription().to_string())
        }

        fn open_login_items_settings(&self) {
            // SAFETY: a class method without arguments.
            unsafe { SMAppService::openSystemSettingsLoginItems() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_executables_inside_an_app_bundle_count() {
        assert!(inside_app_bundle(Path::new(
            "/Users/me/Applications/Hutan.app/Contents/MacOS/hutan-desktop"
        )));
        assert!(!inside_app_bundle(Path::new(
            "/work/hutan/target/debug/hutan-desktop"
        )));
        assert!(!inside_app_bundle(Path::new(
            "/x/Hutan.app/Contents/Resources/hutan-desktop"
        )));
        assert!(!inside_app_bundle(Path::new(
            "/x/Hutan/Contents/MacOS/hutan-desktop"
        )));
        assert!(!inside_app_bundle(Path::new("hutan-desktop")));
    }

    #[test]
    fn the_fake_records_calls_and_follows_its_script() {
        let item = FakeLoginItem::new();
        assert_eq!(item.status(), LoginItemStatus::NotRegistered);
        item.set_after_register(LoginItemStatus::RequiresApproval);
        item.register().unwrap();
        assert_eq!(item.status(), LoginItemStatus::RequiresApproval);
        assert!(item.status().registered());
        item.unregister().unwrap();
        item.fail_register("boom");
        assert_eq!(item.register(), Err("boom".to_owned()));
        assert_eq!(item.status(), LoginItemStatus::NotRegistered);
        assert_eq!(
            item.calls(),
            [
                LoginItemCall::Register,
                LoginItemCall::Unregister,
                LoginItemCall::Register
            ]
        );
    }
}
