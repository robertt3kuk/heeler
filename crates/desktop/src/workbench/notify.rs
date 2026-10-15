//! `ui.open` (protocol 0.13): the daemon asks this window to show a session, because the user
//! chose "Open" on a notification intent.

use gpui::{Context, Window};

use super::view::Workbench;
use crate::sessions::UiOpenRequested;

impl Workbench {
    /// Brings the window forward and focuses the session's tab (its workspace first), with the
    /// same item machinery as every other way of opening a session. A session that is gone or
    /// no longer live is not opened; the window says so instead.
    pub(crate) fn on_ui_open(
        &mut self,
        event: &UiOpenRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let live = self
            .sessions
            .read(cx)
            .session(event.session_id)
            .map(|info| info.state.is_live());
        match live {
            Some(true) => {
                window.activate_window();
                self.open_session(event.session_id, window, cx);
            }
            Some(false) => {
                window.activate_window();
                self.set_notice("That session is no longer running", cx);
            }
            None => self.set_notice("That session no longer exists", cx),
        }
    }
}
