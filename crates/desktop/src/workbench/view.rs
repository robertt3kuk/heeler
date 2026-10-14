//! `Workbench`: the GPUI entity owning the layout model, registries, and transient UI state
//! (palette, context menu, resize drags). Rendering lives in `render.rs`.

use super::actions::{self, Command};
use super::model::{
    DockPosition, DropTarget, Edge, FocusTarget, ItemRef, ItemSource, LayoutPreset, PaneId,
    PanelDest, PanelId, WorkbenchState,
};
use super::palette::PaletteState;
use super::panels::{builtin_panels, builtin_status_items};
use super::persistence;
use super::registry::{PanelRegistry, StatusBarLayout, StatusItemRegistry};
use super::style::UiStyle;
use crate::theme::{Density, Theme};
use gpui::Window;
use gpui::{
    AnyView, App, Bounds, Context, FocusHandle, Focusable, IntoElement, Pixels, Point, Render,
    SharedString, Subscription, Task, WindowAppearance, div, prelude::*, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Wait this long after the last layout change before writing `desktop-state.json`.
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// Keyboard resize steps.
const DOCK_STEP: f32 = 24.0;
const SPLIT_STEP: f32 = 0.05;

/// Everything configurable when building a workbench.
#[derive(Debug)]
pub struct WorkbenchOptions {
    pub panels: PanelRegistry,
    pub status_items: StatusItemRegistry,
    pub status_bar: StatusBarLayout,
    pub presets: Vec<LayoutPreset>,
    /// Where layout state is restored from and saved to; `None` disables persistence.
    pub state_path: Option<PathBuf>,
    /// Skip loading and start from this state (tests, wave-2 seeding).
    pub initial_state: Option<WorkbenchState>,
    pub density: Density,
}

impl WorkbenchOptions {
    /// Built-in placeholder panels, status items, and presets.
    pub fn builtin(state_path: Option<PathBuf>) -> Self {
        Self {
            panels: builtin_panels(),
            status_items: builtin_status_items(),
            status_bar: StatusBarLayout::default(),
            presets: LayoutPreset::builtin(),
            state_path,
            initial_state: None,
            density: Density::default(),
        }
    }
}

/// A laid-out area of the workbench whose painted bounds are recorded every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    Titlebar,
    StatusBar,
    Center,
    Dock(DockPosition),
    Pane(PaneId),
    /// Tab `index` of a center pane.
    Tab(PaneId, usize),
    /// Panel tab `index` of a dock.
    DockTab(DockPosition, usize),
}

/// A pane tab or dock tab being dragged.
#[derive(Clone, Debug)]
pub struct DraggedItem {
    pub source: ItemSource,
    pub item: ItemRef,
    pub label: SharedString,
}

/// The floating chip that follows the cursor during a drag.
#[derive(Debug)]
pub struct DragPreview {
    pub label: SharedString,
    pub style: UiStyle,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.style.theme;
        div()
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(self.style.density.tokens().radius))
            .bg(theme.overlay)
            .border_1()
            .border_color(theme.accent)
            .text_color(theme.fg)
            .text_size(px(self.style.density.tokens().font_size))
            .shadow_md()
            .child(self.label.clone())
    }
}

/// An in-progress drag of a splitter or dock edge.
#[derive(Clone, Debug)]
pub(crate) enum Resizing {
    Dock {
        pos: DockPosition,
        start_mouse: f32,
        start_size: f32,
    },
    Split {
        path: Vec<usize>,
        divider: usize,
        horizontal: bool,
        origin: f32,
        extent: f32,
    },
}

pub(crate) type MenuAction = Rc<dyn Fn(&mut Workbench, &mut Window, &mut Context<Workbench>)>;

#[derive(Clone)]
pub(crate) struct MenuEntry {
    pub label: SharedString,
    pub checked: bool,
    pub run: MenuAction,
}

#[derive(Clone)]
pub(crate) struct ContextMenu {
    pub position: Point<Pixels>,
    pub entries: Vec<MenuEntry>,
}

pub struct Workbench {
    pub(crate) state: WorkbenchState,
    pub(crate) panels: PanelRegistry,
    pub(crate) status_items: StatusItemRegistry,
    pub(crate) status_bar: StatusBarLayout,
    pub(crate) presets: Vec<LayoutPreset>,
    pub(crate) commands: Rc<Vec<Command>>,
    pub(crate) density: Density,
    pub(crate) appearance: WindowAppearance,
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) palette_focus: FocusHandle,
    pub(crate) menu_focus: FocusHandle,
    pub(crate) palette: Option<PaletteState>,
    pub(crate) context_menu: Option<ContextMenu>,
    pub(crate) resizing: Option<Resizing>,
    pub(crate) panel_views: HashMap<PanelId, AnyView>,
    pub(crate) notice: Option<SharedString>,
    pub(crate) last_title: String,
    pub(crate) regions: Rc<RefCell<HashMap<Region, Bounds<Pixels>>>>,
    state_path: Option<PathBuf>,
    last_saved: Option<String>,
    save_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Workbench {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workbench")
            .field("state", &self.state)
            .field("density", &self.density)
            .field("palette", &self.palette)
            .finish_non_exhaustive()
    }
}

impl Focusable for Workbench {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workbench {
    pub fn new(options: WorkbenchOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = match (options.initial_state, &options.state_path) {
            (Some(state), _) => state,
            (None, Some(path)) => persistence::load_state(path),
            (None, None) => WorkbenchState::default(),
        };
        let appearance = window.appearance();
        let theme = Theme::for_appearance(appearance);
        cx.set_global(UiStyle {
            theme,
            density: options.density,
        });
        let commands = Rc::new(actions::commands(&options.presets, &options.panels));
        let focus_handle = cx.focus_handle();
        let palette_focus = cx.focus_handle();
        let menu_focus = cx.focus_handle();
        window.focus(&focus_handle, cx);
        // Closing the window drops this entity before the quit hooks run: write pending
        // changes now instead of waiting for the debounce.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            this.update(cx, |this, _| this.flush_save()).ok();
            true
        });

        let subscriptions = vec![
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
            cx.on_app_quit(|this, _| {
                this.flush_save();
                async {}
            }),
        ];

        Self {
            last_saved: options
                .state_path
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok()),
            state,
            panels: options.panels,
            status_items: options.status_items,
            status_bar: options.status_bar,
            presets: options.presets,
            commands,
            density: options.density,
            appearance,
            theme,
            focus_handle,
            palette_focus,
            menu_focus,
            palette: None,
            context_menu: None,
            resizing: None,
            panel_views: HashMap::new(),
            notice: None,
            last_title: String::new(),
            regions: Rc::default(),
            state_path: options.state_path,
            save_task: None,
            _subscriptions: subscriptions,
        }
    }

    // ---- read access (tests, wave 2) -------------------------------------------------------

    pub fn state(&self) -> &WorkbenchState {
        &self.state
    }

    pub fn density(&self) -> Density {
        self.density
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub fn palette(&self) -> Option<&PaletteState> {
        self.palette.as_ref()
    }

    /// Titles of the palette rows currently shown, in order.
    pub fn palette_titles(&self) -> Vec<SharedString> {
        self.palette
            .as_ref()
            .map(|p| {
                p.matches
                    .iter()
                    .filter_map(|i| self.commands.get(*i))
                    .map(|c| c.title.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Painted bounds of `region` from the last frame (window coordinates), if it was drawn.
    pub fn region_bounds(&self, region: Region) -> Option<Bounds<Pixels>> {
        self.regions.borrow().get(&region).copied()
    }

    pub fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    pub fn panel_registry(&self) -> &PanelRegistry {
        &self.panels
    }

    /// Title shown in the title bar and window title for what has focus.
    pub fn active_title(&self) -> String {
        match self.state.active_item() {
            Some(item) => self.item_title(&item).to_string(),
            None => "Heeler".to_owned(),
        }
    }

    pub fn item_icon_and_title(&self, item: &ItemRef) -> (SharedString, SharedString) {
        match item {
            ItemRef::Terminal { session } => ("❯".into(), session.clone().into()),
            ItemRef::Chat { session } => ("✦".into(), session.clone().into()),
            ItemRef::Panel { panel } => (
                self.panels
                    .get(panel)
                    .map(|s| s.icon.clone())
                    .unwrap_or_else(|| "▫".into()),
                self.panels.title(panel),
            ),
            ItemRef::Placeholder { title } => ("▫".into(), title.clone().into()),
        }
    }

    pub fn item_title(&self, item: &ItemRef) -> SharedString {
        self.item_icon_and_title(item).1
    }

    // ---- state mutation ---------------------------------------------------------------------

    /// Apply a model change, then re-render and schedule a save.
    pub(crate) fn mutate(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut WorkbenchState),
    ) {
        change(&mut self.state);
        self.after_change(cx);
    }

    /// Like [`Self::mutate`] for fallible model operations; failures are logged, not shown.
    pub(crate) fn try_mutate<T, E: std::fmt::Display>(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut WorkbenchState) -> Result<T, E>,
    ) -> Option<T> {
        let result = match change(&mut self.state) {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!("layout operation rejected: {err}");
                None
            }
        };
        self.after_change(cx);
        result
    }

    fn after_change(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        cx.notify();
        self.schedule_save(cx);
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.state_path.is_none() {
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            this.update(cx, |this, _| this.flush_save()).ok();
        }));
    }

    /// Write the layout now if it changed since the last write. Called by the debounce timer,
    /// on quit, and by tests.
    pub fn flush_save(&mut self) {
        let Some(path) = &self.state_path else {
            return;
        };
        let json = match self.state.to_json() {
            Ok(json) => json,
            Err(err) => {
                tracing::error!("cannot serialize desktop layout: {err}");
                return;
            }
        };
        if self.last_saved.as_deref() == Some(json.as_str()) {
            return;
        }
        match persistence::save_json(path, &json) {
            Ok(()) => self.last_saved = Some(json),
            Err(err) => tracing::warn!(path = %path.display(), "cannot save desktop layout: {err}"),
        }
    }

    // ---- theme & density ---------------------------------------------------------------------

    /// Re-derive the theme if the system appearance changed. Called from `render`.
    pub(crate) fn sync_appearance(&mut self, window: &Window, cx: &mut Context<Self>) {
        let appearance = window.appearance();
        if appearance != self.appearance {
            self.appearance = appearance;
            self.theme = Theme::for_appearance(appearance);
            self.publish_style(cx);
        }
    }

    fn publish_style(&self, cx: &mut Context<Self>) {
        cx.set_global(UiStyle {
            theme: self.theme,
            density: self.density,
        });
    }

    pub fn set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        if self.density != density {
            self.density = density;
            self.publish_style(cx);
            cx.notify();
        }
    }

    // ---- panels ------------------------------------------------------------------------------

    pub(crate) fn panel_view(
        &mut self,
        id: &PanelId,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyView> {
        if let Some(view) = self.panel_views.get(id) {
            return Some(view.clone());
        }
        let spec = self.panels.get(id)?;
        let view = (spec.factory)(window, cx);
        self.panel_views.insert(id.clone(), view.clone());
        Some(view)
    }

    // ---- action handlers ---------------------------------------------------------------------

    pub(crate) fn focus_root(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    pub(crate) fn split_active(&mut self, edge: Edge, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        self.try_mutate(cx, |s| {
            s.split_pane(pane, edge, ItemRef::placeholder("New pane"))
        });
    }

    pub(crate) fn new_tab(&mut self, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        self.try_mutate(cx, |s| s.add_item(pane, ItemRef::placeholder("New tab")));
    }

    pub(crate) fn apply_preset_named(&mut self, name: &str, cx: &mut Context<Self>) {
        match self.presets.iter().find(|p| p.name == name).cloned() {
            Some(preset) => self.mutate(cx, |s| s.apply_preset(&preset)),
            None => tracing::warn!("unknown layout preset `{name}`"),
        }
    }

    pub(crate) fn toggle_overview(&mut self, cx: &mut Context<Self>) {
        let presets = self.presets.clone();
        self.try_mutate(cx, |s| s.toggle_preset(LayoutPreset::OVERVIEW, &presets));
    }

    /// The panel the user means by "this panel": the focused dock's active panel, else the
    /// focused pane's active tab when that is a panel.
    pub(crate) fn focused_panel(&self) -> Option<PanelId> {
        match self.state.focus {
            FocusTarget::Dock(pos) => self.state.docks.get(pos).active.clone(),
            FocusTarget::Pane(_) => self
                .state
                .active_pane()
                .and_then(|p| p.active_item())
                .and_then(|i| i.as_panel().cloned()),
        }
    }

    pub(crate) fn move_focused_panel(&mut self, dest: PanelDest, cx: &mut Context<Self>) {
        match self.focused_panel() {
            Some(panel) => {
                self.try_mutate(cx, |s| s.move_panel(&panel, dest));
            }
            None => self.set_notice("Focus a panel first", cx),
        }
    }

    pub(crate) fn move_active_tab_to_split(&mut self, edge: Edge, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        let Some(active) = self.state.pane(pane).map(|p| p.active) else {
            return;
        };
        self.try_mutate(cx, |s| {
            s.move_item(
                ItemSource::Pane {
                    pane,
                    index: active,
                },
                DropTarget::Split { pane, edge },
            )
        });
    }

    pub(crate) fn open_panel(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = PanelId::new(id);
        let dock = self
            .panels
            .get(&id)
            .map(|s| s.default_dock)
            .unwrap_or(DockPosition::Right);
        self.mutate(cx, |s| s.open_panel(&id, dock));
    }

    pub(crate) fn set_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
        cx.notify();
    }

    pub(crate) fn reset_layout(&mut self, cx: &mut Context<Self>) {
        self.mutate(cx, |s| *s = WorkbenchState::default());
    }

    /// Drop a dragged tab/panel on a target.
    pub(crate) fn drop_item(
        &mut self,
        dragged: &DraggedItem,
        target: DropTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.try_mutate(cx, |s| s.move_item(dragged.source.clone(), target));
        self.focus_root(window, cx);
    }

    pub(crate) fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.try_mutate(cx, |s| s.focus_pane(pane));
        self.focus_root(window, cx);
    }

    // ---- palette -----------------------------------------------------------------------------

    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    pub(crate) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
        } else {
            self.open_palette(window, cx);
        }
    }

    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut state = PaletteState::default();
        state.refilter(self.commands.iter().map(|c| c.title.as_ref()));
        self.palette = Some(state);
        self.context_menu = None;
        window.focus(&self.palette_focus, cx);
        cx.notify();
    }

    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    pub fn set_palette_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let commands = Rc::clone(&self.commands);
        if let Some(palette) = &mut self.palette {
            palette.query = query.to_owned();
            palette.refilter(commands.iter().map(|c| c.title.as_ref()));
            cx.notify();
        }
    }

    pub(crate) fn palette_type(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(query) = self.palette.as_ref().map(|p| format!("{}{text}", p.query)) {
            self.set_palette_query(&query, cx);
        }
    }

    pub(crate) fn palette_backspace(&mut self, cx: &mut Context<Self>) {
        if let Some(mut query) = self.palette.as_ref().map(|p| p.query.clone()) {
            query.pop();
            self.set_palette_query(&query, cx);
        }
    }

    pub(crate) fn palette_select(&mut self, next: bool, cx: &mut Context<Self>) {
        if let Some(palette) = &mut self.palette {
            if next {
                palette.select_next();
            } else {
                palette.select_prev();
            }
            cx.notify();
        }
    }

    /// Close the palette and run the highlighted command (or the one at `index` in the filtered
    /// list). The action is dispatched through the normal action path, after focus returns to the
    /// workbench, so palette and keyboard share one code path.
    pub(crate) fn palette_confirm(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(palette) = &self.palette else {
            return;
        };
        let chosen = match index {
            Some(i) => palette.matches.get(i).copied(),
            None => palette.selected_command(),
        };
        let action = chosen
            .and_then(|i| self.commands.get(i))
            .map(|c| c.action.boxed_clone());
        self.close_palette(window, cx);
        if let Some(action) = action {
            window.dispatch_action(action, cx);
        }
    }

    // ---- context menus -----------------------------------------------------------------------

    pub(crate) fn open_menu(
        &mut self,
        position: Point<Pixels>,
        entries: Vec<MenuEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(ContextMenu { position, entries });
        // Menus open on right-button *down*, and the workbench root re-focuses itself on every
        // mouse down later in the same dispatch; focus the menu once the event has finished.
        cx.defer_in(window, |this, window, cx| {
            if this.context_menu.is_some() {
                window.focus(&this.menu_focus, cx);
            }
        });
        cx.notify();
    }

    /// Open the title bar's layout-preset switcher at `position`.
    pub fn show_preset_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.preset_menu();
        self.open_menu(position, entries, window, cx);
    }

    /// Labels (with check state) of the open context menu, in order.
    pub fn context_menu_entries(&self) -> Option<Vec<(SharedString, bool)>> {
        self.context_menu.as_ref().map(|m| {
            m.entries
                .iter()
                .map(|e| (e.label.clone(), e.checked))
                .collect()
        })
    }

    /// Close the context menu and run its `index`-th entry.
    pub fn run_menu_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let run = self
            .context_menu
            .as_ref()
            .and_then(|m| m.entries.get(index))
            .map(|e| Rc::clone(&e.run));
        self.close_menu(window, cx);
        if let Some(run) = run {
            run(self, window, cx);
        }
    }

    pub(crate) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn entry(
        label: impl Into<SharedString>,
        run: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
    ) -> MenuEntry {
        MenuEntry {
            label: label.into(),
            checked: false,
            run: Rc::new(run),
        }
    }

    /// Menu for a panel tab (in a dock or open as a center tab).
    pub(crate) fn panel_menu(&self, panel: &PanelId) -> Vec<MenuEntry> {
        let mut entries = Vec::new();
        let here = self.state.find_panel(panel);
        for pos in DockPosition::ALL {
            let panel = panel.clone();
            let already = matches!(&here, Some(ItemSource::Dock { pos: p, .. }) if *p == pos);
            let mut e = Self::entry(
                format!("Move to {} Dock", pos.label()),
                move |this, _, cx| {
                    this.try_mutate(cx, |s| s.move_panel(&panel, PanelDest::Dock(pos)));
                },
            );
            e.checked = already;
            entries.push(e);
        }
        let p = panel.clone();
        entries.push(Self::entry("Open as Tab", move |this, _, cx| {
            this.try_mutate(cx, |s| s.move_panel(&p, PanelDest::Center));
        }));
        let p = panel.clone();
        entries.push(Self::entry("Close", move |this, _, cx| {
            this.try_mutate(cx, |s| s.close_panel(&p));
        }));
        entries
    }

    /// Menu for a center tab.
    pub(crate) fn tab_menu(&self, pane: PaneId, index: usize) -> Vec<MenuEntry> {
        let mut entries = vec![
            Self::entry("Split Right", move |this, _, cx| {
                this.try_mutate(cx, |s| {
                    s.move_item(
                        ItemSource::Pane { pane, index },
                        DropTarget::Split {
                            pane,
                            edge: Edge::Right,
                        },
                    )
                });
            }),
            Self::entry("Split Down", move |this, _, cx| {
                this.try_mutate(cx, |s| {
                    s.move_item(
                        ItemSource::Pane { pane, index },
                        DropTarget::Split {
                            pane,
                            edge: Edge::Bottom,
                        },
                    )
                });
            }),
        ];
        let panel = self
            .state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .and_then(|i| i.as_panel().cloned());
        if let Some(panel) = panel {
            for pos in DockPosition::ALL {
                let panel = panel.clone();
                entries.push(Self::entry(
                    format!("Move to {} Dock", pos.label()),
                    move |this, _, cx| {
                        this.try_mutate(cx, |s| s.move_panel(&panel, PanelDest::Dock(pos)));
                    },
                ));
            }
        }
        entries.push(Self::entry("Close", move |this, _, cx| {
            this.try_mutate(cx, |s| s.close_item(pane, index));
        }));
        entries
    }

    /// Menu of layout presets (title bar "Layout" switcher).
    pub(crate) fn preset_menu(&self) -> Vec<MenuEntry> {
        self.presets
            .iter()
            .map(|preset| {
                let name = preset.name.clone();
                let mut e = Self::entry(preset_label(&preset.name), move |this, _, cx| {
                    this.apply_preset_named(&name, cx);
                });
                e.checked = self.state.preset == preset.name;
                e
            })
            .collect()
    }

    // ---- resize drags ------------------------------------------------------------------------

    pub(crate) fn on_mouse_move(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resizing) = self.resizing.clone() else {
            return;
        };
        if !event.dragging() {
            self.resizing = None;
            return;
        }
        match resizing {
            Resizing::Dock {
                pos,
                start_mouse,
                start_size,
            } => {
                let mouse = match pos {
                    DockPosition::Bottom => f32::from(event.position.y),
                    _ => f32::from(event.position.x),
                };
                let delta = mouse - start_mouse;
                let size = match pos {
                    DockPosition::Left => start_size + delta,
                    DockPosition::Right | DockPosition::Bottom => start_size - delta,
                };
                let viewport = window.viewport_size();
                let max = match pos {
                    DockPosition::Bottom => f32::from(viewport.height) * 0.8,
                    _ => f32::from(viewport.width) * 0.6,
                };
                self.mutate(cx, |s| s.resize_dock(pos, size.min(max)));
            }
            Resizing::Split {
                path,
                divider,
                horizontal,
                origin,
                extent,
            } => {
                if extent <= 0.0 {
                    return;
                }
                let mouse = if horizontal {
                    f32::from(event.position.x)
                } else {
                    f32::from(event.position.y)
                };
                let position = (mouse - origin) / extent;
                self.mutate(cx, |s| {
                    let _ = s.set_divider(&path, divider, position);
                });
            }
        }
    }

    pub(crate) fn on_mouse_up(&mut self) {
        self.resizing = None;
    }

    pub(crate) fn start_dock_resize(&mut self, pos: DockPosition, mouse: Point<Pixels>) {
        let start_mouse = match pos {
            DockPosition::Bottom => f32::from(mouse.y),
            _ => f32::from(mouse.x),
        };
        self.resizing = Some(Resizing::Dock {
            pos,
            start_mouse,
            start_size: self.state.docks.get(pos).size,
        });
    }
}

/// `default` → "Default".
pub fn preset_label(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Keyboard resize steps (dock pixels, split ratio) shared with the action handlers.
pub(crate) const fn steps() -> (f32, f32) {
    (DOCK_STEP, SPLIT_STEP)
}
