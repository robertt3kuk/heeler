//! The `tasks` panel (M7-C): the tasks of the workspace as a dependency tree — state glyph and
//! color, title, placement badge (`main` / `wt <branch>`), run kind, elapsed time, outcome — with
//! the actions the daemon allows per state (Start / Done / Cancel / Reset / Delete) on the
//! selected row, an inline "+ New task" form, and the `tasks.*` filters.
//!
//! The daemon owns tasks (`task.*`, protocol 0.16): [`SessionsStore`](crate::sessions::SessionsStore)
//! lists them and follows `task.updated` / `task.removed`; this panel draws
//! [`tree_rows`] and sends the requests. A click on a row focuses the task's session
//! (`OpenSession`); "Changes" opens the review of its checkout (`ReviewTaskChanges`).

use std::collections::BTreeSet;
use std::time::Duration;

use gpui::{
    AnyElement, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Task, Window, div, prelude::*, px,
};
use hutan_client::ConnectionState;
use hutan_core::{TaskId, TaskState};
use hutan_protocol::task::TaskUpdate;

use super::common::{edit_text, field_box, now_ms, panel_header_with, tone_color};
use super::history::pill;
use crate::sessions::tasks::{
    NewTask, RunChoice, TaskAction, TaskFilters, TaskRowView, build_create, tree_rows,
};
use crate::settings::{TasksScope, TasksSettings};
use crate::theme::{DensityExt as _, Theme, Tokens, reduce_motion};
use crate::workbench::actions::{Notify, OpenSession, ReviewTaskChanges};
use crate::workbench::chat::TextInput;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::ui::{self, Mascot, StyledExt as _};
use crate::workbench::{Region, style::UiStyle};

/// How often the elapsed times refresh while the panel is drawn and a task runs.
pub const TICK: Duration = Duration::from_secs(1);

/// The text fields of the new-task form, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormField {
    Title,
    Prompt,
    Command,
    Branch,
}

impl FormField {
    fn key(self) -> &'static str {
        match self {
            FormField::Title => "title",
            FormField::Prompt => "prompt",
            FormField::Command => "command",
            FormField::Branch => "branch",
        }
    }
}

/// The open new-task form.
#[derive(Debug)]
struct Form {
    title: TextInput,
    prompt: TextInput,
    command: TextInput,
    branch: TextInput,
    after: BTreeSet<TaskId>,
    worktree: bool,
    run: RunChoice,
    auto_start: bool,
    field: FormField,
    error: Option<String>,
    submitting: bool,
}

impl Form {
    fn new() -> Self {
        Self {
            title: TextInput::new(),
            prompt: TextInput::new(),
            command: TextInput::new(),
            branch: TextInput::new(),
            after: BTreeSet::new(),
            worktree: false,
            run: RunChoice::Terminal,
            auto_start: false,
            field: FormField::Title,
            error: None,
            submitting: false,
        }
    }

    fn input_mut(&mut self, field: FormField) -> &mut TextInput {
        match field {
            FormField::Title => &mut self.title,
            FormField::Prompt => &mut self.prompt,
            FormField::Command => &mut self.command,
            FormField::Branch => &mut self.branch,
        }
    }

    /// The fields the form shows now, in order.
    fn fields(&self) -> Vec<FormField> {
        let mut fields = vec![FormField::Title, FormField::Prompt];
        if self.run == RunChoice::Terminal {
            fields.push(FormField::Command);
        }
        if self.worktree {
            fields.push(FormField::Branch);
        }
        fields
    }

    fn values(&self) -> NewTask {
        NewTask {
            title: self.title.text().to_owned(),
            prompt: self.prompt.text().to_owned(),
            after: self.after.iter().copied().collect(),
            worktree: self.worktree,
            branch: self.branch.text().to_owned(),
            run: self.run,
            command: self.command.text().to_owned(),
            auto_start: self.auto_start,
        }
    }
}

/// A delete the daemon refused because other tasks depend on the task: the panel asks whether to
/// delete them too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeAsk {
    pub task: TaskId,
    pub message: String,
}

pub struct TasksPanel {
    env: PanelEnv,
    form: Option<Form>,
    selected: Option<TaskId>,
    cascade: Option<CascadeAsk>,
    focus: [(FormField, FocusHandle); 4],
    focus_wanted: Option<FormField>,
    /// Runs while the panel is drawn and a listed task runs.
    ticker: Option<Task<()>>,
    drawn: bool,
}

impl std::fmt::Debug for TasksPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TasksPanel")
            .field("form", &self.form.is_some())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl TasksPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |_, _, cx| cx.notify()).detach();
        Self {
            env,
            form: None,
            selected: None,
            cascade: None,
            focus: [
                (FormField::Title, cx.focus_handle()),
                (FormField::Prompt, cx.focus_handle()),
                (FormField::Command, cx.focus_handle()),
                (FormField::Branch, cx.focus_handle()),
            ],
            focus_wanted: None,
            ticker: None,
            drawn: false,
        }
    }

    fn handle(&self, field: FormField) -> &FocusHandle {
        self.focus
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, h)| h)
            .unwrap_or(&self.focus[0].1)
    }

    // ---- read access (also for tests) -----------------------------------------------------

    fn filters(&self, cx: &gpui::App) -> TaskFilters {
        let settings = self.env.settings.get::<TasksSettings>();
        TaskFilters {
            show_done: settings.show_done,
            workspace: match settings.scope {
                TasksScope::All => None,
                TasksScope::Workspace => self.env.sessions.read(cx).active_workspace_id(),
            },
        }
    }

    /// The listed tasks, as the tree draws them.
    pub fn rows(&self, now_ms: u64, cx: &gpui::App) -> Vec<TaskRowView> {
        tree_rows(self.env.sessions.read(cx).tasks(), self.filters(cx), now_ms)
    }

    /// Whether the new-task form is open.
    pub fn form_open(&self) -> bool {
        self.form.is_some()
    }

    /// The error the open form shows.
    pub fn form_error(&self) -> Option<&str> {
        self.form.as_ref().and_then(|f| f.error.as_deref())
    }

    /// The row whose actions are shown.
    pub fn selected(&self) -> Option<TaskId> {
        self.selected
    }

    /// A delete waiting for the user's word about the tasks that depend on its task.
    pub fn cascade_ask(&self) -> Option<&CascadeAsk> {
        self.cascade.as_ref()
    }

    /// Whether the durations are ticking right now (tests).
    pub fn ticking(&self) -> bool {
        self.ticker.is_some()
    }

    // ---- the form -------------------------------------------------------------------------

    /// Opens the new-task form and puts the caret in its title (⌘K "Tasks: New Task…").
    pub fn open_form(&mut self, cx: &mut Context<Self>) {
        if self.form.is_none() {
            self.form = Some(Form::new());
        }
        self.focus_wanted = Some(FormField::Title);
        cx.notify();
    }

    pub fn close_form(&mut self, cx: &mut Context<Self>) {
        self.form = None;
        cx.notify();
    }

    /// Replaces the text of a form field.
    pub fn set_field(&mut self, field: FormField, text: &str, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            form.input_mut(field).set(text);
            form.error = None;
            cx.notify();
        }
    }

    /// Picks what the new task runs.
    pub fn set_run(&mut self, run: RunChoice, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            form.run = run;
            if !form.fields().contains(&form.field) {
                form.field = FormField::Title;
            }
            cx.notify();
        }
    }

    pub fn set_worktree(&mut self, worktree: bool, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            form.worktree = worktree;
            if !form.fields().contains(&form.field) {
                form.field = FormField::Title;
            }
            cx.notify();
        }
    }

    pub fn set_auto_start(&mut self, auto_start: bool, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            form.auto_start = auto_start;
            cx.notify();
        }
    }

    /// Ticks or unticks a task of the "After" list.
    pub fn toggle_dependency(&mut self, task: TaskId, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form
            && !form.after.remove(&task)
        {
            form.after.insert(task);
        }
        cx.notify();
    }

    /// The dependencies ticked in the form.
    pub fn form_dependencies(&self) -> Vec<TaskId> {
        self.form
            .as_ref()
            .map(|f| f.after.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Sends the form as `task.create`; the form closes when the daemon accepts it and shows the
    /// daemon's reason when it does not.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.form else {
            return;
        };
        if form.submitting {
            return;
        }
        let store = self.env.sessions.read(cx);
        let Some(workspace) = store
            .active_workspace_id()
            .or_else(|| store.home_workspace().map(|w| w.id))
        else {
            form.error = Some("Open a workspace first.".to_owned());
            cx.notify();
            return;
        };
        let request = match build_create(workspace, &form.values()) {
            Ok(request) => request,
            Err(error) => {
                form.error = Some(error);
                cx.notify();
                return;
            }
        };
        form.submitting = true;
        form.error = None;
        let creating = self
            .env
            .sessions
            .update(cx, |store, cx| store.create_task(request, cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = creating.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(task) => {
                        this.form = None;
                        this.selected = Some(task.id);
                    }
                    Err(error) => {
                        if let Some(form) = &mut this.form {
                            form.submitting = false;
                            form.error = Some(error);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- row actions ----------------------------------------------------------------------

    /// Selects a task's row and focuses its session, when it has one.
    pub fn click_row(&mut self, task: TaskId, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = if self.selected == Some(task) {
            None
        } else {
            Some(task)
        };
        if let Some(session) = self
            .env
            .sessions
            .read(cx)
            .tasks()
            .get(task)
            .and_then(|t| t.sessions.last().copied())
        {
            window.dispatch_action(Box::new(OpenSession(session.to_string())), cx);
        }
        cx.notify();
    }

    /// Sends the request an action stands for.
    pub fn run_action(
        &mut self,
        task: TaskId,
        action: TaskAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = |state| TaskUpdate {
            id: task,
            title: None,
            prompt: None,
            depends_on: None,
            placement: None,
            run: None,
            auto_start: None,
            state: Some(state),
        };
        match action {
            TaskAction::Start => {
                let starting = self
                    .env
                    .sessions
                    .update(cx, |store, cx| store.start_task(task, cx));
                cx.spawn_in(window, async move |_, cx| {
                    let message = match starting.await {
                        Ok(started) => {
                            cx.update(|window, cx| {
                                window.dispatch_action(
                                    Box::new(OpenSession(started.session.id.to_string())),
                                    cx,
                                );
                            })
                            .ok();
                            started.warnings.first().cloned()
                        }
                        Err(error) => Some(format!("Cannot start the task: {error}")),
                    };
                    notify(cx, message);
                })
                .detach();
            }
            TaskAction::Done | TaskAction::Cancel | TaskAction::Reset => {
                let to = match action {
                    TaskAction::Done => TaskState::Done,
                    TaskAction::Cancel => TaskState::Cancelled,
                    _ => TaskState::Todo,
                };
                let updating = self
                    .env
                    .sessions
                    .update(cx, |store, cx| store.update_task(state(to), cx));
                cx.spawn_in(window, async move |_, cx| {
                    if let Err(error) = updating.await {
                        notify(cx, Some(format!("Cannot change the task: {error}")));
                    }
                })
                .detach();
            }
            TaskAction::Delete => self.delete(task, false, window, cx),
        }
    }

    fn delete(&mut self, task: TaskId, cascade: bool, window: &mut Window, cx: &mut Context<Self>) {
        let deleting = self
            .env
            .sessions
            .update(cx, |store, cx| store.delete_task(task, cascade, cx));
        cx.spawn_in(window, async move |this, cx| {
            match deleting.await {
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        if this.selected == Some(task) {
                            this.selected = None;
                        }
                        this.cascade = None;
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) if !cascade && error.contains("Conflict") => {
                    this.update(cx, |this, cx| {
                        this.cascade = Some(CascadeAsk {
                            task,
                            message: error
                                .split_once(": ")
                                .map_or(error.clone(), |(_, rest)| rest.to_owned()),
                        });
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) => notify(cx, Some(format!("Cannot delete the task: {error}"))),
            };
        })
        .detach();
    }

    /// Answers the cascade question: delete the task and every task depending on it, or leave
    /// everything as it is.
    pub fn answer_cascade(&mut self, delete: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ask) = self.cascade.take() else {
            return;
        };
        if delete {
            self.delete(ask.task, true, window, cx);
        }
        cx.notify();
    }

    // ---- keyboard and focus ---------------------------------------------------------------

    fn on_key(
        &mut self,
        field: FormField,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        let multiline = field == FormField::Prompt;
        match ks.key.as_str() {
            "escape" => self.close_form(cx),
            "tab" => {
                let Some(form) = &mut self.form else {
                    return;
                };
                let fields = form.fields();
                let at = fields.iter().position(|f| *f == field).unwrap_or(0);
                let next = if ks.modifiers.shift {
                    fields[(at + fields.len() - 1) % fields.len()]
                } else {
                    fields[(at + 1) % fields.len()]
                };
                form.field = next;
                self.focus_wanted = Some(next);
                cx.notify();
            }
            "enter" if ks.modifiers.platform || !multiline => self.submit(window, cx),
            _ => {
                let Some(form) = &mut self.form else {
                    return;
                };
                if !edit_text(form.input_mut(field), event, multiline, cx) {
                    return;
                }
                form.error = None;
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    fn focus_field(&mut self, field: FormField, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            form.field = field;
        }
        self.focus_wanted = Some(field);
        cx.notify();
    }

    fn write_setting(&self, key: &str, value: toml::Value) {
        if let Err(err) = self.env.settings.write_user(key, value) {
            tracing::warn!("cannot save {key}: {err}");
        }
    }

    // ---- ticking --------------------------------------------------------------------------

    fn sync_ticker(&mut self, running: bool, cx: &mut Context<Self>) {
        self.drawn = true;
        if !running {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let keep = this
                    .update(cx, |this, cx| {
                        if !std::mem::take(&mut this.drawn) {
                            this.ticker = None;
                            return false;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }
}

/// Shows `message` as the workbench's notice.
fn notify(cx: &mut gpui::AsyncWindowContext, message: Option<String>) {
    if let Some(message) = message {
        cx.update(|window, cx| {
            window.dispatch_action(Box::new(Notify(message)), cx);
        })
        .ok();
    }
}

impl Focusable for TasksPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus[0].1.clone()
    }
}

// ---- rendering ----------------------------------------------------------------------------

/// A compact text button for rows and forms.
fn small_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: ui::ButtonKind,
    enabled: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    ui::button(id.into(), label, kind, enabled, theme, tokens)
        .h(px(tokens.font_size + 8.0))
        .px(px(tokens.spacing * 2.0))
        .text_size(px(tokens.secondary_font_size))
}

impl Render for TasksPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<TasksSettings>();
        let now = now_ms();
        let rows = self.rows(now, cx);
        let running = rows.iter().any(|r| r.state.is_active());
        self.sync_ticker(running, cx);
        let (connected, supported, harness, nothing_at_all) = {
            let store = self.env.sessions.read(cx);
            (
                store.connection_state() == ConnectionState::Connected,
                store.connection().supports_tasks(),
                store.connection().supports_harness_chat(),
                store.tasks().is_empty(),
            )
        };
        if let Some(field) = self.focus_wanted.take() {
            let handle = self.handle(field).clone();
            cx.defer_in(window, move |_, window, cx| window.focus(&handle, cx));
        }

        let new_button = small_button(
            "tasks-new",
            "+ New task",
            ui::ButtonKind::Secondary,
            connected && supported,
            &theme,
            &tokens,
        )
        .relative()
        .when(connected && supported, |s| {
            s.on_click(cx.listener(|this, _, _, cx| this.open_form(cx)))
        })
        .child(probe(&self.env.regions, Region::Tasks("new")))
        .into_any_element();

        let chips = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .pb(px(tokens.spacing))
            .child(
                pill(
                    "tasks-chip-done",
                    "Done",
                    settings.show_done,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener({
                    let value = !settings.show_done;
                    move |this, _, _, cx| {
                        this.write_setting("tasks.show_done", toml::Value::Boolean(value));
                        cx.notify();
                    }
                }))
                .child(probe(&self.env.regions, Region::Tasks("chip-done"))),
            )
            .child(
                pill(
                    "tasks-chip-scope",
                    match settings.scope {
                        TasksScope::Workspace => "This workspace",
                        TasksScope::All => "All workspaces",
                    },
                    settings.scope == TasksScope::All,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener({
                    let next = match settings.scope {
                        TasksScope::Workspace => TasksScope::All,
                        TasksScope::All => TasksScope::Workspace,
                    };
                    move |this, _, _, cx| {
                        this.write_setting(
                            "tasks.scope",
                            toml::Value::String(next.key().to_owned()),
                        );
                        cx.notify();
                    }
                }))
                .child(probe(&self.env.regions, Region::Tasks("chip-scope"))),
            );

        let mut list = div()
            .id("tasks-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        if rows.is_empty() && self.form.is_none() {
            let (text, action) = if !connected {
                ("Not connected to hutand.", None)
            } else if !supported {
                (
                    "This hutand does not know tasks; restart it (Daemon → Restart) to update.",
                    None,
                )
            } else if nothing_at_all {
                (
                    "No tasks yet. Plan work as tasks with dependencies; Hutan starts what is ready.",
                    Some(
                        small_button(
                            "tasks-empty-new",
                            "+ New task",
                            ui::ButtonKind::Primary,
                            true,
                            &theme,
                            &tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, _, cx| this.open_form(cx)))
                        .child(probe(&self.env.regions, Region::Tasks("empty-new")))
                        .into_any_element(),
                    ),
                )
            } else {
                ("Nothing matches the filters.", None)
            };
            let mascot = if nothing_at_all {
                Mascot::Empty
            } else {
                Mascot::Success
            };
            list = list.child(ui::empty_state(mascot, text, action, &theme, &tokens).h(px(280.0)));
        }
        let pulse_dim = !reduce_motion() && (now / 1000) % 2 == 1;
        for (index, row) in rows.iter().enumerate() {
            list = list.child(self.render_row(index, row, pulse_dim, &theme, &tokens, cx));
        }

        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header_with("Tasks", "tasks", vec![new_button], cx))
            .child(chips);
        if let Some(ask) = self.cascade.clone() {
            root = root.child(self.render_cascade(&ask, &theme, &tokens, cx));
        }
        if self.form.is_some() {
            root = root.child(self.render_form(harness, &theme, &tokens, window, cx));
        }
        root.child(list)
    }
}

impl TasksPanel {
    fn render_cascade(
        &self,
        ask: &CascadeAsk,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing))
            .p(px(tokens.spacing * 2.0))
            .card(theme, tokens)
            .child(
                div()
                    .text_color(theme.warning)
                    .child(SharedString::from(ask.message.clone())),
            )
            .child(
                div()
                    .flex()
                    .gap(px(tokens.spacing))
                    .child(
                        small_button(
                            "tasks-cascade-yes",
                            "Delete them too",
                            ui::ButtonKind::Primary,
                            true,
                            theme,
                            tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.answer_cascade(true, window, cx);
                        }))
                        .child(probe(&self.env.regions, Region::Tasks("cascade-yes"))),
                    )
                    .child(
                        small_button(
                            "tasks-cascade-no",
                            "Keep",
                            ui::ButtonKind::Ghost,
                            true,
                            theme,
                            tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.answer_cascade(false, window, cx);
                        }))
                        .child(probe(&self.env.regions, Region::Tasks("cascade-no"))),
                    ),
            )
            .into_any_element()
    }

    fn render_row(
        &self,
        index: usize,
        row: &TaskRowView,
        pulse_dim: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id;
        let hover = theme.surface_hover;
        let selected = self.selected == Some(id);
        let color = tone_color(theme, row.tone);
        let glyph_color = if row.state == TaskState::Running && pulse_dim {
            color.opacity(0.4)
        } else {
            color
        };
        let indent = row.depth as f32 * tokens.spacing * 3.0;
        let finished = matches!(row.state, TaskState::Done | TaskState::Cancelled);

        let mut detail = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(tokens.spacing))
            .pl(px(12.0 + tokens.spacing * 1.5))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(ui::pill(row.placement.clone(), theme.muted, tokens))
            .child(SharedString::from(row.run.clone()));
        if row.auto_start {
            detail = detail.child("auto");
        }
        if let Some(elapsed) = &row.elapsed {
            detail = detail.child(SharedString::from(elapsed.clone()));
        }
        if let Some(outcome) = &row.outcome {
            detail = detail.child(
                div()
                    .text_color(if row.state == TaskState::Failed {
                        theme.danger
                    } else {
                        theme.muted
                    })
                    .child(SharedString::from(format!("({outcome})"))),
            );
        }
        if !row.also_after.is_empty() {
            detail = detail.child(SharedString::from(format!(
                "also after {}",
                row.also_after.join(", ")
            )));
        }

        let mut element = div()
            .id(SharedString::from(format!("tasks-row-{index}")))
            .relative()
            .flex()
            .flex_col()
            .mx(px(tokens.spacing))
            .pl(px(tokens.spacing + indent))
            .pr(px(tokens.spacing))
            .py(px(2.0))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .when(selected, |s| s.bg(theme.accent_soft))
            .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
            .when(finished, |s| s.opacity(0.6))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.click_row(id, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .h(px(tokens.sidebar_row - 4.0))
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .text_color(glyph_color)
                            .child(row.glyph),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .medium()
                            .child(SharedString::from(row.title.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(color)
                            .child(SharedString::from(row.state.to_string())),
                    ),
            )
            .child(detail);
        if selected {
            let mut actions = div()
                .flex()
                .flex_wrap()
                .gap(px(tokens.spacing))
                .pl(px(12.0 + tokens.spacing * 1.5))
                .py(px(tokens.spacing));
            for action in &row.actions {
                let action = *action;
                actions = actions.child(
                    small_button(
                        SharedString::from(format!("tasks-action-{index}-{}", action.key())),
                        action.label(),
                        if action == TaskAction::Start {
                            ui::ButtonKind::Primary
                        } else {
                            ui::ButtonKind::Secondary
                        },
                        true,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_action(id, action, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(probe(
                        &self.env.regions,
                        Region::TaskAction(index, action.key()),
                    )),
                );
            }
            if row.reviewable {
                actions = actions.child(
                    small_button(
                        SharedString::from(format!("tasks-action-{index}-changes")),
                        "Changes",
                        ui::ButtonKind::Ghost,
                        true,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(move |_, _, window, cx| {
                        window.dispatch_action(Box::new(ReviewTaskChanges(id.to_string())), cx);
                        cx.stop_propagation();
                    }))
                    .child(probe(
                        &self.env.regions,
                        Region::TaskAction(index, "changes"),
                    )),
                );
            }
            element = element.child(actions);
        }
        element
            .child(probe(&self.env.regions, Region::TaskRow(index)))
            .into_any_element()
    }

    fn render_form(
        &self,
        harness: bool,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(form) = &self.form else {
            return div().into_any_element();
        };
        let label = |text: &'static str| ui::section_label(text, theme, tokens);
        let field = |which: FormField,
                     input: &TextInput,
                     placeholder: &str,
                     multiline: bool,
                     cx: &mut Context<Self>| {
            let handle = self.handle(which).clone();
            let focused = handle.is_focused(window);
            field_box(
                SharedString::from(format!("tasks-field-{}", which.key())),
                input,
                placeholder,
                focused,
                multiline,
                theme,
                tokens,
            )
            .relative()
            .track_focus(&handle)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.on_key(which, event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    // The dock takes focus on this same mouse down; ask again after it.
                    this.focus_field(which, cx);
                }),
            )
            .child(probe(
                &self.env.regions,
                Region::Tasks(match which {
                    FormField::Title => "form-title",
                    FormField::Prompt => "form-prompt",
                    FormField::Command => "form-command",
                    FormField::Branch => "form-branch",
                }),
            ))
            .into_any_element()
        };

        let mut run_pills = div().flex().flex_wrap().gap(px(tokens.spacing));
        let mut choices = vec![RunChoice::Terminal, RunChoice::Codex];
        if harness {
            choices.extend([RunChoice::Omp, RunChoice::Claude]);
        }
        for choice in choices {
            run_pills = run_pills.child(
                pill(
                    SharedString::from(format!("tasks-run-{}", choice.key())),
                    choice.label(),
                    form.run == choice,
                    theme,
                    tokens,
                )
                .relative()
                .on_click(cx.listener(move |this, _, _, cx| this.set_run(choice, cx)))
                .child(probe(
                    &self.env.regions,
                    Region::Tasks(match choice {
                        RunChoice::Terminal => "run-terminal",
                        RunChoice::Codex => "run-codex",
                        RunChoice::Omp => "run-omp",
                        RunChoice::Claude => "run-claude",
                    }),
                )),
            );
        }
        let place_pills = div()
            .flex()
            .gap(px(tokens.spacing))
            .child(
                pill(
                    "tasks-place-main",
                    "Main checkout",
                    !form.worktree,
                    theme,
                    tokens,
                )
                .relative()
                .on_click(cx.listener(|this, _, _, cx| this.set_worktree(false, cx)))
                .child(probe(&self.env.regions, Region::Tasks("place-main"))),
            )
            .child(
                pill(
                    "tasks-place-worktree",
                    "New worktree",
                    form.worktree,
                    theme,
                    tokens,
                )
                .relative()
                .on_click(cx.listener(|this, _, _, cx| this.set_worktree(true, cx)))
                .child(probe(&self.env.regions, Region::Tasks("place-worktree"))),
            );

        // The tasks of the workspace the new one can wait for: every one listed, as a flat list
        // (the form picks, the tree shows).
        let mut after = div()
            .id("tasks-after")
            .flex()
            .flex_col()
            .max_h(px(tokens.sidebar_row * 4.5))
            .overflow_y_scroll();
        let candidates: Vec<TaskRowView> = {
            let store = self.env.sessions.read(cx);
            let workspace = store
                .active_workspace_id()
                .or_else(|| store.home_workspace().map(|w| w.id));
            tree_rows(
                store.tasks(),
                TaskFilters {
                    show_done: true,
                    workspace,
                },
                0,
            )
        };
        if candidates.is_empty() {
            after = after.child(
                div()
                    .text_color(theme.faint)
                    .text_size(px(tokens.secondary_font_size))
                    .child("No tasks to wait for."),
            );
        }
        for (at, row) in candidates.into_iter().enumerate() {
            let task = row.id;
            let ticked = form.after.contains(&task);
            after = after.child(
                div()
                    .id(SharedString::from(format!("tasks-dep-{at}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .h(px(tokens.sidebar_row - 4.0))
                    .px(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme.surface_hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_dependency(task, cx)))
                    .child(if ticked { "☑" } else { "☐" })
                    .child(
                        div()
                            .truncate()
                            .child(SharedString::from(row.title.clone())),
                    )
                    .child(probe(&self.env.regions, Region::TaskDep(at))),
            );
        }

        let mut body = div()
            .id("tasks-form")
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing * 1.5))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 2.5))
            .card(theme, tokens)
            .child(label("New task"))
            .child(field(FormField::Title, &form.title, "Title", false, cx))
            .child(field(
                FormField::Prompt,
                &form.prompt,
                "Prompt (the first message of a chat)",
                true,
                cx,
            ))
            .child(label("Run"))
            .child(run_pills);
        if form.run == RunChoice::Terminal {
            body = body.child(field(
                FormField::Command,
                &form.command,
                "Command (empty: a login shell)",
                false,
                cx,
            ));
        }
        body = body.child(label("Where")).child(place_pills);
        if form.worktree {
            body = body.child(field(
                FormField::Branch,
                &form.branch,
                "Branch (empty: task/<slug of the title>)",
                false,
                cx,
            ));
        }
        body = body.child(label("After")).child(after).child(
            div()
                .id("tasks-auto")
                .relative()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .cursor_pointer()
                .on_click(cx.listener({
                    let value = !form.auto_start;
                    move |this, _, _, cx| this.set_auto_start(value, cx)
                }))
                .child(if form.auto_start { "☑" } else { "☐" })
                .child("Start automatically when ready")
                .child(probe(&self.env.regions, Region::Tasks("form-auto"))),
        );
        if let Some(error) = &form.error {
            body = body.child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone())),
            );
        }
        body.child(
            div()
                .flex()
                .justify_end()
                .gap(px(tokens.spacing))
                .child(
                    small_button(
                        "tasks-form-cancel",
                        "Cancel",
                        ui::ButtonKind::Ghost,
                        true,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| this.close_form(cx)))
                    .child(probe(&self.env.regions, Region::Tasks("form-cancel"))),
                )
                .child(
                    small_button(
                        "tasks-form-create",
                        if form.submitting {
                            "Creating…"
                        } else {
                            "Create"
                        },
                        ui::ButtonKind::Primary,
                        !form.submitting,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                    .child(probe(&self.env.regions, Region::Tasks("form-create"))),
                ),
        )
        .into_any_element()
    }
}
