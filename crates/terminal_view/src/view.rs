//! The terminal view entity: state, input handling, IME, selection, scrollback viewing.

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, KeyDownEvent, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, Size,
    Styled, Subscription, Task, UTF16Selection, Window, actions, div, point, px, rgba,
};
use heeler_protocol::messages::TerminalSize;
use heeler_protocol::render::{CursorShape, MouseMode, Row, TerminalRender};

use crate::TerminalBackend;
use crate::convert;
use crate::element::TerminalElement;
use crate::keys::{KeyOptions, Mods, encode_key};
use crate::model::{Applied, HISTORY_PAGE, TerminalModel, row_cells};
use crate::mouse::{
    Button, MouseAction, MouseEvent, WheelDirection, alternate_scroll, encode_mouse,
};
use crate::paste::paste_bytes;
use crate::selection::{Point as GridPoint, Selection, SelectionKind, selected_text};
use crate::settings::{CursorStyle, TerminalViewSettings};
use crate::shape::{RowCache, hsla};
use crate::theme::{DefaultColors, Theme};

actions!(
    heeler_terminal_view,
    [
        /// Copies the selection to the clipboard.
        Copy,
        /// Pastes the clipboard into the terminal.
        Paste
    ]
);

/// Key context of the view's root element, for keymap bindings.
pub const KEY_CONTEXT: &str = "HeelerTerminal";

/// Binds ⌘C and ⌘V to [`Copy`] and [`Paste`] in the view's key context. Optional: the view also
/// handles both chords itself when no binding claims them.
pub fn bind_default_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(KEY_CONTEXT)),
    ]);
}

const RESIZE_DEBOUNCE: Duration = Duration::from_millis(50);
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Lines one wheel event may scroll.
const MAX_WHEEL_LINES: f32 = 64.0;
/// Hyperlink schemes ⌘-click opens; anything else in an OSC 8 target is untrusted.
const OPENABLE_SCHEMES: &[&str] = &["http://", "https://", "mailto:"];

/// Connection state the owner reports; non-live states disable input and show a banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewStatus {
    Live,
    Reconnecting,
    /// The session ended; the text says how.
    Ended(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalViewEvent {
    /// The terminal title changed (`None` = cleared).
    TitleChanged(Option<String>),
}

/// Grid geometry from the last layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Layout {
    pub bounds: Bounds<Pixels>,
    pub cell: Size<Pixels>,
    pub cols: u16,
    pub rows: u16,
    pub scale: f32,
}

impl Layout {
    /// Cell under `position` (window coordinates), clamped to the grid.
    pub fn cell_at(&self, position: Point<Pixels>) -> (u16, u16) {
        let x = (position.x - self.bounds.origin.x) / self.cell.width;
        let y = (position.y - self.bounds.origin.y) / self.cell.height;
        let clamp =
            |v: f32, n: u16| (v.floor().max(0.0) as u32).min(u32::from(n.max(1)) - 1) as u16;
        (clamp(y, self.rows), clamp(x, self.cols))
    }
}

#[derive(Debug, Clone)]
struct Marked {
    text: String,
    /// Selected range inside `text`, in UTF-16 units.
    selected: Range<usize>,
}

pub struct TerminalView {
    pub(crate) backend: Rc<dyn TerminalBackend>,
    pub(crate) settings: TerminalViewSettings,
    pub(crate) model: TerminalModel,
    pub(crate) theme: Theme,
    pub(crate) status: ViewStatus,
    focus_handle: FocusHandle,

    /// Lines the viewport is scrolled back into history; 0 follows the live screen.
    pub(crate) scroll_offset: u32,
    scroll_accum: f32,
    history_in_flight: HashSet<u32>,
    /// History lines from this one on do not exist (an empty page came back).
    history_end: Option<u32>,

    pub(crate) selection: Option<Selection>,
    selecting: bool,
    /// Button held while the application receives mouse reports.
    reporting: Option<Button>,
    last_motion_cell: Option<(u16, u16)>,
    pub(crate) hover_cell: Option<(u16, u16)>,
    pub(crate) cmd_held: bool,

    marked: Option<Marked>,

    pub(crate) layout: Option<Layout>,
    reported_size: Option<TerminalSize>,
    wanted_size: Option<TerminalSize>,
    resize_task: Option<Task<()>>,

    /// Newest frame painted-or-not-yet that still has to be acked.
    pub(crate) ack_due: Option<u64>,
    pub(crate) blink_on: bool,
    pub(crate) rows: RowCache,

    _subscriptions: Vec<Subscription>,
    _blink: Task<()>,
}

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView")
            .field("model", &self.model)
            .field("status", &self.status)
            .field("scroll_offset", &self.scroll_offset)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    pub fn new(
        backend: Rc<dyn TerminalBackend>,
        settings: TerminalViewSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        backend.set_default_colors(DefaultColors::from(&settings.palette));
        let theme = Theme::new(settings.palette);
        let subscriptions = vec![
            cx.on_focus_in(&focus_handle, window, |this, _window, cx| {
                this.focus_changed(true, cx);
            }),
            cx.on_focus_out(&focus_handle, window, |this, _event, _window, cx| {
                this.focus_changed(false, cx);
            }),
            cx.observe_window_activation(window, |this, _window, cx| {
                this.blink_on = true;
                cx.notify();
            }),
        ];
        let blink = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;
                let alive = this.update_in(cx, |view, window, cx| view.blink_tick(window, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            backend,
            settings,
            model: TerminalModel::new(),
            theme,
            status: ViewStatus::Live,
            focus_handle,
            scroll_offset: 0,
            scroll_accum: 0.0,
            history_in_flight: HashSet::new(),
            history_end: None,
            selection: None,
            selecting: false,
            reporting: None,
            last_motion_cell: None,
            hover_cell: None,
            cmd_held: false,
            marked: None,
            layout: None,
            reported_size: None,
            wanted_size: None,
            resize_task: None,
            ack_due: None,
            blink_on: true,
            rows: RowCache::default(),
            _subscriptions: subscriptions,
            _blink: blink,
        }
    }

    // ---- owner API -------------------------------------------------------------------------

    /// Applies a snapshot or frame from the daemon. The frame is acked once it has been painted.
    pub fn apply_render(&mut self, render: TerminalRender, cx: &mut Context<Self>) {
        let frame = match &render {
            TerminalRender::Snapshot(s) => s.frame,
            TerminalRender::Frame(f) => f.frame,
        };
        // Even a frame that could not be applied is acked, or the daemon would wait for it.
        self.ack_due = Some(frame);
        let Some(applied) = self.model.apply(render) else {
            cx.notify();
            return;
        };
        if applied.snapshot {
            self.theme.set_overrides(self.model.palette());
        }
        self.after_apply(&applied, cx);
        if applied.title_changed {
            cx.emit(TerminalViewEvent::TitleChanged(
                self.model.title().map(str::to_owned),
            ));
        }
        self.blink_on = true;
        cx.notify();
    }

    fn after_apply(&mut self, applied: &Applied, cx: &mut Context<Self>) {
        if applied.history_reset {
            self.reset_history_view();
        } else if applied.history_grew > 0 && self.scroll_offset > 0 {
            // Stay on the same lines while output scrolls into history.
            self.scroll_offset =
                (self.scroll_offset + applied.history_grew).min(self.model.history_len());
        }
        self.scroll_offset = self.scroll_offset.min(self.model.history_len());
        if applied.snapshot && applied.resized {
            self.selection = None;
            self.selecting = false;
        } else if !applied.snapshot && applied.history_grew == 0 {
            // Output rewrote lines under the selection: it no longer describes this text.
            let touched = self
                .selection
                .and_then(|s| s.span(&self.model))
                .is_some_and(|span| {
                    applied.dirty.iter().any(|row| {
                        let line = self.model.viewport_line(*row);
                        span.start.line <= line && line <= span.end.line
                    })
                });
            if touched {
                self.selection = None;
                self.selecting = false;
            }
        }
        self.ensure_history();
        let _ = cx;
    }

    fn reset_history_view(&mut self) {
        self.scroll_offset = 0;
        self.scroll_accum = 0.0;
        self.history_in_flight.clear();
        self.history_end = None;
        self.selection = None;
        self.selecting = false;
    }

    /// Delivers a `terminal.history` answer for a [`TerminalBackend::request_history`] call:
    /// `from_line` 0 is the line directly above the viewport, `rows` are newest first.
    pub fn apply_history(&mut self, from_line: u32, rows: Vec<Row>, cx: &mut Context<Self>) {
        self.history_in_flight.remove(&from_line);
        if rows.is_empty() {
            self.history_end = Some(self.history_end.map_or(from_line, |e| e.min(from_line)));
        } else {
            self.model.apply_history(from_line, rows);
        }
        // A short page leaves lines missing: ask for the rest.
        self.ensure_history();
        cx.notify();
    }

    pub fn set_status(&mut self, status: ViewStatus, cx: &mut Context<Self>) {
        if status == self.status {
            return;
        }
        if status == ViewStatus::Live {
            // Requests that were in flight when the connection dropped never answer.
            self.history_in_flight.clear();
            // The new connection has not seen our size.
            self.reported_size = None;
            self.wanted_size = None;
        }
        self.status = status;
        self.reporting = None;
        self.selecting = false;
        cx.notify();
    }

    pub fn update_settings(&mut self, settings: TerminalViewSettings, cx: &mut Context<Self>) {
        if settings == self.settings {
            return;
        }
        if settings.palette != self.settings.palette {
            self.theme.set_base(settings.palette);
            self.backend
                .set_default_colors(DefaultColors::from(&settings.palette));
        }
        self.settings = settings;
        self.blink_on = true;
        cx.notify();
    }

    // ---- accessors -------------------------------------------------------------------------

    pub fn model(&self) -> &TerminalModel {
        &self.model
    }

    pub fn status(&self) -> &ViewStatus {
        &self.status
    }

    pub fn title(&self) -> Option<&str> {
        self.model.title()
    }

    /// Columns and rows that fit the last laid-out bounds.
    pub fn grid_size(&self) -> Option<(u16, u16)> {
        self.layout.map(|l| (l.cols, l.rows))
    }

    /// Cell size in pixels from the last layout.
    pub fn cell_size(&self) -> Option<Size<Pixels>> {
        self.layout.map(|l| l.cell)
    }

    /// Lines scrolled back from the live screen.
    pub fn scroll_offset(&self) -> u32 {
        self.scroll_offset
    }

    /// Whether the cursor is in the visible phase of its blink cycle.
    pub fn cursor_blink_visible(&self) -> bool {
        self.blink_on
    }

    pub fn marked_text(&self) -> Option<&str> {
        self.marked.as_ref().map(|m| m.text.as_str())
    }

    /// The selected text, if anything is selected.
    pub fn selection_text(&self) -> Option<String> {
        let span = self.selection?.span(&self.model)?;
        let text = selected_text(&self.model, span);
        (!text.is_empty()).then_some(text)
    }

    // ---- internals shared with the element ----------------------------------------------------

    pub(crate) fn accepts_input(&self) -> bool {
        self.status == ViewStatus::Live
    }

    pub(crate) fn has_focus(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    /// Absolute line shown in grid row `row`.
    pub(crate) fn line_for_row(&self, row: u16) -> i64 {
        self.model.viewport_line(0) + i64::from(row) - i64::from(self.scroll_offset)
    }

    /// Cursor shape and whether it blinks: an application's explicit choice wins over the
    /// user's preference, which applies while the application keeps the default steady block.
    pub(crate) fn cursor_appearance(&self) -> (CursorShape, bool) {
        let cursor = self.model.cursor();
        let app_default = cursor.shape == CursorShape::Block && !cursor.blinking;
        if app_default {
            let shape = match self.settings.cursor.style {
                CursorStyle::Block => CursorShape::Block,
                CursorStyle::Bar => CursorShape::Bar,
                CursorStyle::Underline => CursorShape::Underline,
            };
            (shape, self.settings.cursor.blink)
        } else {
            (cursor.shape, cursor.blinking)
        }
    }

    pub(crate) fn link_at(&self, row: u16, col: u16) -> Option<u32> {
        let row = self.model.row_at(self.line_for_row(row))?;
        row_cells(row)
            .iter()
            .find(|c| c.col <= col && col < c.end())
            .and_then(|c| c.run.link)
    }

    pub(crate) fn hover_link(&self) -> Option<u32> {
        let (row, col) = self.hover_cell?;
        self.link_at(row, col)
    }

    pub(crate) fn marked_for_paint(&self) -> Option<&str> {
        self.marked.as_ref().map(|m| m.text.as_str())
    }

    /// Takes the frame to ack; the element schedules the ack for the next frame.
    pub(crate) fn take_ack(&mut self) -> Option<u64> {
        self.ack_due.take()
    }

    pub(crate) fn send_ack(&self, frame: u64) {
        self.backend.ack(frame);
    }

    /// Records the laid-out grid and reports its size to the backend: at once the first time,
    /// debounced afterwards.
    pub(crate) fn update_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        self.layout = Some(layout);
        let pixels = |v: Pixels| {
            (f32::from(v) * layout.scale)
                .round()
                .clamp(0.0, f32::from(u16::MAX)) as u16
        };
        let size = TerminalSize {
            cols: layout.cols,
            rows: layout.rows,
            pixel_width: pixels(layout.bounds.size.width),
            pixel_height: pixels(layout.bounds.size.height),
        };
        if self.wanted_size == Some(size) {
            return;
        }
        self.wanted_size = Some(size);
        if self.reported_size.is_none() {
            self.resize_task = None;
            self.report_size(size);
        } else if self.reported_size == Some(size) {
            self.resize_task = None;
        } else {
            self.resize_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RESIZE_DEBOUNCE).await;
                this.update(cx, |view, _| {
                    view.resize_task = None;
                    view.report_size(size);
                })
                .ok();
            }));
        }
    }

    fn report_size(&mut self, size: TerminalSize) {
        if self.reported_size == Some(size) {
            return;
        }
        self.reported_size = Some(size);
        self.backend.resize(size);
    }

    // ---- focus and blink -------------------------------------------------------------------

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.blink_on = true;
        if self.model.modes().focus_events && self.accepts_input() {
            self.backend.input(if focused {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            });
        }
        cx.notify();
    }

    fn blink_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (_, blinks) = self.cursor_appearance();
        if blinks && self.has_focus(window) {
            self.blink_on = !self.blink_on;
            cx.notify();
        } else if !self.blink_on {
            self.blink_on = true;
            cx.notify();
        }
    }

    // ---- keyboard --------------------------------------------------------------------------

    fn key_options(&self) -> KeyOptions {
        let modes = self.model.modes();
        KeyOptions {
            app_cursor: modes.app_cursor,
            app_keypad: modes.app_keypad,
            kitty_flags: modes.kitty_keyboard,
            option_as_meta: self.settings.option_as_meta.enabled(),
        }
    }

    /// Sends bytes the user produced (typing, paste) to the application.
    fn send_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if !self.accepts_input() || bytes.is_empty() {
            return;
        }
        if self.settings.scroll_to_bottom_on_input && self.scroll_offset != 0 {
            self.scroll_offset = 0;
            self.scroll_accum = 0.0;
        }
        self.selection = None;
        self.selecting = false;
        self.blink_on = true;
        self.backend.input(bytes);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = ks.modifiers;
        if m.platform {
            if !m.control && !m.alt {
                match ks.key.as_str() {
                    "c" if !m.shift => {
                        self.copy(cx);
                        cx.stop_propagation();
                    }
                    "v" => {
                        self.paste(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }
            return;
        }
        if !self.accepts_input() {
            return;
        }
        if m.shift && !m.control && !m.alt && !self.model.modes().alt_screen {
            let page =
                i32::from(self.layout.map_or(self.model.screen_rows(), |l| l.rows)).max(2) - 1;
            let handled = match ks.key.as_str() {
                "pageup" => Some(page),
                "pagedown" => Some(-page),
                "home" => Some(i32::MAX / 2),
                "end" => Some(i32::MIN / 2),
                _ => None,
            };
            if let Some(lines) = handled {
                self.scroll_lines(lines, cx);
                cx.stop_propagation();
                return;
            }
        }
        let Some(input) = convert::key_input(ks) else {
            return;
        };
        if let Some(bytes) = encode_key(&input, self.key_options()) {
            self.send_input(bytes, cx);
            cx.stop_propagation();
        }
    }

    // ---- clipboard -------------------------------------------------------------------------

    /// Copies the selection; returns whether there was one. Never touches the PTY.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        match self.selection_text() {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            None => false,
        }
    }

    /// Pastes the clipboard text, bracketed when the application enabled it.
    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bytes = paste_bytes(&text, self.model.modes().bracketed_paste);
        self.send_input(bytes, cx);
    }

    // ---- scrollback ------------------------------------------------------------------------

    /// Scrolls the viewport by `lines` (positive = towards older output).
    pub fn scroll_lines(&mut self, lines: i32, cx: &mut Context<Self>) {
        let max = i64::from(self.model.history_len());
        let new = (i64::from(self.scroll_offset) + i64::from(lines)).clamp(0, max);
        let new = u32::try_from(new).unwrap_or(0);
        if new != self.scroll_offset {
            self.scroll_offset = new;
            self.ensure_history();
            cx.notify();
        }
    }

    pub fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        if self.scroll_offset != 0 {
            self.scroll_offset = 0;
            self.scroll_accum = 0.0;
            cx.notify();
        }
    }

    /// Requests the first missing history page the viewport needs.
    fn ensure_history(&mut self) {
        if self.scroll_offset == 0 {
            return;
        }
        let rows = u32::from(self.layout.map_or(self.model.screen_rows(), |l| l.rows)).max(1);
        let hi = self.scroll_offset - 1;
        let lo = self.scroll_offset.saturating_sub(rows);
        let Some(missing) = self.model.first_missing_history(lo, hi - lo + 1) else {
            return;
        };
        if self.history_end.is_some_and(|end| missing >= end)
            || !self.history_in_flight.insert(missing)
        {
            return;
        }
        self.backend.request_history(missing, HISTORY_PAGE);
    }

    // ---- mouse -----------------------------------------------------------------------------

    fn report_mouse(&mut self, action: MouseAction, mods: Mods, row: u16, col: u16) -> bool {
        let modes = self.model.modes();
        let event = MouseEvent {
            action,
            mods,
            col,
            row,
        };
        match encode_mouse(&event, modes.mouse, modes.mouse_sgr) {
            Some(bytes) => {
                self.backend.input(bytes);
                true
            }
            None => false,
        }
    }

    fn captures_mouse(&self, mods: gpui::Modifiers) -> bool {
        self.model.modes().mouse != MouseMode::None && !mods.shift && self.accepts_input()
    }

    pub(crate) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.layout else { return };
        window.focus(&self.focus_handle, cx);
        let (row, col) = layout.cell_at(event.position);
        if event.button == gpui::MouseButton::Left
            && event.modifiers.platform
            && let Some(id) = self.link_at(row, col)
            && let Some(uri) = self.model.link(id)
            && OPENABLE_SCHEMES.iter().any(|s| uri.starts_with(s))
        {
            cx.open_url(uri);
            return;
        }
        if self.captures_mouse(event.modifiers) {
            if let Some(button) = convert::button(event.button) {
                self.reporting = Some(button);
                self.last_motion_cell = Some((row, col));
                self.report_mouse(
                    MouseAction::Press(button),
                    convert::mods(event.modifiers),
                    row,
                    col,
                );
            }
            return;
        }
        if event.button != gpui::MouseButton::Left {
            return;
        }
        let kind = match event.click_count {
            0 | 1 => SelectionKind::Simple,
            2 => SelectionKind::Word,
            _ => SelectionKind::Line,
        };
        let at = GridPoint {
            line: self.line_for_row(row),
            col,
        };
        self.selection = Some(Selection::new(kind, at, &self.model));
        self.selecting = true;
        cx.notify();
    }

    /// `inside`: the pointer is over the terminal (drags continue outside it).
    pub(crate) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        inside: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.layout else { return };
        let (row, col) = layout.cell_at(event.position);
        let hover = inside.then_some((row, col));
        let cmd = event.modifiers.platform;
        if hover != self.hover_cell || cmd != self.cmd_held {
            let had_link = self.hover_link().is_some();
            self.hover_cell = hover;
            self.cmd_held = cmd;
            if had_link || self.hover_link().is_some() {
                cx.notify();
            }
        }
        let mods = convert::mods(event.modifiers);
        if let Some(button) = self.reporting {
            if self.last_motion_cell != Some((row, col)) {
                self.last_motion_cell = Some((row, col));
                self.report_mouse(MouseAction::Motion(Some(button)), mods, row, col);
            }
            return;
        }
        if self.selecting && event.pressed_button == Some(gpui::MouseButton::Left) {
            let top = layout.bounds.origin.y;
            let bottom = top + layout.bounds.size.height;
            if event.position.y < top {
                self.scroll_lines(1, cx);
            } else if event.position.y > bottom {
                self.scroll_lines(-1, cx);
            }
            let at = GridPoint {
                line: self.line_for_row(row),
                col,
            };
            if let Some(selection) = &mut self.selection {
                selection.extend(at);
                cx.notify();
            }
            return;
        }
        if inside
            && event.pressed_button.is_none()
            && self.model.modes().mouse == MouseMode::Motion
            && self.captures_mouse(event.modifiers)
            && self.last_motion_cell != Some((row, col))
        {
            self.last_motion_cell = Some((row, col));
            self.report_mouse(MouseAction::Motion(None), mods, row, col);
        }
    }

    pub(crate) fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let (row, col) = layout.cell_at(event.position);
        if let Some(button) = self.reporting.take() {
            self.last_motion_cell = None;
            self.report_mouse(
                MouseAction::Release(button),
                convert::mods(event.modifiers),
                row,
                col,
            );
            return;
        }
        if !self.selecting {
            return;
        }
        self.selecting = false;
        if self.selection.and_then(|s| s.span(&self.model)).is_none() {
            self.selection = None;
        } else if self.settings.copy_on_select {
            self.copy(cx);
        }
        cx.notify();
    }

    pub(crate) fn scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let delta = match event.delta {
            ScrollDelta::Pixels(p) => p.y / layout.cell.height,
            ScrollDelta::Lines(l) => l.y,
        };
        if delta == 0.0 {
            return;
        }
        if (delta > 0.0) != (self.scroll_accum > 0.0) {
            self.scroll_accum = 0.0;
        }
        self.scroll_accum = (self.scroll_accum + delta).clamp(-MAX_WHEEL_LINES, MAX_WHEEL_LINES);
        let lines = self.scroll_accum.trunc();
        if lines == 0.0 {
            return;
        }
        self.scroll_accum -= lines;
        let count = lines.abs() as usize;
        let up = lines > 0.0;
        let modes = self.model.modes();
        let (row, col) = layout.cell_at(event.position);

        if self.captures_mouse(event.modifiers) && self.scroll_offset == 0 {
            let direction = if up {
                WheelDirection::Up
            } else {
                WheelDirection::Down
            };
            let mods = convert::mods(event.modifiers);
            for _ in 0..count {
                self.report_mouse(MouseAction::Wheel(direction), mods, row, col);
            }
            return;
        }
        if modes.alt_screen {
            if modes.alternate_scroll && !event.modifiers.shift && self.accepts_input() {
                self.backend
                    .input(alternate_scroll(up, count, modes.app_cursor));
            }
            return;
        }
        let signed = i32::try_from(count).unwrap_or(i32::MAX);
        self.scroll_lines(if up { signed } else { -signed }, cx);
    }

    pub(crate) fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        cx: &mut Context<Self>,
    ) {
        if event.modifiers.platform != self.cmd_held {
            self.cmd_held = event.modifiers.platform;
            if self.hover_link().is_some() {
                cx.notify();
            }
        }
    }
}

// ---- text input (IME) ---------------------------------------------------------------------

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Byte range of `s` for a UTF-16 range, clamped to the string.
fn byte_range(s: &str, range: &Range<usize>) -> Range<usize> {
    let mut units = 0;
    let mut start = s.len();
    let mut end = s.len();
    for (i, c) in s.char_indices() {
        if units >= range.start && start == s.len() {
            start = i;
        }
        if units >= range.end {
            end = i;
            break;
        }
        units += c.len_utf16();
    }
    start..end.max(start)
}

impl gpui::EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let marked = self.marked.as_ref()?;
        let bytes = byte_range(&marked.text, &range);
        *adjusted_range =
            Some(utf16_len(&marked.text[..bytes.start])..utf16_len(&marked.text[..bytes.end]));
        Some(marked.text[bytes].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.marked.as_ref().map_or(0..0, |m| m.selected.clone()),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..utf16_len(&m.text))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The terminal has no document to edit: committing text (also the end of a composition)
        // sends it to the application.
        self.marked = None;
        self.send_input(text.as_bytes().to_vec(), cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_input() {
            return;
        }
        self.marked = (!new_text.is_empty()).then(|| {
            let len = utf16_len(new_text);
            Marked {
                text: new_text.to_owned(),
                selected: new_selected_range
                    .map(|r| r.start.min(len)..r.end.min(len))
                    .unwrap_or(len..len),
            }
        });
        self.blink_on = true;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout?;
        let cursor = self.model.cursor();
        // Columns the marked text covers before the requested range starts.
        let skipped = self.marked.as_ref().map_or(0, |m| {
            let bytes = byte_range(&m.text, &range_utf16);
            crate::shape::text_columns(&m.text[..bytes.start])
        });
        let col = f32::from(cursor.col) + skipped as f32;
        let origin = point(
            element_bounds.origin.x + layout.cell.width * col,
            element_bounds.origin.y + layout.cell.height * f32::from(cursor.row),
        );
        Some(Bounds::new(origin, layout.cell))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.accepts_input()
    }
}

// ---- rendering ----------------------------------------------------------------------------

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = hsla(self.theme.background());
        let banner = match &self.status {
            ViewStatus::Live => None,
            ViewStatus::Reconnecting => Some("Reconnecting…".to_owned()),
            ViewStatus::Ended(message) => Some(message.clone()),
        };
        let root = div()
            .id("heeler-terminal")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|this, _: &Copy, _window, cx| {
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &Paste, _window, cx| this.paste(cx)))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(background)
            .child(TerminalElement::new(cx.entity()));
        match banner {
            None => root,
            Some(text) => root.child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .px(px(10.))
                    .py(px(4.))
                    .bg(rgba(0x000000b3))
                    .text_color(hsla(self.theme.foreground()))
                    .text_size(px(12.))
                    .child(text),
            ),
        }
    }
}
