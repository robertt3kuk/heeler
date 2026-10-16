//! The Cocoa side of the daemon's menu bar item: `NSStatusItem` and `NSAlert` through objc2,
//! driven from the process's main thread, the one that also hosts the native notifications
//! (`notify_native::cocoa`).
//!
//! **Threads.** Everything that touches an Objective-C object runs on the main thread, inside
//! the loop of [`run_main_loop`](crate::notify_native::cocoa::run_main_loop). The host's calls
//! only record what is wanted ([`Desired`]: the content, or none, and the stop confirmations
//! asked for) and schedule one drain block on the main queue, so the tokio side never waits for
//! Cocoa and Cocoa never waits for tokio; a burst of `show`s costs one update with the last
//! content. Clicks go the other way through a bounded channel.
//!
//! **Nothing here runs unless asked.** The application object (an accessory: no Dock icon, no
//! menu) is created by the first drain that has something to show, and the item is created
//! when the first model arrives.
//!
//! **Opening Hutan.** [`OpenApp`] runs `open -n <Hutan.app>`: `-n` because a running `hutand`
//! counts as an instance of the app for Launch Services (same bundle), and the desktop's own
//! lock (`desktop.lock`) makes a second desktop bring the first one forward and exit. The data
//! folder of a daemon that was given `HUTAN_HOME` / `HUTAN_SOCKET` is passed on to the desktop
//! (`--env`), so a daemon on a scratch folder opens a desktop on that folder.

#![allow(unsafe_code)] // Objective-C calls and class definition; each block says why it is sound.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationDelegate,
    NSBitmapImageRep, NSCellImagePosition, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem,
    NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSData, NSObject, NSObjectProtocol, NSSize, NSString};
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::{
    AppLauncher, CHOICE_QUEUE, MenuChoice, MenuModel, MenuRow, StatusItemHost, StopAnswer,
};
use crate::notify_native::cocoa::{on_main_thread, shared_application};

/// The glyph at 18 and 36 pixels (`just menubar-icon`), the one the desktop's item uses.
const GLYPH_1X: &[u8] = include_bytes!("../../../../assets/macos/MenuBarIcon.png");
const GLYPH_2X: &[u8] = include_bytes!("../../../../assets/macos/MenuBarIcon@2x.png");
const GLYPH_POINTS: f64 = 18.0;
/// Next to the glyph while something needs the user.
const DOT: &str = "•";
/// Stop confirmations waiting for the main thread; more are answered with a cancel.
const MAX_CONFIRMS: usize = 4;

// ---- Opening Hutan -----------------------------------------------------------------------------------

/// Starts (or reactivates) the app this daemon is part of.
#[derive(Debug)]
pub struct OpenApp {
    app: PathBuf,
    /// `NAME=VALUE` pairs the desktop must start with.
    env: Vec<String>,
}

impl OpenApp {
    /// `app` is `Hutan.app`; `HUTAN_HOME` and `HUTAN_SOCKET` of this process (when set and not
    /// empty) are handed to the desktop.
    pub fn new(app: &Path) -> Self {
        let env = ["HUTAN_HOME", "HUTAN_SOCKET"]
            .into_iter()
            .filter_map(|name| {
                let value = std::env::var_os(name).filter(|v| !v.is_empty())?;
                Some(format!("{name}={}", value.to_string_lossy()))
            })
            .collect();
        Self {
            app: app.to_owned(),
            env,
        }
    }

    /// The arguments of `/usr/bin/open`.
    pub fn args(&self) -> Vec<OsString> {
        let mut args = vec![OsString::from("-n")];
        for pair in &self.env {
            args.push(OsString::from("--env"));
            args.push(OsString::from(pair));
        }
        args.push(self.app.clone().into_os_string());
        args
    }
}

impl AppLauncher for OpenApp {
    fn open_hutan(&self) {
        let spawned = Command::new("/usr/bin/open")
            .args(self.args())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                info!(app = %self.app.display(), "opening Hutan");
                // `open` returns once the app is launched; reap it.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(error) => warn!(%error, app = %self.app.display(), "could not run `open`"),
        }
    }
}

// ---- The host ----------------------------------------------------------------------------------------

/// What the main thread is to make true.
#[derive(Default)]
struct Desired {
    /// The content of the item; `None` while it is not to be on the menu bar.
    model: Option<MenuModel>,
    confirms: VecDeque<(u32, oneshot::Sender<StopAnswer>)>,
}

#[derive(Debug)]
struct Shared {
    desired: Mutex<Desired>,
    /// A drain block is queued on the main queue (at most one is, so the queue stays bounded).
    scheduled: AtomicBool,
    clicks: mpsc::Sender<MenuChoice>,
}

impl std::fmt::Debug for Desired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desired")
            .field("shown", &self.model.is_some())
            .field("confirms", &self.confirms.len())
            .finish()
    }
}

/// `NSStatusItem` as a [`StatusItemHost`].
#[derive(Debug)]
pub struct SystemStatusItem {
    shared: Arc<Shared>,
    choices: Mutex<Option<mpsc::Receiver<MenuChoice>>>,
}

impl SystemStatusItem {
    /// Builds the channels only; no system API runs until the first `show`.
    pub fn new() -> Self {
        let (clicks, choices) = mpsc::channel(CHOICE_QUEUE);
        Self {
            shared: Arc::new(Shared {
                desired: Mutex::new(Desired::default()),
                scheduled: AtomicBool::new(false),
                clicks,
            }),
            choices: Mutex::new(Some(choices)),
        }
    }

    fn schedule(&self) {
        if !self.shared.scheduled.swap(true, Ordering::AcqRel) {
            let shared = self.shared.clone();
            on_main_thread(move || drain(&shared));
        }
    }
}

impl Default for SystemStatusItem {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusItemHost for SystemStatusItem {
    fn show(&self, model: &MenuModel) {
        self.shared.desired.lock().model = Some(model.clone());
        self.schedule();
    }

    fn hide(&self) {
        self.shared.desired.lock().model = None;
        self.schedule();
    }

    fn confirm_stop(&self, live_sessions: u32) -> oneshot::Receiver<StopAnswer> {
        let (answer, receiver) = oneshot::channel();
        {
            let mut desired = self.shared.desired.lock();
            if desired.confirms.len() >= MAX_CONFIRMS {
                warn!("too many stop confirmations are waiting; this one is cancelled");
                return receiver; // `answer` drops: the caller counts it as a cancel.
            }
            desired.confirms.push_back((live_sessions, answer));
        }
        self.schedule();
        receiver
    }

    fn choices(&self) -> Option<mpsc::Receiver<MenuChoice>> {
        self.choices.lock().take()
    }
}

// ---- Main-thread state -------------------------------------------------------------------------------

struct Native {
    item: Retained<NSStatusItem>,
    target: Retained<MenuTarget>,
}

struct MainState {
    app: Retained<NSApplication>,
    /// The application keeps its delegate weakly.
    _delegate: Retained<AppDelegate>,
    clicks: mpsc::Sender<MenuChoice>,
    native: Option<Native>,
    /// What the item shows now; the same content is not put on it again.
    applied: Option<MenuModel>,
}

thread_local! {
    /// Created by the first drain that has something to do, on the main thread; Objective-C
    /// objects never leave it.
    static STATE: RefCell<Option<MainState>> = const { RefCell::new(None) };
}

/// Runs on the main thread: makes `desired` true.
fn drain(shared: &Shared) {
    // Before reading: a call from now on schedules another drain.
    shared.scheduled.store(false, Ordering::Release);
    let Some(mtm) = MainThreadMarker::new() else {
        warn!("the menu bar item was driven off the main thread; ignored");
        return;
    };
    let (model, confirms) = {
        let mut desired = shared.desired.lock();
        (desired.model.clone(), std::mem::take(&mut desired.confirms))
    };
    let app = STATE.with(|state| {
        let mut state = state.borrow_mut();
        // Nothing to take off before anything was put on, and nothing to ask without a user
        // interface that the first model builds.
        if state.is_none() && model.is_none() && confirms.is_empty() {
            return None;
        }
        let state = state.get_or_insert_with(|| MainState::new(mtm, shared));
        state.apply(mtm, model);
        Some(state.app.clone())
    });
    // The modal loop of an alert serves the main queue, so a drain may run inside it: no borrow
    // of the state is held while it runs.
    if let Some(app) = app {
        for (live, answer) in confirms {
            let _ = answer.send(MainState::confirm_stop(mtm, &app, live));
        }
    }
}

impl MainState {
    fn new(mtm: MainThreadMarker, shared: &Shared) -> Self {
        let app = shared_application(mtm);
        let delegate = AppDelegate::new(mtm, shared.clicks.clone());
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        info!("the daemon's menu bar item: application ready");
        Self {
            app,
            _delegate: delegate,
            clicks: shared.clicks.clone(),
            native: None,
            applied: None,
        }
    }

    fn apply(&mut self, mtm: MainThreadMarker, model: Option<MenuModel>) {
        let Some(model) = model else {
            self.applied = None;
            if let Some(native) = self.native.take() {
                NSStatusBar::systemStatusBar().removeStatusItem(&native.item);
                debug!("the menu bar item was removed");
            }
            return;
        };
        if self.applied.as_ref() == Some(&model) && self.native.is_some() {
            return;
        }
        if self.native.is_none() {
            self.native = self.create(mtm);
        }
        let Some(native) = &self.native else {
            warn!("the status item has no button; the menu bar item is off");
            return;
        };
        if let Some(button) = native.item.button(mtm) {
            button.setTitle(&NSString::from_str(if model.dot() { DOT } else { "" }));
        }
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let mut choices = Vec::new();
        for row in model.rows() {
            match row {
                MenuRow::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
                MenuRow::Label(text) => {
                    let item = menu_item(mtm, &text, None);
                    item.setEnabled(false);
                    menu.addItem(&item);
                }
                MenuRow::Item {
                    title,
                    choice,
                    checked,
                } => {
                    let item = menu_item(mtm, &title, Some(sel!(chosen:)));
                    item.setTag(isize::try_from(choices.len()).unwrap_or(isize::MAX));
                    // SAFETY: the target outlives the menu: both are owned by `Native`.
                    unsafe { item.setTarget(Some(&native.target)) };
                    if checked {
                        item.setState(NSControlStateValueOn);
                    }
                    choices.push(choice);
                    menu.addItem(&item);
                }
            }
        }
        *native.target.ivars().choices.borrow_mut() = choices;
        native.item.setMenu(Some(&menu));
        self.applied = Some(model);
    }

    fn create(&self, mtm: MainThreadMarker) -> Option<Native> {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let button = item.button(mtm)?;
        button.setImage(Some(&glyph()));
        button.setImagePosition(NSCellImagePosition::ImageLeft);
        Some(Native {
            item,
            target: MenuTarget::new(self.clicks.clone()),
        })
    }

    /// Asks in a modal alert; returns when the user has answered. The main thread does not
    /// serve anything else meanwhile apart from what the modal loop serves.
    fn confirm_stop(mtm: MainThreadMarker, app: &NSApplication, live_sessions: u32) -> StopAnswer {
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Warning);
        let what = if live_sessions == 1 {
            "1 running session".to_owned()
        } else {
            format!("{live_sessions} running sessions")
        };
        alert.setMessageText(&NSString::from_str("Stop the Hutan daemon?"));
        alert.setInformativeText(&NSString::from_str(&format!(
            "It has {what}. They end with the daemon."
        )));
        alert.addButtonWithTitle(&NSString::from_str("Stop Sessions"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        // An accessory has no window to be in front of; without this the alert hides behind
        // whatever is active.
        #[allow(deprecated)] // `activate` needs macOS 14; the app runs on 13.
        app.activateIgnoringOtherApps(true);
        let response = alert.runModal();
        if response == NSAlertFirstButtonReturn {
            StopAnswer::StopSessions
        } else {
            StopAnswer::Cancel
        }
    }
}

/// The template glyph: the 18-pixel PNG with the 36-pixel one as its @2x representation.
fn glyph() -> Retained<NSImage> {
    let one = NSData::with_bytes(GLYPH_1X);
    let two = NSData::with_bytes(GLYPH_2X);
    let image = NSImage::initWithData(NSImage::alloc(), &one).unwrap_or_default();
    let size = NSSize::new(GLYPH_POINTS, GLYPH_POINTS);
    if let Some(rep) = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &two) {
        // 36 pixels in 18 points: the representation for 2x displays.
        rep.setSize(size);
        image.addRepresentation(&rep);
    }
    image.setSize(size);
    image.setTemplate(true);
    image
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
) -> Retained<NSMenuItem> {
    // SAFETY: `initWithTitle:action:keyEquivalent:` on a freshly allocated item; the key
    // equivalent is empty.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc::<NSMenuItem>(),
            &NSString::from_str(title),
            action,
            &NSString::new(),
        )
    }
}

// ---- Menu target and application delegate ---------------------------------------------------------------

struct TargetIvars {
    clicks: mpsc::Sender<MenuChoice>,
    /// What each menu item's tag stands for, for the menu on screen.
    choices: RefCell<Vec<MenuChoice>>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and `MenuTarget` has no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "HutanDaemonMenuTarget"]
    #[ivars = TargetIvars]
    struct MenuTarget;

    // SAFETY: `NSObjectProtocol` has no requirements.
    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        // SAFETY: the selector takes the sending menu item, as `NSMenuItem` actions do.
        #[unsafe(method(chosen:))]
        fn chosen(&self, sender: &NSMenuItem) {
            let choice = usize::try_from(sender.tag())
                .ok()
                .and_then(|tag| self.ivars().choices.borrow().get(tag).copied());
            if let Some(choice) = choice
                && let Err(error) = self.ivars().clicks.try_send(choice)
            {
                warn!(%error, "a menu bar choice could not be delivered");
            }
        }
    }
);

impl MenuTarget {
    fn new(clicks: mpsc::Sender<MenuChoice>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(TargetIvars {
            clicks,
            choices: RefCell::new(Vec::new()),
        });
        // SAFETY: `init` of `NSObject`, the superclass, on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

struct DelegateIvars {
    clicks: mpsc::Sender<MenuChoice>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and `AppDelegate` has no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "HutanDaemonAppDelegate"]
    #[ivars = DelegateIvars]
    struct AppDelegate;

    // SAFETY: `NSObjectProtocol` has no requirements.
    unsafe impl NSObjectProtocol for AppDelegate {}

    // SAFETY: the method signature is that of the protocol.
    unsafe impl NSApplicationDelegate for AppDelegate {
        /// Launch Services found this process when Hutan.app was opened (it counts as an
        /// instance of the app): start the desktop instead of doing nothing.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _has_visible_windows: bool) -> bool {
            if let Err(error) = self.ivars().clicks.try_send(MenuChoice::OpenHutan) {
                warn!(%error, "a reopen request could not be delivered");
            }
            false
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, clicks: mpsc::Sender<MenuChoice>) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(DelegateIvars { clicks });
        // SAFETY: `init` of `NSObject`, the superclass, on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn the_launcher_opens_a_new_instance_of_the_app_with_the_data_folder() {
        let app = OpenApp {
            app: PathBuf::from("/Users/x/Applications/Hutan.app"),
            env: vec![
                "HUTAN_HOME=/tmp/scratch".to_owned(),
                "HUTAN_SOCKET=/tmp/s.sock".to_owned(),
            ],
        };
        let args = app.args();
        let args: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
        assert_eq!(
            args,
            [
                "-n",
                "--env",
                "HUTAN_HOME=/tmp/scratch",
                "--env",
                "HUTAN_SOCKET=/tmp/s.sock",
                "/Users/x/Applications/Hutan.app",
            ]
            .map(OsStr::new)
        );
        let plain = OpenApp {
            app: PathBuf::from("/Applications/Hutan.app"),
            env: Vec::new(),
        };
        assert_eq!(
            plain.args(),
            ["-n", "/Applications/Hutan.app"].map(OsString::from)
        );
    }
}
