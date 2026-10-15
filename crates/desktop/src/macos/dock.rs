//! The Dock badge (`app.dock_badge`): the number of sessions that need the user on the app's
//! Dock tile, empty at zero.

use std::cell::RefCell;
use std::rc::Rc;

/// The app's Dock tile.
pub trait DockBadge {
    /// Shows `count` as the badge; `0` clears it.
    fn set_count(&self, count: usize);
}

/// Does nothing (no Dock integration).
#[derive(Debug, Default, Clone, Copy)]
pub struct Nothing;

impl DockBadge for Nothing {
    fn set_count(&self, _count: usize) {}
}

/// Records every badge it is given.
#[derive(Debug, Clone, Default)]
pub struct FakeDock {
    counts: Rc<RefCell<Vec<usize>>>,
}

impl FakeDock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every count set so far, oldest first.
    pub fn history(&self) -> Vec<usize> {
        self.counts.borrow().clone()
    }

    /// The badge on the tile now (`0`: none).
    pub fn badge(&self) -> usize {
        self.counts.borrow().last().copied().unwrap_or(0)
    }
}

impl DockBadge for FakeDock {
    fn set_count(&self, count: usize) {
        self.counts.borrow_mut().push(count);
    }
}

/// The text of the badge label: the count, or none at zero.
pub fn badge_label(count: usize) -> Option<String> {
    (count > 0).then(|| count.to_string())
}

/// `NSApp.dockTile.badgeLabel`.
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDock;

#[cfg(target_os = "macos")]
impl DockBadge for SystemDock {
    fn set_count(&self, count: usize) {
        use objc2_app_kit::NSApplication;
        use objc2_foundation::{MainThreadMarker, NSString};
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!("the Dock badge was set off the main thread; ignored");
            return;
        };
        let label = badge_label(count).map(|text| NSString::from_str(&text));
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(label.as_deref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_is_empty_at_zero() {
        assert_eq!(badge_label(0), None);
        assert_eq!(badge_label(3).as_deref(), Some("3"));
    }
}
