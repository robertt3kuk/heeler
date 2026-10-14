//! Workbench behavior around live sessions: terminal items, session lifecycle commands, focus
//! and attention, and the hot-reload reactions of settings and keymap.

use crate::theme::DensityExt as _;
use std::collections::HashSet;
use std::path::PathBuf;

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, Focusable as _,
    InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use heeler_core::SessionId;
use heeler_protocol::messages::SessionInfo;
use heeler_settings::{ChangeSet, KEYMAP_NAMESPACE};
use heeler_terminal_view::{DefaultColors, TerminalView, ViewMode};

use super::actions;
use super::modal::PromptSpec;
use super::model::{Edge, FocusTarget, ItemRef, PaneId};
use super::registry::StatusBarLayout;
use super::style::{TerminalStyle, UiStyle};
use super::view::{MenuEntry, Workbench, system_appearance};
use crate::sessions::TerminalSession;
use crate::settings::{AppearanceSettings, LayoutSettings, TerminalSettings};
use crate::theme::Theme;

/// A terminal tab's view; the attachment it draws from lives in [`Workbench::held`].
#[derive(Debug)]
pub(crate) struct TerminalItem {
    pub view: Entity<TerminalView>,
}

/// The session an item shows.
pub fn session_of_item(item: &ItemRef) -> Option<SessionId> {
    match item {
        ItemRef::Terminal { session } | ItemRef::Chat { session } => session.parse().ok(),
        _ => None,
    }
}

impl Workbench {
    // ---- items ------------------------------------------------------------------------------

    /// Every `(pane, session)` that has a terminal tab, in tree order.
    fn open_terminal_items(&self) -> Vec<(PaneId, SessionId)> {
        self.state
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| {
                pane.items.iter().filter_map(move |item| match item {
                    ItemRef::Terminal { session } => {
                        session.parse().ok().map(|session| (pane.id, session))
                    }
                    _ => None,
                })
            })
            .collect()
    }

    /// The session of the focused item (a terminal tab in the focused pane), if any.
    pub fn focused_session(&self) -> Option<SessionId> {
        match self.state.focus {
            FocusTarget::Pane(_) => self
                .state
                .active_pane()
                .and_then(|p| p.active_item())
                .and_then(session_of_item),
            FocusTarget::Dock(_) => None,
        }
    }

    /// Attaches every open terminal item whose session exists and is not attached yet (restoring
    /// a layout re-attaches all of them), and detaches the ones no tab shows anymore.
    pub(crate) fn attach_open_items(&mut self, cx: &mut Context<Self>) {
        let items = self.open_terminal_items();
        // Sessions hidden in the overview's stash stay attached (paced at zero cost).
        let mut wanted: HashSet<SessionId> = items.iter().map(|(_, s)| *s).collect();
        if let Some(saved) = &self.state.stashed {
            wanted.extend(
                saved
                    .center
                    .panes()
                    .into_iter()
                    .flat_map(|p| p.items.iter().filter_map(session_of_item)),
            );
        }
        self.held.retain(|session, _| wanted.contains(session));
        let keep: HashSet<(PaneId, SessionId)> = items.into_iter().collect();
        let gone: Vec<(PaneId, SessionId)> = self
            .terminals
            .keys()
            .filter(|key| !keep.contains(*key))
            .copied()
            .collect();
        for key in gone {
            if let Some(item) = self.terminals.remove(&key) {
                // Focus may still sit on it; `rescue_orphaned_focus` checks before it is dropped.
                self.pruned.push(item.view);
            }
        }
        for session in wanted {
            self.ensure_held(session, cx);
        }
    }

    fn ensure_held(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalSession>> {
        if let Some(held) = self.held.get(&session) {
            return Some(held.clone());
        }
        let attached = self
            .sessions
            .update(cx, |store, cx| store.terminal_session(session, cx))?;
        self.held.insert(session, attached.clone());
        Some(attached)
    }

    /// The view of `session` in `pane`, created on first use.
    pub(crate) fn terminal_view(
        &mut self,
        pane: PaneId,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalView>> {
        if let Some(item) = self.terminals.get(&(pane, session)) {
            return Some(item.view.clone());
        }
        let attached = self.ensure_held(session, cx)?;
        let (shared, backend) = {
            let attached = attached.read(cx);
            (attached.shared().clone(), attached.backend())
        };
        let settings = self.terminal_style.clone();
        let view = cx.new(|cx| {
            TerminalView::with_shared(shared, backend, settings, ViewMode::Interactive, window, cx)
        });
        self.terminals
            .insert((pane, session), TerminalItem { view: view.clone() });
        Some(view)
    }

    /// The view of the focused pane's active tab, if it is a terminal.
    pub fn focused_terminal_view(&self) -> Option<Entity<TerminalView>> {
        let FocusTarget::Pane(_) = self.state.focus else {
            return None;
        };
        let pane = self.state.active_pane_id();
        let session = self.focused_session()?;
        self.terminals.get(&(pane, session)).map(|t| t.view.clone())
    }

    /// The view of `session` in any pane (tests, inspection).
    pub fn terminal_view_of(&self, session: SessionId) -> Option<Entity<TerminalView>> {
        self.terminals
            .iter()
            .find(|((_, s), _)| *s == session)
            .map(|(_, t)| t.view.clone())
    }

    /// Whether `session` is attached because some tab holds it.
    pub fn holds(&self, session: SessionId) -> bool {
        self.held.contains_key(&session)
    }

    /// Moves keyboard focus to what the focused pane shows: its terminal, else the workbench.
    pub(crate) fn focus_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let FocusTarget::Pane(_) = self.state.focus
            && let Some(session) = self.focused_session()
        {
            let pane = self.state.active_pane_id();
            if let Some(view) = self.terminal_view(pane, session, window, cx) {
                window.focus(&view.focus_handle(cx), cx);
                return;
            }
        }
        window.focus(&self.focus_handle, cx);
    }

    /// A terminal view that had focus but is no longer drawn (its tab was switched away, its pane
    /// closed, the overview replaced the center) leaves keystrokes without a dispatch path.
    /// Moves focus to whatever the focused pane shows now.
    pub(crate) fn rescue_orphaned_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(focused) = window.focused(cx) else {
            return;
        };
        let orphaned = self.terminals.iter().any(|(key, item)| {
            !self.rendered_terminals.contains(key) && item.view.focus_handle(cx) == focused
        }) || self
            .pruned
            .iter()
            .any(|view| view.focus_handle(cx) == focused);
        self.pruned.clear();
        if orphaned {
            self.focus_root(window, cx);
        }
    }

    // ---- open / new -------------------------------------------------------------------------

    /// Shows `session` in the focused pane: focuses its tab when open, else adds a tab (a pane
    /// that only holds a placeholder gives the placeholder's place to the session).
    pub fn open_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = ItemRef::Terminal {
            session: session.to_string(),
        };
        if let Some((pane, index)) = self.state.find_item(&item) {
            self.try_mutate(cx, |s| {
                s.set_active_item(pane, index)?;
                s.focus_pane(pane)
            });
        } else {
            let pane = self.state.active_pane_id();
            let placeholder = self
                .state
                .pane(pane)
                .and_then(|p| match p.items.as_slice() {
                    [only @ ItemRef::Placeholder { .. }] => Some(only.clone()),
                    _ => None,
                });
            self.try_mutate(cx, |s| {
                if let Some(placeholder) = &placeholder
                    && s.replace_item(placeholder, item.clone())
                {
                    return s.focus_pane(pane);
                }
                s.add_item(pane, item.clone()).map(|_| ())
            });
        }
        self.focus_root(window, cx);
    }

    /// Opens `session` in a new split at `edge` of the focused pane.
    pub fn open_session_in_split(
        &mut self,
        session: SessionId,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.state.active_pane_id();
        let item = ItemRef::Terminal {
            session: session.to_string(),
        };
        self.try_mutate(cx, |s| s.split_pane(pane, edge, item));
        self.focus_root(window, cx);
    }

    /// Leaves the overview (restoring the layout it replaced) and focuses `session`.
    pub fn open_session_from_overview(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.preset == crate::workbench::model::LayoutPreset::OVERVIEW {
            self.toggle_overview(cx);
        }
        self.open_session(session, window, cx);
    }

    /// Starts a terminal in the focused session's workspace (the default one otherwise), in the
    /// workspace root or `$HOME`, and opens it.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (workspace, cwd) = {
            let store = self.sessions.read(cx);
            let workspace = self
                .focused_session()
                .and_then(|s| store.session(s))
                .and_then(|s| store.workspace(s.workspace_id))
                .or_else(|| store.workspaces().first());
            let cwd = workspace
                .and_then(|w| w.roots.first().cloned())
                .or_else(std::env::home_dir);
            (workspace.map(|w| w.id), cwd)
        };
        let task = self
            .sessions
            .update(cx, |store, cx| store.create_terminal(workspace, cwd, cx));
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(info) => {
                this.update_in(cx, |this, window, cx| {
                    this.open_session(info.id, window, cx)
                })
                .ok();
            }
            Err(err) => {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot start a terminal: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Where a terminal started from `pane` begins: the pane's terminal session's current
    /// directory (it follows OSC 7) in its workspace; for any other item the default
    /// workspace's root, else `$HOME`.
    fn split_target(
        &self,
        pane: PaneId,
        cx: &gpui::App,
    ) -> (Option<heeler_core::WorkspaceId>, Option<PathBuf>) {
        let store = self.sessions.read(cx);
        let home_or_root = |workspace: Option<&heeler_protocol::messages::WorkspaceInfo>| {
            workspace
                .and_then(|w| w.roots.first().cloned())
                .or_else(std::env::home_dir)
        };
        let info = self
            .state
            .pane(pane)
            .and_then(|p| p.active_item())
            .and_then(session_of_item)
            .and_then(|id| store.session(id));
        match info {
            Some(info) => {
                let workspace = store.workspace(info.workspace_id);
                (
                    Some(info.workspace_id),
                    info.cwd.clone().or_else(|| home_or_root(workspace)),
                )
            }
            None => {
                let workspace = store.workspaces().first();
                (workspace.map(|w| w.id), home_or_root(workspace))
            }
        }
    }

    /// Splits `pane` at `edge` and starts a new terminal in the new pane (iTerm / Ghostty
    /// style). Without a daemon connection the new pane is an empty placeholder and a toast says
    /// why.
    pub fn split_pane_with_terminal(
        &mut self,
        pane: PaneId,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.read(cx).connection_state() != heeler_client::ConnectionState::Connected {
            self.split_with_placeholder(pane, edge, cx);
            self.set_notice("Not connected to heelerd: opened an empty pane", cx);
            return;
        }
        let (workspace, cwd) = self.split_target(pane, cx);
        let task = self
            .sessions
            .update(cx, |store, cx| store.create_terminal(workspace, cwd, cx));
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(info) => {
                this.update_in(cx, |this, window, cx| {
                    // The pane may have closed meanwhile; then split the focused one.
                    let pane = if this.state.pane(pane).is_some() {
                        pane
                    } else {
                        this.state.active_pane_id()
                    };
                    if this.state.focus != FocusTarget::Pane(pane) {
                        this.try_mutate(cx, |s| s.focus_pane(pane));
                    }
                    this.open_session_in_split(info.id, edge, window, cx);
                })
                .ok();
            }
            Err(err) => {
                this.update(cx, |this, cx| {
                    if this.state.pane(pane).is_some() {
                        this.split_with_placeholder(pane, edge, cx);
                    }
                    this.set_notice(format!("Cannot start a terminal: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- lifecycle --------------------------------------------------------------------------

    fn info_of(&self, session: SessionId, cx: &gpui::App) -> Option<SessionInfo> {
        self.sessions.read(cx).session(session).cloned()
    }

    fn report(
        &mut self,
        what: &'static str,
        task: gpui::Task<Result<SessionInfo, String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            if let Err(err) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot {what}: {err}"), cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Stops `session`; asks first while it is running.
    pub fn stop_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.info_of(session, cx) else {
            return;
        };
        if !info.state.is_live() {
            self.set_notice("That session already ended", cx);
            return;
        }
        let title = info.title.clone();
        self.open_confirm(
            "Stop session?",
            format!(
                "“{title}” is running. Stopping ends its process; closing the tab only detaches."
            ),
            "Stop",
            move |this, window, cx| {
                let task = this
                    .sessions
                    .update(cx, |store, cx| store.stop(session, cx));
                this.report("stop the session", task, window, cx);
            },
            window,
            cx,
        );
    }

    pub fn restart_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.restart(session, cx));
        self.report("restart the session", task, window, cx);
    }

    pub fn rename_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.info_of(session, cx) else {
            return;
        };
        self.open_prompt(
            PromptSpec {
                title: "Rename session",
                message: "New title:",
                initial: &info.title,
                confirm_label: "Rename",
            },
            move |this, title, window, cx| {
                let task = this.sessions.update(cx, |store, cx| {
                    store.rename(session, title.trim().to_owned(), cx)
                });
                this.report("rename the session", task, window, cx);
            },
            window,
            cx,
        );
    }

    pub fn copy_session_id(&mut self, session: SessionId, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(session.to_string()));
        self.set_notice("Copied the session id", cx);
    }

    /// Focuses the next session that needs the user.
    pub fn jump_to_next_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.focused_session();
        let next = self.sessions.read(cx).next_needing_attention(current);
        match next {
            Some(session) => self.open_session(session, window, cx),
            None => self.set_notice("Nothing needs you", cx),
        }
    }

    /// Context menu of a session (sessions panel, overview card, tab).
    pub(crate) fn session_menu(&self, session: SessionId) -> Vec<MenuEntry> {
        vec![
            Self::entry("Open", move |this, window, cx| {
                this.open_session(session, window, cx);
            }),
            Self::entry("Open in Split Right", move |this, window, cx| {
                this.open_session_in_split(session, Edge::Right, window, cx);
            }),
            Self::entry("Open in Split Down", move |this, window, cx| {
                this.open_session_in_split(session, Edge::Bottom, window, cx);
            }),
            Self::entry("Rename…", move |this, window, cx| {
                this.rename_session(session, window, cx);
            }),
            Self::entry("Restart", move |this, window, cx| {
                this.restart_session(session, window, cx);
            }),
            Self::entry("Stop…", move |this, window, cx| {
                this.stop_session(session, window, cx);
            }),
            Self::entry("Copy Session ID", move |this, _, cx| {
                this.copy_session_id(session, cx);
            }),
        ]
    }

    // ---- attention & focus ------------------------------------------------------------------

    /// Tells the sessions store which session the user is looking at (a focused terminal tab in
    /// an active window): that session's unread attention is read, and its workspace's settings
    /// apply.
    pub(crate) fn sync_session_focus(&mut self, cx: &mut Context<Self>) {
        let focused = if self.window_active {
            self.focused_session()
        } else {
            None
        };
        self.sessions.update(cx, |store, cx| {
            store.set_focused(focused, cx);
            if let Some(session) = focused {
                store.mark_read(session, cx);
            }
        });
        if let Some(session) = self.focused_session() {
            let root = {
                let store = self.sessions.read(cx);
                store
                    .session(session)
                    .and_then(|s| store.workspace(s.workspace_id))
                    .and_then(|w| w.roots.first().cloned())
            };
            if root != self.workspace_root {
                self.workspace_root = root.clone();
                self.settings.set_workspace_root(root);
            }
        }
    }

    pub(crate) fn on_sessions_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.held.len();
        self.attach_open_items(cx);
        self.sync_session_focus(cx);
        let nothing_focused = window.focused(cx).is_none() || self.focus_handle.is_focused(window);
        if nothing_focused && self.held.len() != before {
            self.focus_root(window, cx);
        }
        cx.notify();
    }

    /// The workspace root whose settings currently apply, if any.
    pub fn workspace_root(&self) -> Option<&PathBuf> {
        self.workspace_root.as_ref()
    }

    // ---- rendering --------------------------------------------------------------------------

    /// Body of a terminal tab: the terminal view with Restart / Close when it ended, or a
    /// placeholder while the session is unknown.
    pub(crate) fn render_terminal_item(
        &mut self,
        pane: PaneId,
        session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let Ok(id) = session.parse::<SessionId>() else {
            return self.placeholder_body(format!("Invalid session id {session}"), cx);
        };
        let (known, synced, live) = {
            let store = self.sessions.read(cx);
            let info = store.session(id);
            (
                info.is_some(),
                store.synced(),
                info.is_some_and(|i| i.state.is_live()),
            )
        };
        self.rendered_terminals.insert((pane, id));
        let Some(view) = self.terminal_view(pane, id, window, cx) else {
            let text = if known {
                "Starting…"
            } else if synced {
                "This session no longer exists. Close the tab."
            } else {
                "Connecting to heelerd…"
            };
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(tokens.spacing * 2.0))
                .text_color(theme.muted)
                .child(text)
                .when(!known && synced, |s| {
                    s.child(
                        self.action_button("ended-close", "Close", cx, move |this, _, cx| {
                            this.close_session_tabs(id, cx);
                        }),
                    )
                })
                .into_any_element();
        };
        let ended = self.held.get(&id).is_some_and(|h| {
            matches!(
                h.read(cx).status(),
                heeler_terminal_view::ViewStatus::Ended(_)
            )
        });
        let mut root = div().relative().size_full().child(view);
        if ended {
            let mut bar = div()
                .absolute()
                .top(px(tokens.spacing * 2.0))
                .right(px(tokens.spacing * 3.0))
                .flex()
                .gap(px(tokens.spacing * 2.0));
            if known && !live {
                bar = bar.child(self.action_button(
                    "ended-restart",
                    "Restart",
                    cx,
                    move |this, window, cx| {
                        this.restart_session(id, window, cx);
                    },
                ));
            }
            bar = bar.child(
                self.action_button("ended-close", "Close", cx, move |this, _, cx| {
                    this.close_session_tabs(id, cx);
                }),
            );
            root = root.child(bar);
        }
        root.into_any_element()
    }

    fn action_button(
        &self,
        id: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
        run: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.hover;
        div()
            .id(id)
            .px(px(tokens.spacing * 3.0))
            .h(px(tokens.sidebar_row))
            .flex()
            .items_center()
            .rounded(px(tokens.radius - 1.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.overlay)
            .text_color(theme.fg)
            .text_size(px(tokens.font_size))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
            .child(SharedString::from(label))
    }

    /// Closes (detaches) every tab showing `session`.
    pub(crate) fn close_session_tabs(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let item = ItemRef::Terminal {
            session: session.to_string(),
        };
        while let Some((pane, index)) = self.state.find_item(&item) {
            if self.try_mutate(cx, |s| s.close_item(pane, index)).is_none() {
                break;
            }
        }
    }

    // ---- hot reload -------------------------------------------------------------------------

    /// Re-reads the keymap (defaults + `keymap.toml`) and rebinds every key.
    pub(crate) fn rebind_keys(&mut self, cx: &mut Context<Self>) {
        let load = self.settings.keymap(&actions::default_bindings());
        actions::apply_keymap(cx, &load);
        cx.notify();
    }

    /// The status bar of the current preset.
    pub fn status_bar(&self) -> StatusBarLayout {
        self.preset_defs
            .iter()
            .find(|d| d.layout.name == self.state.preset)
            .map(|d| d.status_bar.clone())
            .unwrap_or_default()
    }

    /// Recomputes the terminal view settings (theme resolved against the system appearance) and
    /// pushes them to every view; the daemon learns the new default colors for every attached
    /// terminal.
    pub(crate) fn refresh_terminal_style(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.get::<TerminalSettings>();
        let style = heeler_terminal_view::TerminalViewSettings {
            padding: self.density.ui().terminal_padding,
            ..settings.view_settings(system_appearance(self.appearance))
        };
        self.terminal_settings = settings;
        if style == self.terminal_style {
            return;
        }
        self.terminal_style = style.clone();
        cx.set_global(TerminalStyle(style.clone()));
        for item in self.terminals.values() {
            item.view
                .update(cx, |view, cx| view.update_settings(style.clone(), cx));
        }
        let colors = DefaultColors::from(&style.palette);
        self.sessions
            .update(cx, |store, cx| store.set_terminal_colors(colors, cx));
        cx.notify();
    }

    pub(crate) fn on_settings_changed(
        &mut self,
        changes: &ChangeSet,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if changes.contains(KEYMAP_NAMESPACE) {
            self.rebind_keys(cx);
        }
        if changes.contains("appearance") {
            let appearance = self.settings.get::<AppearanceSettings>();
            self.chrome = appearance.chrome;
            self.density = appearance.density;
            self.theme = Theme::for_chrome(self.chrome, self.appearance);
            self.publish_style(cx);
            self.refresh_terminal_style(cx);
        }
        if changes.contains("terminal") {
            self.refresh_terminal_style(cx);
        }
        if changes.contains("layout") {
            self.apply_layout_settings(cx);
        }
        cx.notify();
    }

    fn apply_layout_settings(&mut self, cx: &mut Context<Self>) {
        let layout = self.settings.get::<LayoutSettings>();
        let previous = std::mem::replace(&mut self.layout_seen, layout.clone());
        let before = self
            .preset_defs
            .iter()
            .find(|d| d.layout.name == self.state.preset)
            .cloned();
        self.preset_defs = layout.preset_defs();
        self.presets = self.preset_defs.iter().map(|d| d.layout.clone()).collect();
        self.commands = std::rc::Rc::new(actions::commands(&self.presets, &self.panels));
        if let Some(palette) = &mut self.palette {
            palette.refilter(self.commands.iter().map(|c| c.title.as_ref()));
        }
        cx.set_menus(crate::app::menus(&self.presets));
        if layout.preset != previous.preset {
            self.apply_preset_named(&layout.preset.clone(), cx);
        } else if let Some(before) = before
            && let Some(after) = self
                .preset_defs
                .iter()
                .find(|d| d.layout.name == before.layout.name)
                .cloned()
            && after != before
        {
            let preset = after.layout;
            self.mutate(cx, |s| s.apply_preset(&preset));
        }
    }

    /// Opens a split of an existing tab: used by the pane header menus of terminal tabs.
    pub(crate) fn tab_session_menu(&self, pane: PaneId, index: usize) -> Vec<MenuEntry> {
        let session = self
            .state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .and_then(session_of_item);
        match session {
            Some(session) => {
                let mut entries = self.tab_menu(pane, index);
                let mut extra = vec![
                    Self::entry("Rename…", move |this, window, cx| {
                        this.rename_session(session, window, cx);
                    }),
                    Self::entry("Restart", move |this, window, cx| {
                        this.restart_session(session, window, cx);
                    }),
                    Self::entry("Stop…", move |this, window, cx| {
                        this.stop_session(session, window, cx);
                    }),
                    Self::entry("Copy Session ID", move |this, _, cx| {
                        this.copy_session_id(session, cx);
                    }),
                ];
                // "Close" (detach) stays last.
                let close = entries.pop();
                entries.append(&mut extra);
                entries.extend(close);
                entries
            }
            None => self.tab_menu(pane, index),
        }
    }
}
