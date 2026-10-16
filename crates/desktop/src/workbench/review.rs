//! The workbench side of the Tasks panel and the Changes view (M7-C): the palette commands
//! ("Tasks: New Task…", "Tasks: Show Panel", "Worktree: Review Changes…"), opening the Changes
//! view of a worktree or a task as a tab, and the notices those views raise.

use std::path::PathBuf;

use gpui::{AppContext as _, Context, Entity, Window};
use hutan_core::TaskId;
use hutan_core::TaskPlacementKind;

use super::changes::{ChangesEvent, ChangesView};
use super::model::{ItemRef, PanelId};
use super::panels::TasksPanel;
use super::view::{MenuEntry, Workbench};
use super::workspaces::{PickerAction, PickerEntry, PickerKind, PickerState, tilde};
use hutan_protocol::worktree::WorktreeInfo;

impl Workbench {
    /// "Tasks: Show Panel".
    pub(crate) fn show_tasks_panel(&mut self, cx: &mut Context<Self>) {
        self.open_panel("tasks", cx);
    }

    /// "Tasks: New Task…": reveals the panel and opens its form.
    pub(crate) fn new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_panel("tasks", cx);
        let id = PanelId::new("tasks");
        if let Some(panel) = self
            .panel_view(&id, window, cx)
            .and_then(|view| view.downcast::<TasksPanel>().ok())
        {
            panel.update(cx, |panel, cx| panel.open_form(cx));
        }
    }

    /// "Review Changes…" for the checkout `session` works in; none outside git.
    pub(crate) fn review_entry(
        &self,
        session: hutan_core::SessionId,
        cx: &gpui::App,
    ) -> Option<MenuEntry> {
        let root = self
            .sessions
            .read(cx)
            .session(session)?
            .placement
            .as_ref()?
            .root
            .clone();
        Some(Self::entry("Review Changes…", move |this, window, cx| {
            this.open_changes(root.clone(), window, cx);
        }))
    }

    /// The menu of a session's placement badge: review what the checkout changed, copy its path.
    pub(crate) fn placement_menu(
        &self,
        session: hutan_core::SessionId,
        cx: &gpui::App,
    ) -> Vec<MenuEntry> {
        let mut entries: Vec<MenuEntry> = self.review_entry(session, cx).into_iter().collect();
        if let Some(root) = self
            .sessions
            .read(cx)
            .session(session)
            .and_then(|s| s.placement.as_ref())
            .map(|p| p.root.clone())
        {
            entries.push(Self::entry("Copy Checkout Path", move |this, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(root.display().to_string()));
                this.set_notice("Copied the checkout path", cx);
            }));
        }
        entries
    }

    // ---- the Changes view ------------------------------------------------------------------

    /// The Changes view of the checkout at `path`, made when there is none.
    pub(crate) fn changes_view(
        &mut self,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Entity<ChangesView> {
        if let Some(view) = self.changes_views.get(path) {
            return view.clone();
        }
        let env = self.env.clone();
        let dir = PathBuf::from(path);
        let view = cx.new(|cx| ChangesView::new(env, dir, None, cx));
        let key = path.to_owned();
        let subscription = cx.subscribe(
            &view,
            move |this, _, event: &ChangesEvent, cx| match event {
                ChangesEvent::Notice(text) => this.set_notice(text.clone(), cx),
                ChangesEvent::Close => this.close_changes(&key, cx),
            },
        );
        self.changes_subscriptions.push(subscription);
        self.changes_views.insert(path.to_owned(), view.clone());
        view
    }

    /// The Changes view of `path`, if its tab was opened in this window.
    pub fn changes(&self, path: &str) -> Option<Entity<ChangesView>> {
        self.changes_views.get(path).cloned()
    }

    /// Opens the Changes view of the checkout at `path` as a tab (or focuses and refreshes its
    /// tab): "Worktree: Review Changes…", a task row's "Changes", a session's placement menu.
    pub fn open_changes(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let key = path.display().to_string();
        let existing = self.changes_views.contains_key(&key);
        let view = self.changes_view(&key, cx);
        if existing {
            view.update(cx, |view, cx| view.reload(cx));
        }
        let item = ItemRef::changes(key);
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

    fn close_changes(&mut self, path: &str, cx: &mut Context<Self>) {
        let item = ItemRef::changes(path);
        if let Some((pane, index)) = self.state.find_item(&item) {
            self.try_mutate(cx, |s| s.close_item(pane, index));
        }
        self.changes_views.remove(path);
    }

    /// A task row's "Changes": the checkout its last session works in, else the worktree of its
    /// branch (or the main checkout for a task placed there).
    pub(crate) fn review_task_changes(
        &mut self,
        task: TaskId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (found, root) = {
            let store = self.sessions.read(cx);
            let found = store.tasks().get(task).cloned();
            let root = found.as_ref().and_then(|t| {
                t.sessions
                    .iter()
                    .rev()
                    .filter_map(|id| store.session(*id))
                    .find_map(|s| s.placement.as_ref().map(|p| p.root.clone()))
            });
            (found, root)
        };
        let Some(found) = found else {
            self.set_notice("That task is gone", cx);
            return;
        };
        if let Some(root) = root {
            self.open_changes(root, window, cx);
            return;
        }
        let listing = self
            .sessions
            .update(cx, |store, cx| store.list_worktrees(found.workspace_id, cx));
        cx.spawn_in(window, async move |this, cx| {
            let listed = listing.await;
            this.update_in(cx, |this, window, cx| {
                let pick = listed.map_err(|e| e.to_string()).and_then(|list| {
                    let wanted = match found.placement.kind {
                        TaskPlacementKind::Main => list.into_iter().find(|w| w.main),
                        TaskPlacementKind::Worktree => {
                            let branch =
                                found.placement.branch.clone().unwrap_or_else(|| {
                                    hutan_core::task::default_branch(&found.title)
                                });
                            list.into_iter()
                                .find(|w| w.branch.as_deref() == Some(branch.as_str()))
                        }
                    };
                    wanted.ok_or_else(|| "its worktree does not exist (yet)".to_owned())
                });
                match pick {
                    Ok(worktree) => this.open_changes(worktree.path, window, cx),
                    Err(error) => {
                        this.set_notice(
                            format!("Cannot show the changes of the task: {error}"),
                            cx,
                        );
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// "Worktree: Review Changes…": a picker of the worktrees of the active workspace's
    /// repositories, the main checkouts included.
    pub fn open_review_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (connected, supported, workspace) = {
            let store = self.sessions.read(cx);
            (
                store.connection_state() == hutan_client::ConnectionState::Connected,
                store.connection().supports_worktree_review(),
                self.target_workspace(store).map(|w| w.id),
            )
        };
        if !connected {
            self.set_notice("Not connected to hutand", cx);
            return;
        }
        if !supported {
            self.set_notice(
                "This hutand cannot show changes; restart it (Daemon → Restart) to update",
                cx,
            );
            return;
        }
        let Some(workspace) = workspace else {
            return;
        };
        let mut state = PickerState::new(PickerKind::Changes, Vec::new());
        state.loading = true;
        self.show_picker(state, window, cx);
        let listing = self
            .sessions
            .update(cx, |store, cx| store.list_worktrees(workspace, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = listing.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(worktrees) => this.fill_review_picker(worktrees),
                    Err(err) => {
                        if let Some(picker) = &mut this.picker
                            && picker.kind == PickerKind::Changes
                        {
                            picker.loading = false;
                        }
                        this.set_notice(format!("Cannot list the worktrees: {err}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fill_review_picker(&mut self, worktrees: Vec<WorktreeInfo>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if picker.kind != PickerKind::Changes {
            return;
        }
        picker.entries = worktrees
            .into_iter()
            .filter(|w| !w.prunable)
            .map(|w| {
                let mut notes: Vec<String> = Vec::new();
                if w.main {
                    notes.push("main checkout".to_owned());
                }
                if w.dirty {
                    notes.push("uncommitted changes".to_owned());
                }
                let place = tilde(&w.path);
                let detail = if notes.is_empty() {
                    place
                } else {
                    format!("{place} · {}", notes.join(", "))
                };
                PickerEntry {
                    icon: "±".into(),
                    title: w
                        .branch
                        .clone()
                        .unwrap_or_else(|| "(detached)".to_owned())
                        .into(),
                    detail: detail.into(),
                    action: PickerAction::ReviewChanges(w.path),
                    fixed: false,
                }
            })
            .collect();
        picker.loading = false;
        picker.refilter();
    }
}
