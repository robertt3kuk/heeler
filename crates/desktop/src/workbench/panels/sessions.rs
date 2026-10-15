//! The `sessions` panel: the workspace list (sorted, pinned, filtered, draggable) with the
//! sessions of each workspace under it, search, "Needs you", grouping, configurable rows,
//! actions.

use crate::theme::DensityExt as _;
use std::collections::HashSet;
use std::time::Duration;

use gpui::{
    AnyElement, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Task, Window, div, prelude::*, px,
};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::messages::{SessionInfo, WorkspaceInfo, WorkspaceKind};

use super::common::{
    age_label, agent_badge, kind_icon, panel_header_with, placement, placement_badge, state_dot,
    tone_color,
};
use crate::sessions::workspaces as order;
use crate::settings::{GroupBy, SessionsPanelSettings, WorkspaceSort, WorkspacesSettings};
use crate::workbench::actions::{
    MoveWorkspaceOnto, NewTerminal, OpenSession, OpenWorkspace, SessionMenuAt, ShowSessionMenu,
    ShowSortMenu, ShowWorkspaceMenu, WorkspaceDrop, WorkspaceMenuAt,
};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::{BadgeSite, DragPreview, Region};

/// Row refresh for the relative ages.
const AGE_REFRESH: Duration = Duration::from_secs(30);

/// A workspace row being dragged to a new place (manual order).
#[derive(Clone, Debug)]
pub struct DraggedWorkspace {
    pub workspace: WorkspaceId,
    pub label: SharedString,
}

pub struct SessionsPanel {
    env: PanelEnv,
    query: String,
    search_focus: FocusHandle,
    collapsed: HashSet<WorkspaceId>,
    _ages: Task<()>,
}

impl std::fmt::Debug for SessionsPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionsPanel")
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

impl SessionsPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |_, _, cx| cx.notify()).detach();
        let ages = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGE_REFRESH).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            env,
            query: String::new(),
            search_focus: cx.focus_handle(),
            collapsed: HashSet::new(),
            _ages: ages,
        }
    }

    /// The search text (tests, inspection).
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Sets the search text.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.query = query.to_owned();
        cx.notify();
    }

    /// Titles of the rows currently listed, top to bottom (needs-you first).
    pub fn visible_titles(&self, cx: &gpui::App) -> Vec<String> {
        let settings = self.env.settings.get::<SessionsPanelSettings>();
        self.layout(&settings, cx)
            .into_iter()
            .flat_map(|section| section.rows)
            .map(|info| info.title)
            .collect()
    }

    /// Names of the workspaces currently listed, top to bottom (pinned first), whatever
    /// the mode orders them by.
    pub fn visible_workspaces(&self, cx: &gpui::App) -> Vec<String> {
        let settings = self.env.settings.get::<SessionsPanelSettings>();
        self.layout(&settings, cx)
            .into_iter()
            .filter_map(|section| section.workspace.map(|w| w.name))
            .collect()
    }

    fn matches(&self, info: &SessionInfo) -> bool {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let haystack = format!(
            "{} {} {} {} {}",
            info.title,
            info.command.join(" "),
            placement(info),
            crate::sessions::placement_badge(info)
                .map(|badge| badge.text)
                .unwrap_or_default(),
            info.cwd
                .as_ref()
                .map(|c| c.display().to_string())
                .unwrap_or_default()
        )
        .to_lowercase();
        query.split_whitespace().all(|word| haystack.contains(word))
    }

    /// Sections in display order, after workspace filters, search and grouping.
    fn layout(&self, settings: &SessionsPanelSettings, cx: &gpui::App) -> Vec<Section> {
        let store = self.env.sessions.read(cx);
        let ws_settings = self.env.settings.get::<WorkspacesSettings>();
        let active = store.active_workspace_id();
        let needs: Vec<SessionId> = store.needs_you();
        let attention = store.needs_you_by_workspace();
        let sorted = order::sort(store.workspaces(), ws_settings.sort, &attention);
        let shown: Vec<&WorkspaceInfo> = sorted
            .into_iter()
            .filter(|w| !order::hidden(w, &ws_settings, active))
            .collect();
        let workspace_of = |s: &SessionInfo| store.workspace(s.workspace_id);
        // A session is listed when it matches the search itself or sits in a workspace whose
        // name or folder does.
        let listed = |s: &SessionInfo| {
            self.matches(s) || workspace_of(s).is_some_and(|w| order::matches_query(w, &self.query))
        };
        let visible: Vec<&SessionInfo> = store
            .sessions()
            .iter()
            .filter(|s| listed(s))
            .filter(|s| {
                // Sessions that need the user stay visible whatever the filters say.
                (settings.needs_you && needs.contains(&s.id))
                    || shown.iter().any(|w| w.id == s.workspace_id)
            })
            .collect();
        let mut sections = Vec::new();
        let (needing, rest): (Vec<&SessionInfo>, Vec<&SessionInfo>) = if settings.needs_you {
            visible.into_iter().partition(|s| needs.contains(&s.id))
        } else {
            (Vec::new(), visible)
        };
        if !needing.is_empty() {
            sections.push(Section {
                key: SectionKey::NeedsYou,
                title: format!("NEEDS YOU · {}", needing.len()),
                rows: needing.into_iter().cloned().collect(),
                workspace: None,
                needs: 0,
            });
        }
        match settings.group_by {
            GroupBy::None => {
                if !rest.is_empty() {
                    sections.push(Section {
                        key: SectionKey::All,
                        title: String::new(),
                        rows: rest.into_iter().cloned().collect(),
                        workspace: None,
                        needs: 0,
                    });
                }
            }
            GroupBy::Workspace => {
                let searching = !self.query.trim().is_empty();
                let mut pinned_label = false;
                for workspace in shown {
                    let rows: Vec<SessionInfo> = rest
                        .iter()
                        .filter(|s| s.workspace_id == workspace.id)
                        .map(|s| (*s).clone())
                        .collect();
                    if searching && rows.is_empty() {
                        continue;
                    }
                    if workspace.pinned && !pinned_label {
                        pinned_label = true;
                        sections.push(Section {
                            key: SectionKey::Pinned,
                            title: "PINNED".to_owned(),
                            rows: Vec::new(),
                            workspace: None,
                            needs: 0,
                        });
                    }
                    sections.push(Section {
                        key: SectionKey::Workspace(workspace.id),
                        title: workspace.name.to_uppercase(),
                        rows,
                        workspace: Some(workspace.clone()),
                        needs: attention.get(&workspace.id).copied().unwrap_or(0),
                    });
                }
            }
            GroupBy::State => {
                let mut labels: Vec<String> = Vec::new();
                for info in &rest {
                    let label = store.presentation(info).label;
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
                for label in labels {
                    let rows: Vec<SessionInfo> = rest
                        .iter()
                        .filter(|s| store.presentation(s).label == label)
                        .map(|s| (*s).clone())
                        .collect();
                    sections.push(Section {
                        key: SectionKey::State(label.clone()),
                        title: format!("{}  ·  {}", label.to_uppercase(), rows.len()),
                        rows,
                        workspace: None,
                        needs: 0,
                    });
                }
            }
        }
        sections
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        if ks.key == "backspace" {
            self.query.pop();
        } else if ks.key == "escape" {
            self.query.clear();
        } else if let Some(text) = &ks.key_char {
            self.query.push_str(text);
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SectionKey {
    NeedsYou,
    All,
    Pinned,
    Workspace(WorkspaceId),
    State(String),
}

struct Section {
    key: SectionKey,
    title: String,
    rows: Vec<SessionInfo>,
    workspace: Option<WorkspaceInfo>,
    /// Sessions of the workspace that need the user.
    needs: usize,
}

impl Focusable for SessionsPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.search_focus.clone()
    }
}

impl Render for SessionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<SessionsPanelSettings>();
        let ws_settings = self.env.settings.get::<WorkspacesSettings>();
        let sections = self.layout(&settings, cx);
        let (focused, active) = {
            let store = self.env.sessions.read(cx);
            (store.focused(), store.active_workspace_id())
        };
        let searching = self.search_focus.is_focused(window);
        let hover = theme.hover;
        let manual = ws_settings.sort == WorkspaceSort::Manual;

        let mut list = div()
            .id("sessions-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if sections.is_empty() {
            let empty = if self.query.trim().is_empty() {
                "No sessions yet. ⌘T starts a terminal."
            } else {
                "Nothing matches the filter."
            };
            list = list.child(
                div()
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .text_color(theme.muted)
                    .child(empty),
            );
        }
        for section in sections {
            let workspace_id = section.workspace.as_ref().map(|w| w.id);
            let collapsed = workspace_id.is_some_and(|w| self.collapsed.contains(&w));
            let header_color = if section.key == SectionKey::NeedsYou {
                theme.warning
            } else {
                theme.muted
            };
            if let Some(workspace) = &section.workspace {
                list = list.child(self.render_workspace_header(
                    workspace,
                    section.rows.len(),
                    section.needs,
                    collapsed,
                    active == Some(workspace.id),
                    manual,
                    cx,
                ));
            } else if !section.title.is_empty() {
                list = list.child(
                    div()
                        .id(SharedString::from(format!("section-{:?}", section.key)))
                        .flex()
                        .items_center()
                        .h(px(tokens.sidebar_row))
                        .px(px(tokens.spacing * 2.0))
                        .text_size(px(tokens.section_font_size))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(header_color)
                        .child(section.title.clone()),
                );
            }
            if collapsed {
                continue;
            }
            for info in &section.rows {
                list = list.child(self.render_row(
                    info,
                    &settings,
                    focused == Some(info.id),
                    section.key == SectionKey::NeedsYou,
                    cx,
                ));
            }
        }

        let sort_button = div()
            .id("workspaces-sort")
            .px(px(tokens.spacing))
            .rounded(px(3.0))
            .cursor_pointer()
            .text_size(px(tokens.section_font_size))
            .text_color(theme.muted)
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|event, window, cx| {
                window.dispatch_action(Box::new(ShowSortMenu(event.position())), cx);
            })
            .child(format!("⇅ {} ▾", ws_settings.sort.label()))
            .into_any_element();

        let search = div()
            .id("sessions-search")
            .key_context("SessionsSearch")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row))
            .flex()
            .items_center()
            .rounded(px(tokens.radius - 1.0))
            .border_1()
            .border_color(if searching {
                theme.accent
            } else {
                theme.border
            })
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    // The dock re-focuses the workbench on this same mouse down; take focus
                    // after the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.search_focus, cx);
                    });
                }),
            )
            .child(if self.query.is_empty() {
                div()
                    .text_color(theme.muted)
                    .child("⌕ Filter workspaces and sessions")
            } else {
                div().child(self.query.clone())
            });

        let new_session = div()
            .id("sessions-new")
            .flex()
            .flex_none()
            .items_center()
            .h(px(tokens.sidebar_row))
            .mx(px(tokens.spacing))
            .mb(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius - 1.0))
            .cursor_pointer()
            .text_color(theme.muted)
            .hover(move |s| s.bg(hover))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(NewTerminal), cx))
            .child("＋ New session");

        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header_with(
                "Workspaces",
                "panels.sessions",
                vec![sort_button],
                cx,
            ))
            .child(search)
            .child(list)
            .child(new_session)
    }
}

impl SessionsPanel {
    /// `▾ ★ NAME  ······  ● 2  3`: click shows the workspace, the chevron folds its sessions,
    /// right-click opens its menu, and (manual order) the row can be dragged to reorder.
    #[allow(clippy::too_many_arguments)] // one call site; a struct would only rename them
    fn render_workspace_header(
        &self,
        workspace: &WorkspaceInfo,
        rows: usize,
        needs: usize,
        collapsed: bool,
        active: bool,
        draggable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let (hover, accent) = (theme.hover, theme.accent);
        let id = workspace.id;
        let id_text = id.to_string();
        let open_id = id_text.clone();
        let menu_id = id_text.clone();
        let drop_id = id_text.clone();
        let count = if workspace.live_sessions as usize == rows || workspace.total_sessions == 0 {
            rows.to_string()
        } else {
            format!("{}/{}", workspace.live_sessions, workspace.total_sessions)
        };
        let mut header = div()
            .id(SharedString::from(format!("workspace-{id_text}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.sidebar_row))
            .mx(px(tokens.spacing))
            .px(px(tokens.spacing))
            .rounded(px(tokens.radius - 1.0))
            .text_size(px(tokens.section_font_size))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(if active { theme.fg } else { theme.muted })
            .cursor_pointer()
            .when(active, move |s| s.bg(accent.opacity(0.18)))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |_, _, window, cx| {
                window.dispatch_action(Box::new(OpenWorkspace(open_id.clone())), cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    window.dispatch_action(
                        Box::new(ShowWorkspaceMenu(WorkspaceMenuAt {
                            workspace: menu_id.clone(),
                            position: event.position,
                        })),
                        cx,
                    );
                }),
            )
            .drag_over::<DraggedWorkspace>(move |s, _, _, _| s.border_t_2().border_color(accent))
            .on_drop(
                cx.listener(move |_, dragged: &DraggedWorkspace, window, cx| {
                    window.dispatch_action(
                        Box::new(MoveWorkspaceOnto(WorkspaceDrop {
                            dragged: dragged.workspace.to_string(),
                            target: drop_id.clone(),
                        })),
                        cx,
                    );
                }),
            );
        if draggable {
            let dragged = DraggedWorkspace {
                workspace: id,
                label: workspace.name.clone().into(),
            };
            header = header.on_drag(dragged, |dragged: &DraggedWorkspace, _, _, cx| {
                let style = UiStyle::get(cx);
                let label = dragged.label.clone();
                cx.new(|_| DragPreview { label, style })
            });
        }
        header
            .child(
                div()
                    .id(SharedString::from(format!("workspace-fold-{id_text}")))
                    .flex_none()
                    .w(px(12.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&id) {
                            this.collapsed.insert(id);
                        }
                        cx.notify();
                    }))
                    .child(if collapsed { "▸" } else { "▾" }),
            )
            .when(workspace.kind == WorkspaceKind::Home, |s| {
                s.child(div().flex_none().child("⌂"))
            })
            .when(workspace.pinned, |s| {
                s.child(div().flex_none().text_color(theme.accent).child("★"))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(workspace.name.to_uppercase())),
            )
            .when(needs > 0, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.warning)
                        .child(format!("● {needs}")),
                )
            })
            .child(div().flex_none().child(count))
            .child(probe(&self.env.regions, Region::WorkspaceRow(id)))
            .into_any_element()
    }
}

impl SessionsPanel {
    fn render_row(
        &self,
        info: &SessionInfo,
        settings: &SessionsPanelSettings,
        selected: bool,
        in_needs_you: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let presentation = self.env.sessions.read(cx).presentation(info);
        let hover = theme.hover;
        let accent = theme.accent;
        let id = info.id.to_string();
        let open_id = id.clone();
        let menu_id = id.clone();
        let badge = self.env.sessions.read(cx).agent_badge(info.id);
        let mut badge = agent_badge(
            &theme,
            &badge,
            BadgeSite::Row,
            info.id,
            &self.env.regions,
            cx,
        );

        let mut line = div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 1.5))
            .h(px(tokens.sidebar_row))
            .w_full();
        for field in &settings.row {
            line = match field.as_str() {
                "state" => line.child(state_dot(&theme, &presentation)),
                "kind" => line.child(
                    div()
                        .flex_none()
                        .text_color(theme.muted)
                        .child(kind_icon(info.kind)),
                ),
                "title" => line
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(info.title.clone())),
                    )
                    .children(badge.take()),
                "placement" => match crate::sessions::placement_badge(info) {
                    Some(badge) => line.child(placement_badge(
                        &theme,
                        &badge,
                        BadgeSite::Row,
                        info.id,
                        &self.env.regions,
                        cx,
                    )),
                    None => {
                        let place = placement(info);
                        if place.is_empty() {
                            line
                        } else {
                            line.child(
                                div()
                                    .flex_none()
                                    .max_w(px(80.0))
                                    .truncate()
                                    .px(px(tokens.spacing))
                                    .rounded(px(3.0))
                                    .border_1()
                                    .border_color(theme.border)
                                    .text_size(px(tokens.section_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(place)),
                            )
                        }
                    }
                },
                "age" => line.child(
                    div()
                        .flex_none()
                        .text_size(px(tokens.section_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(age_label(info.created_at_ms))),
                ),
                _ => line,
            };
        }

        let mut row = div()
            .id(SharedString::from(format!("session-row-{id}")))
            .flex()
            .flex_col()
            .mx(px(tokens.spacing))
            .px(px(tokens.spacing))
            .rounded(px(tokens.radius - 1.0))
            .cursor_pointer()
            .when(selected, move |s| s.bg(accent.opacity(0.18)))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |_, _, window, cx| {
                window.dispatch_action(Box::new(OpenSession(open_id.clone())), cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    window.dispatch_action(
                        Box::new(ShowSessionMenu(SessionMenuAt {
                            session: menu_id.clone(),
                            position: event.position,
                        })),
                        cx,
                    );
                }),
            )
            .child(line);
        if in_needs_you {
            let subtitle = if presentation.detail.is_empty() {
                presentation.label.clone()
            } else {
                format!("{} · {}", presentation.label, presentation.detail)
            };
            row = row.child(
                div()
                    .pl(px(tokens.spacing * 4.0))
                    .pb(px(tokens.spacing))
                    .text_size(px(tokens.section_font_size))
                    .text_color(tone_color(&theme, presentation.tone))
                    .child(SharedString::from(subtitle)),
            );
        }
        row.into_any_element()
    }
}
