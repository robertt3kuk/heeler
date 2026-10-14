//! [`TerminalSession`]: one attachment to one daemon terminal, shared by every view of it.
//!
//! The session owns the [`TerminalHandle`] (one attachment per terminal per connection), pumps
//! its updates into the [`SharedTerminal`] model that pane tabs and overview previews draw, and
//! turns what happens to the terminal (reconnect, restart, the session ending) into the
//! [`ViewStatus`] all views show. Dropping the session detaches.

use std::cell::RefCell;
use std::rc::Rc;
use std::task::Poll;

use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Task, WeakEntity};
use heeler_client::{Connection, GoneReason, TerminalHandle, TerminalUpdate};
use heeler_core::{SessionId, TerminalId};
use heeler_protocol::messages::{
    Request, SessionInfo, TerminalColors, TerminalHistory, TerminalHistoryRows, TerminalSize,
};
use heeler_terminal_view::{DefaultColors, SharedTerminal, TerminalBackend, ViewStatus};

use super::attention::ended_text;

type SharedHandle = Rc<RefCell<Option<TerminalHandle>>>;

pub(crate) fn terminal_colors(colors: DefaultColors) -> TerminalColors {
    let tuple = |c: heeler_terminal_view::Rgb| (c.0, c.1, c.2);
    TerminalColors {
        foreground: tuple(colors.foreground),
        background: tuple(colors.background),
        cursor: tuple(colors.cursor),
    }
}

/// What views of a terminal ask of the daemon, through the session's handle.
struct SessionBackend {
    handle: SharedHandle,
    connection: Connection,
    shared: WeakEntity<SharedTerminal>,
    cx: AsyncApp,
}

impl TerminalBackend for SessionBackend {
    fn input(&self, bytes: Vec<u8>) {
        if let Some(handle) = self.handle.borrow().as_ref() {
            handle.input(bytes);
        }
    }

    fn resize(&self, size: TerminalSize) {
        if let Some(handle) = self.handle.borrow().as_ref() {
            handle.resize(size);
        }
    }

    fn ack(&self, frame: u64) {
        if let Some(handle) = self.handle.borrow().as_ref() {
            handle.ack(frame);
        }
    }

    fn request_history(&self, from_line: u32, count: u32) {
        let Some(terminal) = self.handle.borrow().as_ref().map(TerminalHandle::terminal) else {
            return;
        };
        let connection = self.connection.clone();
        let shared = self.shared.clone();
        self.cx
            .spawn(async move |cx| {
                let request = Request::TerminalHistory(TerminalHistory {
                    terminal_id: terminal,
                    from_line,
                    count,
                });
                match connection.call::<TerminalHistoryRows>(request).await {
                    Ok(page) => {
                        shared
                            .update(cx, |shared, cx| {
                                shared.apply_history(page.from_line, page.rows, cx);
                            })
                            .ok();
                    }
                    Err(err) => tracing::debug!("terminal.history failed: {err}"),
                }
            })
            .detach();
    }

    fn set_default_colors(&self, colors: DefaultColors) {
        if let Some(handle) = self.handle.borrow().as_ref() {
            handle.set_colors(terminal_colors(colors));
        }
    }
}

pub struct TerminalSession {
    session: SessionId,
    shared: Entity<SharedTerminal>,
    handle: SharedHandle,
    backend: Rc<SessionBackend>,
    connection: Connection,
    info: Option<SessionInfo>,
    reconnecting: bool,
    /// Why the daemon-side terminal is unreachable for good.
    gone: Option<String>,
    colors: Option<DefaultColors>,
    _pump: Task<()>,
}

impl std::fmt::Debug for TerminalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalSession")
            .field("session", &self.session)
            .field("terminal", &self.terminal())
            .field("reconnecting", &self.reconnecting)
            .field("gone", &self.gone)
            .finish_non_exhaustive()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // Views may still hold the backend; the attachment ends with the session.
        self.handle.borrow_mut().take();
    }
}

impl TerminalSession {
    /// Attaches to the terminal of `info`; `None` when the session has no terminal (yet).
    pub fn attach(
        info: SessionInfo,
        connection: Connection,
        colors: Option<DefaultColors>,
        cx: &mut App,
    ) -> Option<Entity<Self>> {
        let terminal = info.terminal_id?;
        Some(cx.new(|cx| {
            let shared = cx.new(|_| SharedTerminal::new());
            let handle: SharedHandle = Rc::new(RefCell::new(None));
            let backend = Rc::new(SessionBackend {
                handle: Rc::clone(&handle),
                connection: connection.clone(),
                shared: shared.downgrade(),
                cx: cx.to_async(),
            });
            let mut this = Self {
                session: info.id,
                shared,
                handle,
                backend,
                connection,
                info: Some(info),
                reconnecting: false,
                gone: None,
                colors,
                _pump: Task::ready(()),
            };
            this.bind(terminal, cx);
            this
        }))
    }

    /// Opens a fresh handle on `terminal` and starts pumping it.
    fn bind(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        let handle = self.connection.attach(terminal, None);
        if let Some(colors) = self.colors {
            handle.set_colors(terminal_colors(colors));
        }
        *self.handle.borrow_mut() = Some(handle);
        let handle = Rc::clone(&self.handle);
        self._pump = cx.spawn(async move |this, cx| {
            loop {
                let update = std::future::poll_fn(|task_cx| match handle.borrow_mut().as_mut() {
                    Some(h) => h.updates().poll_recv(task_cx),
                    None => Poll::Ready(None),
                })
                .await;
                let Some(update) = update else { break };
                if this
                    .update(cx, |this, cx| this.on_update(update, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    fn on_update(&mut self, update: TerminalUpdate, cx: &mut Context<Self>) {
        match update {
            TerminalUpdate::Render(render) => {
                self.reconnecting = false;
                self.shared
                    .update(cx, |shared, cx| shared.apply_render(render, cx));
                self.refresh_status(cx);
            }
            TerminalUpdate::Reconnecting => {
                self.reconnecting = true;
                self.refresh_status(cx);
            }
            TerminalUpdate::Gone { reason } => match reason {
                GoneReason::SessionRestarted { new_terminal } => self.rebind(new_terminal, cx),
                GoneReason::EpochChanged => {
                    self.gone = Some("The daemon restarted; this session is gone".to_owned());
                    self.refresh_status(cx);
                }
                GoneReason::NotFound => {
                    self.gone = Some("The daemon no longer knows this terminal".to_owned());
                    self.refresh_status(cx);
                }
            },
        }
    }

    /// The session restarted into another terminal: start over on it. The views are told
    /// `Reconnecting` until its first snapshot arrives, which makes them report their size.
    fn rebind(&mut self, new_terminal: TerminalId, cx: &mut Context<Self>) {
        self.shared.update(cx, |shared, cx| shared.reset(cx));
        self.reconnecting = true;
        self.bind(new_terminal, cx);
        self.refresh_status(cx);
    }

    fn compute_status(&self) -> ViewStatus {
        if let Some(reason) = &self.gone {
            return ViewStatus::Ended(reason.clone());
        }
        match &self.info {
            None => return ViewStatus::Ended("The session no longer exists".to_owned()),
            Some(info) => {
                if let Some(text) = ended_text(&info.state) {
                    return ViewStatus::Ended(text);
                }
            }
        }
        if self.reconnecting {
            ViewStatus::Reconnecting
        } else {
            ViewStatus::Live
        }
    }

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let status = self.compute_status();
        self.shared
            .update(cx, |shared, cx| shared.set_status(status, cx));
        cx.notify();
    }

    /// The store's latest word on the session (`None`: it is gone from the daemon).
    pub fn update_info(&mut self, info: Option<SessionInfo>, cx: &mut Context<Self>) {
        if self.info == info {
            return;
        }
        self.info = info;
        self.refresh_status(cx);
    }

    /// The default colors the daemon answers OSC 10/11/12 queries with; remembered for rebinds.
    pub fn set_colors(&mut self, colors: DefaultColors) {
        if self.colors == Some(colors) {
            return;
        }
        self.colors = Some(colors);
        self.backend.set_default_colors(colors);
    }

    // ---- read access -----------------------------------------------------------------------

    pub fn session_id(&self) -> SessionId {
        self.session
    }

    /// The terminal currently attached, if any.
    pub fn terminal(&self) -> Option<TerminalId> {
        self.handle.borrow().as_ref().map(TerminalHandle::terminal)
    }

    pub fn info(&self) -> Option<&SessionInfo> {
        self.info.as_ref()
    }

    /// The model every view of this terminal draws.
    pub fn shared(&self) -> &Entity<SharedTerminal> {
        &self.shared
    }

    /// What views send input, resizes, acks and history requests through.
    pub fn backend(&self) -> Rc<dyn TerminalBackend> {
        self.backend.clone()
    }

    pub fn status(&self) -> ViewStatus {
        self.compute_status()
    }
}
