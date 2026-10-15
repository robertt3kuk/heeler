//! Workbench behavior around projects and workspaces (M12): the active workspace and its
//! layout, the ⌘O project picker, the ⌘J jump palette, the workspace menus and commands.
//!
//! One workspace is *active*: its layout is the live one in the model (the others are parked,
//! see `model/workspace.rs`), its first root layers its `.hutan/settings.toml` over the user
//! settings, and new sessions start in it. Opening a session of another workspace from
//! anywhere (sidebar, ⌘J, attention, a restored tab) switches to that workspace first.
//!
//! **Terminals of a parked workspace are detached**, not kept attached: only what the live
//! layout shows is attached (`attach_open_items`), so nothing outside the window renders or is
//! acked. Coming back re-attaches, and the daemon answers with a snapshot of the screen
//! (measured in `tests/workspaces_gpui.rs`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, ElementId, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, PathPromptOptions, SharedString, StatefulInteractiveElement, Styled, Window,
    div, prelude::*, px,
};
use hutan_client::ConnectionState;
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::history::HistorySource;
use hutan_protocol::messages::{RecentProject, WorkspaceInfo, WorkspaceKind};

use super::actions::palette;
use super::modal::PromptSpec;
use super::palette::fuzzy_score;
use super::panels::HistoryResume;
use super::terminals::session_of_item;
use super::view::{MenuEntry, Workbench};
use crate::sessions::workspaces as order;
use crate::settings::{WorkspaceSort, WorkspacesSettings};
use crate::theme::DensityExt as _;

/// How many projects the ⌘O picker asks the daemon for.
const RECENT_LIMIT: u32 = 50;

/// Which list the picker shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// ⌘O: "Open Folder…" and the recent projects.
    Projects,
    /// ⌘J: every workspace and session, hidden workspaces included.
    Jump,
    /// ⌘K "Review approvals": the permission requests of every chat that wait for an answer.
    Approvals,
}

/// What confirming a picker row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickerAction {
    OpenFolder,
    OpenProject(PathBuf),
    Workspace(WorkspaceId),
    Session(SessionId),
    /// A history hit that can be resumed.
    ResumeHit(HistoryResume),
    /// A history hit without a resume command: show it in the History panel.
    ShowHit {
        query: String,
        source: HistorySource,
        key: String,
    },
}

/// One row of the picker.
#[derive(Debug, Clone)]
pub struct PickerEntry {
    pub icon: SharedString,
    pub title: SharedString,
    pub detail: SharedString,
    pub(crate) action: PickerAction,
    /// Stays listed whatever the query says ("Open Folder…").
    pub fixed: bool,
}

/// Where the ⌘J `?` history search stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HistoryState {
    /// Nothing typed after the `?`.
    #[default]
    Idle,
    Searching,
    Done,
    /// Indexing is off.
    Disabled,
    /// The daemon predates `history.*`.
    Unsupported,
    Failed(String),
}

/// An open ⌘O picker or ⌘J palette: the query, the entries, the filtered rows (indices into
/// `entries`, best first) and the highlighted row. A ⌘J query that starts with `?` searches the
/// session history instead: the rows are then the hits (`history`), best first.
#[derive(Debug, Clone)]
pub struct PickerState {
    pub kind: PickerKind,
    pub query: String,
    pub entries: Vec<PickerEntry>,
    pub matches: Vec<usize>,
    pub selected: usize,
    /// The project list is still on its way from the daemon.
    pub loading: bool,
    /// The hits of the `?` search.
    pub history: Vec<PickerEntry>,
    pub history_state: HistoryState,
}

impl PickerState {
    fn new(kind: PickerKind, entries: Vec<PickerEntry>) -> Self {
        let mut state = Self {
            kind,
            query: String::new(),
            entries,
            matches: Vec::new(),
            selected: 0,
            loading: false,
            history: Vec::new(),
            history_state: HistoryState::Idle,
        };
        state.refilter();
        state
    }

    /// The text after the `?` of a ⌘J history search; `None` for every other query.
    pub fn history_query(&self) -> Option<&str> {
        match self.kind {
            PickerKind::Jump => self.query.strip_prefix('?').map(str::trim),
            PickerKind::Projects | PickerKind::Approvals => None,
        }
    }

    /// The rows `matches` indexes: the hits in a history search, else the workspaces and sessions.
    fn shown(&self) -> &[PickerEntry] {
        if self.history_query().is_some() {
            &self.history
        } else {
            &self.entries
        }
    }

    /// Recomputes the filtered rows: fixed rows first, then the fuzzy matches, titles before
    /// details. In a history search every hit is a row, in the order the index ranked them.
    pub(super) fn refilter(&mut self) {
        if self.history_query().is_some() {
            self.matches = (0..self.history.len()).collect();
            self.selected = self.selected.min(self.matches.len().saturating_sub(1));
            return;
        }
        let mut fixed = Vec::new();
        let mut scored: Vec<(i32, usize)> = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.fixed {
                fixed.push(index);
                continue;
            }
            // Titles match fuzzily; details (folders, workspace names) only by whole words, so
            // a short query does not hit random letters of a long path.
            let detail = entry.detail.to_lowercase();
            let score = fuzzy_score(&self.query, &entry.title)
                .map(|s| s + 1000)
                .or_else(|| {
                    self.query
                        .split_whitespace()
                        .all(|word| detail.contains(&word.to_lowercase()))
                        .then_some(0)
                });
            if let Some(score) = score {
                scored.push((score, index));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = fixed;
        self.matches.extend(scored.into_iter().map(|(_, i)| i));
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    /// The row at position `row` of the filtered list.
    fn entry_at(&self, row: usize) -> Option<&PickerEntry> {
        self.matches
            .get(row)
            .and_then(|index| self.shown().get(*index))
    }

    fn selected_entry(&self) -> Option<&PickerEntry> {
        self.entry_at(self.selected)
    }
}

/// `~/proj/x` for paths under the user's home folder.
fn tilde(path: &Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl Workbench {
    // ---- the active workspace ----------------------------------------------------------------

    /// The workspace whose layout is shown.
    pub fn active_workspace(&self) -> Option<WorkspaceId> {
        self.state.workspace().and_then(|id| id.parse().ok())
    }

    /// The workspace new sessions start in: the active one, else Home (`kind == home`).
    pub(crate) fn target_workspace<'a>(
        &self,
        store: &'a crate::sessions::SessionsStore,
    ) -> Option<&'a WorkspaceInfo> {
        self.active_workspace()
            .and_then(|id| store.workspace(id))
            .or_else(|| store.home_workspace())
    }

    /// Shows `workspace`: its layout becomes the live one, its settings apply, and the
    /// terminals of the layout that was shown detach.
    pub fn switch_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_workspace(workspace, cx);
        self.focus_root(window, cx);
    }

    /// The model part of [`Self::switch_workspace`]: a no-op for the workspace already shown.
    pub(crate) fn show_workspace(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        self.resolve_active_workspace(cx);
        let key = workspace.to_string();
        if self.state.workspace() == Some(key.as_str()) {
            return;
        }
        self.mutate(cx, |s| {
            s.switch_workspace(&key);
        });
    }

    /// Switches to the workspace `session` belongs to (it is about to be shown).
    pub(crate) fn show_workspace_of(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let workspace = self
            .sessions
            .read(cx)
            .session(session)
            .map(|s| s.workspace_id);
        if let Some(workspace) = workspace {
            self.show_workspace(workspace, cx);
        }
    }

    /// Tells the sessions store and the settings which workspace is active: the first root of
    /// the active workspace layers its `.hutan/settings.toml` over the user settings (the
    /// settings store re-reads and hot-reloads that file while it stays the active one).
    pub(crate) fn sync_workspace(&mut self, cx: &mut Context<Self>) {
        let active = self.active_workspace();
        let root = {
            let store = self.sessions.read(cx);
            active
                .and_then(|id| store.workspace(id))
                .and_then(|w| w.roots.first().cloned())
        };
        self.sessions
            .update(cx, |store, cx| store.set_active_workspace(active, cx));
        if root != self.workspace_root {
            self.workspace_root = root.clone();
            self.settings.set_workspace_root(root);
        }
    }

    /// After the daemon told us what it has: a layout from before workspaces existed (or a first
    /// start) belongs to the workspace of its first session, else Home; a workspace that is gone
    /// hands the window to Home; parked layouts of vanished workspaces are forgotten.
    pub(crate) fn resolve_active_workspace(&mut self, cx: &mut Context<Self>) {
        let (known, home, owner) = {
            let store = self.sessions.read(cx);
            if !store.synced() || store.connection_state() != ConnectionState::Connected {
                return;
            }
            let known: HashSet<String> = store
                .workspaces()
                .iter()
                .map(|w| w.id.to_string())
                .collect();
            let owner = self
                .state
                .center
                .panes()
                .into_iter()
                .flat_map(|pane| pane.items.iter())
                .filter_map(session_of_item)
                .find_map(|session| store.session(session))
                .map(|session| session.workspace_id);
            (known, store.home_workspace().map(|w| w.id), owner)
        };
        let mut changed = false;
        match self.state.workspace().map(str::to_owned) {
            None => {
                if let Some(owner) = owner.or(home) {
                    self.state.adopt_workspace(&owner.to_string());
                    changed = true;
                }
            }
            Some(active) if known.contains(&active) => {
                if self.pending_workspace.map(|p| p.to_string()) == Some(active) {
                    self.pending_workspace = None;
                }
            }
            Some(active) => {
                let pending = self.pending_workspace.map(|p| p.to_string()) == Some(active);
                if !pending && let Some(home) = home {
                    self.state.switch_workspace(&home.to_string());
                    changed = true;
                }
            }
        }
        let gone: Vec<String> = self
            .state
            .parked_workspaces()
            .filter(|id| !known.contains(*id))
            .map(str::to_owned)
            .collect();
        for id in gone {
            self.state.forget_workspace(&id);
            changed = true;
        }
        if changed {
            self.schedule_save(cx);
        }
    }

    // ---- opening projects --------------------------------------------------------------------

    /// `workspace.open` for `path`, then shows the workspace (creating it when it is new).
    pub fn open_project_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.open_workspace(path, cx));
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(opened) => {
                this.update_in(cx, |this, window, cx| {
                    let id = opened.workspace.id;
                    // Its `workspace.updated` may still be on its way.
                    this.pending_workspace = Some(id);
                    this.switch_workspace(id, window, cx);
                })
                .ok();
            }
            Err(err) => {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot open the folder: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Asks for a folder with the system's open panel; `then` runs with the choice.
    fn choose_folder(
        &mut self,
        prompt: &'static str,
        then: impl FnOnce(&mut Workbench, PathBuf, &mut Window, &mut Context<Workbench>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                this.update_in(cx, |this, window, cx| then(this, path, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// "Open Folder…": the system folder panel, then `workspace.open`.
    pub fn open_folder_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_folder(
            "Open",
            |this, path, window, cx| this.open_project_path(path, window, cx),
            window,
            cx,
        );
    }

    // ---- workspace verbs ---------------------------------------------------------------------

    fn workspace_info(&self, id: WorkspaceId, cx: &gpui::App) -> Option<WorkspaceInfo> {
        self.sessions.read(cx).workspace(id).cloned()
    }

    /// The daemon's description of the active workspace.
    pub(crate) fn active_workspace_info(&self, cx: &gpui::App) -> Option<WorkspaceInfo> {
        self.active_workspace()
            .and_then(|id| self.workspace_info(id, cx))
    }

    fn report_unit(
        &mut self,
        what: &'static str,
        task: gpui::Task<Result<(), String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            if let Err(err) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot {what}: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn pin_workspace(
        &mut self,
        workspace: WorkspaceId,
        pinned: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.pin_workspace(workspace, pinned, cx));
        self.report_unit(
            if pinned {
                "pin the workspace"
            } else {
                "unpin the workspace"
            },
            task,
            window,
            cx,
        );
    }

    pub fn rename_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.workspace_info(workspace, cx) else {
            return;
        };
        self.open_prompt(
            PromptSpec {
                title: "Rename workspace",
                message: "New name:",
                initial: &info.name,
                confirm_label: "Rename",
            },
            move |this, name, window, cx| {
                let task = this.sessions.update(cx, |store, cx| {
                    store.rename_workspace(workspace, name.trim().to_owned(), cx)
                });
                this.report_unit("rename the workspace", task, window, cx);
            },
            window,
            cx,
        );
    }

    pub fn add_workspace_folder(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.choose_folder(
            "Add",
            move |this, path, window, cx| {
                let task = this.sessions.update(cx, |store, cx| {
                    store.add_workspace_root(workspace, path, cx)
                });
                this.report_unit("add the folder", task, window, cx);
            },
            window,
            cx,
        );
    }

    pub fn remove_workspace_folder(
        &mut self,
        workspace: WorkspaceId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.sessions.update(cx, |store, cx| {
            store.remove_workspace_root(workspace, path, cx)
        });
        self.report_unit("remove the folder", task, window, cx);
    }

    /// Closes a workspace without live sessions after asking; its project stays in the recent
    /// list. Home and workspaces with live sessions cannot be closed.
    pub fn close_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.workspace_info(workspace, cx) else {
            return;
        };
        if let Some(reason) = close_blocker(&info) {
            self.set_notice(format!("Cannot close “{}”: {reason}", info.name), cx);
            return;
        }
        self.open_confirm(
            "Close workspace?",
            format!(
                "“{}” leaves the sidebar and its ended sessions are removed. The project stays in the recent projects (⌘O).",
                info.name
            ),
            "Close",
            move |this, window, cx| {
                let task = this
                    .sessions
                    .update(cx, |store, cx| store.close_workspace(workspace, cx));
                this.report_unit("close the workspace", task, window, cx);
            },
            window,
            cx,
        );
    }

    pub fn reveal_workspace(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        match self
            .workspace_info(workspace, cx)
            .and_then(|w| w.roots.first().cloned())
        {
            Some(root) => (self.daemon.reveal.clone())(&root, cx),
            None => self.set_notice("This workspace has no folder", cx),
        }
    }

    /// The workspace row's context menu: Pin / Unpin, Rename, Add Folder, Remove Folder, Close,
    /// Reveal in Finder. Entries that do not apply are greyed out with their reason.
    pub(crate) fn workspace_menu(&self, workspace: WorkspaceId, cx: &gpui::App) -> Vec<MenuEntry> {
        let Some(info) = self.workspace_info(workspace, cx) else {
            return Vec::new();
        };
        let (home, pinned) = (info.kind == WorkspaceKind::Home, info.pinned);
        let mut entries = vec![
            Self::entry(
                if pinned { "Unpin" } else { "Pin" },
                move |this, window, cx| this.pin_workspace(workspace, !pinned, window, cx),
            ),
            Self::entry("Rename…", move |this, window, cx| {
                this.rename_workspace(workspace, window, cx);
            }),
        ];
        if home {
            entries.push(Self::disabled_entry("Add Folder… (Home has one folder)"));
            entries.push(Self::disabled_entry("Remove Folder (Home has one folder)"));
        } else {
            entries.push(Self::entry("Add Folder…", move |this, window, cx| {
                this.add_workspace_folder(workspace, window, cx);
            }));
            if info.roots.len() < 2 {
                entries.push(Self::disabled_entry("Remove Folder (the only folder)"));
            } else {
                for root in &info.roots {
                    let root = root.clone();
                    let label = format!("Remove Folder “{}”", folder_name(&root));
                    entries.push(Self::entry(label, move |this, window, cx| {
                        this.remove_workspace_folder(workspace, root.clone(), window, cx);
                    }));
                }
            }
        }
        match close_blocker(&info) {
            Some(reason) => entries.push(Self::disabled_entry(format!("Close ({reason})"))),
            None => entries.push(Self::entry("Close", move |this, window, cx| {
                this.close_workspace(workspace, window, cx);
            })),
        }
        entries.push(Self::entry("Reveal in Finder", move |this, _, cx| {
            this.reveal_workspace(workspace, cx);
        }));
        entries
    }

    /// Opens the context menu of a workspace row.
    pub fn show_workspace_menu(
        &mut self,
        workspace: WorkspaceId,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.workspace_menu(workspace, cx);
        if !entries.is_empty() {
            self.open_menu(position, entries, window, cx);
        }
    }

    // ---- sorting and filters -----------------------------------------------------------------

    /// Sets how the sidebar sorts workspaces and keeps it in `workspaces.sort`.
    pub fn set_workspace_sort(&mut self, mode: WorkspaceSort, cx: &mut Context<Self>) {
        self.write_workspace_setting("workspaces.sort", toml::Value::String(mode.key().into()));
        cx.notify();
    }

    /// Flips the boolean filter `key` (`hide_empty`, `hide_sleeping`, `hide_cli_created`,
    /// `always_show_active`) of the `workspaces` namespace.
    pub fn toggle_workspace_filter(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let current = self.settings.get::<WorkspacesSettings>();
        let value = match key {
            "hide_empty" => current.hide_empty,
            "hide_sleeping" => current.hide_sleeping,
            "hide_cli_created" => current.hide_cli_created,
            "always_show_active" => current.always_show_active,
            _ => return,
        };
        self.write_workspace_setting(&format!("workspaces.{key}"), toml::Value::Boolean(!value));
        cx.notify();
    }

    fn write_workspace_setting(&mut self, key: &str, value: toml::Value) {
        if let Err(err) = self.settings.write_user(key, value) {
            tracing::warn!("cannot save `{key}`: {err}");
        }
    }

    /// The sort and filter menu of the sidebar header.
    pub(crate) fn sort_menu(&self) -> Vec<MenuEntry> {
        let settings = self.settings.get::<WorkspacesSettings>();
        let mut entries: Vec<MenuEntry> = WorkspaceSort::ALL
            .into_iter()
            .map(|mode| {
                let mut entry = Self::entry(mode.label(), move |this, _, cx| {
                    this.set_workspace_sort(mode, cx);
                });
                entry.checked = settings.sort == mode;
                entry
            })
            .collect();
        for (key, label, on) in [
            ("hide_empty", "Hide Empty Workspaces", settings.hide_empty),
            (
                "hide_sleeping",
                "Hide Sleeping Workspaces",
                settings.hide_sleeping,
            ),
            (
                "hide_cli_created",
                "Hide CLI-Created Workspaces",
                settings.hide_cli_created,
            ),
            (
                "always_show_active",
                "Always Show the Active Workspace",
                settings.always_show_active,
            ),
        ] {
            let mut entry = Self::entry(label, move |this, _, cx| {
                this.toggle_workspace_filter(key, cx);
            });
            entry.checked = on;
            entries.push(entry);
        }
        entries
    }

    /// Opens the sort and filter menu at `position`.
    pub fn show_sort_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.sort_menu();
        self.open_menu(position, entries, window, cx);
    }

    // ---- manual reordering -------------------------------------------------------------------

    /// A workspace row was dropped onto another: `workspace.move` (manual order only).
    pub fn drop_workspace(
        &mut self,
        dragged: WorkspaceId,
        target: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.get::<WorkspacesSettings>().sort != WorkspaceSort::Manual {
            return;
        }
        let before = order::reorder(self.sessions.read(cx).workspaces(), dragged, target);
        let Some(before) = before else {
            return;
        };
        let task = self
            .sessions
            .update(cx, |store, cx| store.move_workspace(dragged, before, cx));
        self.report_unit("move the workspace", task, window, cx);
    }

    // ---- the picker (⌘O project picker, ⌘J jump palette) -------------------------------------

    /// The open picker, if any.
    pub fn picker(&self) -> Option<&PickerState> {
        self.picker.as_ref()
    }

    /// Titles of the picker rows currently shown, in order.
    pub fn picker_titles(&self) -> Vec<SharedString> {
        self.picker
            .as_ref()
            .map(|p| {
                (0..p.matches.len())
                    .filter_map(|row| p.entry_at(row))
                    .map(|e| e.title.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn show_picker(&mut self, state: PickerState, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.context_menu = None;
        self.picker = Some(state);
        window.focus(&self.picker_focus, cx);
        cx.notify();
    }

    pub fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_search = None;
        if self.picker.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    /// ⌘O: "Open Folder…" and the recent projects (`workspace.recent`).
    pub fn open_project_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open_folder = PickerEntry {
            icon: "＋".into(),
            title: "Open Folder…".into(),
            detail: "Choose a folder in Finder".into(),
            action: PickerAction::OpenFolder,
            fixed: true,
        };
        let mut state = PickerState::new(PickerKind::Projects, vec![open_folder]);
        state.loading = true;
        self.show_picker(state, window, cx);
        let task = self
            .sessions
            .update(cx, |store, cx| store.recent_projects(RECENT_LIMIT, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let notice = match result {
                    Ok(projects) => {
                        this.fill_projects(projects);
                        None
                    }
                    Err(err) => {
                        if let Some(picker) = &mut this.picker
                            && picker.kind == PickerKind::Projects
                        {
                            picker.loading = false;
                        }
                        Some(format!("Cannot list the recent projects: {err}"))
                    }
                };
                if let Some(notice) = notice {
                    this.set_notice(notice, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fill_projects(&mut self, projects: Vec<RecentProject>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if picker.kind != PickerKind::Projects {
            return;
        }
        picker.entries.truncate(1);
        picker
            .entries
            .extend(projects.into_iter().map(|project| PickerEntry {
                icon: if project.workspace_id.is_some() {
                    "▣".into()
                } else {
                    "▢".into()
                },
                title: project.name.into(),
                detail: tilde(&project.path).into(),
                action: PickerAction::OpenProject(project.path),
                fixed: false,
            }));
        picker.loading = false;
        picker.refilter();
    }

    /// ⌘J: every workspace (hidden ones too) and every session; Enter shows it.
    pub fn open_jump_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = {
            let store = self.sessions.read(cx);
            let settings = self.settings.get::<WorkspacesSettings>();
            let sorted = order::sort(
                store.workspaces(),
                settings.sort,
                &store.needs_you_by_workspace(),
            );
            let mut entries: Vec<PickerEntry> = sorted
                .iter()
                .map(|w| PickerEntry {
                    icon: "▣".into(),
                    title: w.name.clone().into(),
                    detail: w
                        .roots
                        .first()
                        .map(|root| tilde(root))
                        .unwrap_or_default()
                        .into(),
                    action: PickerAction::Workspace(w.id),
                    fixed: false,
                })
                .collect();
            for workspace in &sorted {
                for session in store
                    .sessions()
                    .iter()
                    .filter(|s| s.workspace_id == workspace.id)
                {
                    entries.push(PickerEntry {
                        icon: super::panels::kind_icon(session.kind).into(),
                        title: session.title.clone().into(),
                        detail: format!("{} · {}", workspace.name, session.command.join(" "))
                            .into(),
                        action: PickerAction::Session(session.id),
                        fixed: false,
                    });
                }
            }
            entries
        };
        self.show_picker(PickerState::new(PickerKind::Jump, entries), window, cx);
    }

    /// ⌘K "Review approvals": every pending permission request, oldest first; Enter shows the
    /// chat it belongs to, where its card has the buttons.
    pub fn open_approvals_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries: Vec<PickerEntry> = {
            let store = self.sessions.read(cx);
            store
                .approvals()
                .iter()
                .map(|approval| {
                    let session = store.session(approval.session_id);
                    let options: Vec<String> = approval
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| format!("{} {}", i + 1, o.label))
                        .collect();
                    PickerEntry {
                        icon: "●".into(),
                        title: approval.title.clone().into(),
                        detail: format!(
                            "{} · {}",
                            session.map_or("a session that is gone", |s| s.title.as_str()),
                            options.join(" · ")
                        )
                        .into(),
                        action: PickerAction::Session(approval.session_id),
                        fixed: false,
                    }
                })
                .collect()
        };
        self.show_picker(PickerState::new(PickerKind::Approvals, entries), window, cx);
    }

    pub fn set_picker_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        picker.query = query.to_owned();
        picker.selected = 0;
        picker.refilter();
        let searching_history = picker.history_query().is_some();
        if searching_history {
            self.search_history_in_picker(cx);
        } else {
            self.picker_search = None;
        }
        cx.notify();
    }

    pub(crate) fn picker_type(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(query) = self.picker.as_ref().map(|p| format!("{}{text}", p.query)) {
            self.set_picker_query(&query, cx);
        }
    }

    pub(crate) fn picker_backspace(&mut self, cx: &mut Context<Self>) {
        if let Some(mut query) = self.picker.as_ref().map(|p| p.query.clone()) {
            query.pop();
            self.set_picker_query(&query, cx);
        }
    }

    pub(crate) fn picker_select(&mut self, next: bool, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.picker {
            let last = picker.matches.len().saturating_sub(1);
            picker.selected = if next {
                (picker.selected + 1).min(last)
            } else {
                picker.selected.saturating_sub(1)
            };
            cx.notify();
        }
    }

    /// Closes the picker and runs the highlighted row (or the one at `index`).
    pub fn picker_confirm(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = &self.picker else {
            return;
        };
        let entry = match index {
            Some(row) => picker.entry_at(row),
            None => picker.selected_entry(),
        };
        let Some(action) = entry.map(|e| e.action.clone()) else {
            return;
        };
        self.close_picker(window, cx);
        match action {
            PickerAction::OpenFolder => self.open_folder_dialog(window, cx),
            PickerAction::OpenProject(path) => self.open_project_path(path, window, cx),
            PickerAction::Workspace(workspace) => self.switch_workspace(workspace, window, cx),
            PickerAction::Session(session) => self.open_session(session, window, cx),
            PickerAction::ResumeHit(resume) => self.resume_history_hit(resume, window, cx),
            PickerAction::ShowHit { query, source, key } => {
                self.show_history_hit(&query, source, &key, window, cx);
            }
        }
    }

    pub(crate) fn render_picker(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.picker.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        const VISIBLE_ROWS: usize = 12;
        let start = picker.selected.saturating_sub(VISIBLE_ROWS - 1);
        let rows: Vec<AnyElement> = picker
            .matches
            .iter()
            .enumerate()
            .skip(start)
            .take(VISIBLE_ROWS)
            .filter_map(|(row, _)| {
                let entry = picker.entry_at(row)?;
                let selected = row == picker.selected;
                let hover = theme.hover;
                Some(
                    div()
                        .id(ElementId::from(SharedString::from(format!(
                            "picker-row-{row}"
                        ))))
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .h(px(tokens.sidebar_row))
                        .px(px(tokens.spacing * 2.0))
                        .rounded(px(tokens.radius - 2.0))
                        .cursor_pointer()
                        .when(selected, |s| s.bg(theme.accent.opacity(0.25)))
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.picker_confirm(Some(row), window, cx);
                        }))
                        .child(
                            div()
                                .w(px(14.0))
                                .text_color(theme.muted)
                                .child(entry.icon.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(260.0))
                                .truncate()
                                .child(entry.title.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted)
                                .text_size(px(tokens.section_font_size + 1.0))
                                .child(entry.detail.clone()),
                        )
                        .into_any_element(),
                )
            })
            .collect();
        let hint: Option<SharedString> = match (
            picker.history_query().is_some(),
            picker.matches.is_empty(),
            picker.loading,
        ) {
            (_, _, true) => Some("Loading recent projects…".into()),
            (true, true, false) => Some(match &picker.history_state {
                HistoryState::Idle => "Type to search the session history".into(),
                HistoryState::Searching => "Searching…".into(),
                HistoryState::Done => "No session matches".into(),
                HistoryState::Disabled => {
                    "History indexing is off. Enable it in the History panel (⌘K → Search History)"
                        .into()
                }
                HistoryState::Unsupported => {
                    "This hutand does not offer history search; restart it".into()
                }
                HistoryState::Failed(error) => format!("History search failed: {error}").into(),
            }),
            (true, false, false) => None,
            (false, true, false) if picker.kind == PickerKind::Projects => {
                Some("No matching project".into())
            }
            (false, true, false) if picker.kind == PickerKind::Approvals => {
                Some(if picker.entries.is_empty() {
                    "No approvals are waiting".into()
                } else {
                    "No approval matches".into()
                })
            }
            (false, true, false) => Some("Nothing matches".into()),
            (false, false, false) => None,
        };
        let prompt = match (picker.kind, picker.history_query().is_some()) {
            (PickerKind::Projects, _) => "Open project ›",
            (PickerKind::Jump, true) => "Search history ›",
            (PickerKind::Jump, false) => "Jump to ›",
            (PickerKind::Approvals, _) => "Review approvals ›",
        };

        Some(
            div()
                .id("picker-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(80.0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_picker(window, cx)),
                )
                .child(
                    div()
                        .id("picker")
                        .key_context("Picker")
                        .track_focus(&self.picker_focus)
                        .w(px(560.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .p(px(tokens.spacing * 2.0))
                        .rounded(px(tokens.radius + 4.0))
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            let keystroke = &event.keystroke;
                            if keystroke.key == "backspace" {
                                this.picker_backspace(cx);
                            } else if !keystroke.modifiers.platform
                                && !keystroke.modifiers.control
                                && !keystroke.modifiers.function
                                && let Some(text) = &keystroke.key_char
                            {
                                this.picker_type(text, cx);
                            }
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectNext, _, cx| {
                            this.picker_select(true, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectPrev, _, cx| {
                            this.picker_select(false, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::Confirm, window, cx| {
                            this.picker_confirm(None, window, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .h(px(tokens.sidebar_row + 6.0))
                                .px(px(tokens.spacing * 2.0))
                                .border_b_1()
                                .border_color(theme.border)
                                .child(div().text_color(theme.muted).child(format!("{prompt} ")))
                                .child(picker.query.clone())
                                .child(
                                    div()
                                        .w(px(1.0))
                                        .h(px(tokens.font_size + 2.0))
                                        .bg(theme.accent),
                                ),
                        )
                        .children(rows)
                        .when_some(hint, |s, hint| {
                            s.child(
                                div()
                                    .px(px(tokens.spacing * 2.0))
                                    .text_color(theme.muted)
                                    .child(hint),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

/// Why `workspace` cannot be closed, if it cannot.
fn close_blocker(workspace: &WorkspaceInfo) -> Option<String> {
    if workspace.kind == WorkspaceKind::Home {
        return Some("Home cannot be closed".to_owned());
    }
    match workspace.live_sessions {
        0 => None,
        1 => Some("1 live session".to_owned()),
        n => Some(format!("{n} live sessions")),
    }
}
