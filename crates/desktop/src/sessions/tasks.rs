//! The desktop's copy of the daemon's tasks (protocol 0.16) and what the Tasks panel draws from
//! it: the dependency tree, state glyphs, row actions, elapsed time. No GPUI here.
//!
//! The daemon owns tasks. The store lists them once per connection (`task.list`) and then
//! follows `task.updated` / `task.removed`; [`TaskBook`] merges the two so an event that arrives
//! while the list is in flight is not undone by the older list.

use std::collections::{HashMap, HashSet};

use hutan_core::task::{PROMPT_MAX_BYTES, TITLE_MAX_CHARS, default_branch, topological_order};
use hutan_core::{
    SessionId, Task, TaskId, TaskPlacement, TaskPlacementKind, TaskRun, TaskState, WorkspaceId,
};
use hutan_protocol::task::TaskCreate;

use super::Tone;
use super::agents::format_duration;

/// Every task the daemon told this window about.
#[derive(Debug, Clone, Default)]
pub struct TaskBook {
    tasks: HashMap<TaskId, Task>,
    /// A `task.list` answered since the connection came up.
    loaded: bool,
    /// Tasks an event changed or removed since the last list was asked for: the list's version
    /// of them is older than the event.
    touched: HashSet<TaskId>,
}

impl TaskBook {
    /// A `task.list` is about to be asked for.
    pub fn start_loading(&mut self) {
        self.touched.clear();
    }

    /// The answer of `task.list`: adds what is missing and replaces what no event changed
    /// since; forgets tasks the daemon no longer lists.
    pub fn merge_listed(&mut self, listed: Vec<Task>) {
        let ids: HashSet<TaskId> = listed.iter().map(|t| t.id).collect();
        self.tasks
            .retain(|id, _| ids.contains(id) || self.touched.contains(id));
        for task in listed {
            if !self.touched.contains(&task.id) {
                self.tasks.insert(task.id, task);
            }
        }
        self.loaded = true;
    }

    /// `task.updated`.
    pub fn updated(&mut self, task: Task) {
        self.touched.insert(task.id);
        self.tasks.insert(task.id, task);
    }

    /// `task.removed`.
    pub fn removed(&mut self, id: TaskId) {
        self.touched.insert(id);
        self.tasks.remove(&id);
    }

    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// A list answered on this connection.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Every task, dependencies first, otherwise oldest first.
    pub fn ordered(&self) -> Vec<&Task> {
        topological_order(self.tasks.values())
            .into_iter()
            .filter_map(|id| self.tasks.get(&id))
            .collect()
    }
}

/// What the panel offers to do with a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskAction {
    Start,
    Done,
    Cancel,
    Reset,
    Delete,
}

impl TaskAction {
    pub fn label(self) -> &'static str {
        match self {
            TaskAction::Start => "Start",
            TaskAction::Done => "Done",
            TaskAction::Cancel => "Cancel",
            TaskAction::Reset => "Reset",
            TaskAction::Delete => "Delete",
        }
    }

    /// The element-id suffix and the name tests click by.
    pub fn key(self) -> &'static str {
        match self {
            TaskAction::Start => "start",
            TaskAction::Done => "done",
            TaskAction::Cancel => "cancel",
            TaskAction::Reset => "reset",
            TaskAction::Delete => "delete",
        }
    }
}

/// The actions the daemon accepts for a task shown in `state` (`docs/protocol.md` § Tasks):
/// start needs `ready`; done is allowed from everything but `cancelled`; cancel from every live
/// state; reset from every ended one (and `todo`, which it leaves alone); a running task cannot
/// be deleted.
pub fn actions_for(state: TaskState) -> Vec<TaskAction> {
    match state {
        TaskState::Ready => vec![
            TaskAction::Start,
            TaskAction::Done,
            TaskAction::Cancel,
            TaskAction::Delete,
        ],
        TaskState::Todo => vec![TaskAction::Done, TaskAction::Cancel, TaskAction::Delete],
        TaskState::Running | TaskState::Blocked => vec![TaskAction::Done, TaskAction::Cancel],
        TaskState::Failed => vec![TaskAction::Done, TaskAction::Reset, TaskAction::Delete],
        TaskState::Done | TaskState::Cancelled => vec![TaskAction::Reset, TaskAction::Delete],
    }
}

/// The glyph, what it says, and its tone, as `hutan task ls` draws them.
pub fn state_glyph(state: TaskState) -> (&'static str, Tone) {
    match state {
        TaskState::Todo => ("○", Tone::Muted),
        TaskState::Ready => ("◎", Tone::Accent),
        TaskState::Running => ("▶", Tone::Accent),
        TaskState::Blocked => ("◆", Tone::Warning),
        TaskState::Done => ("✓", Tone::Success),
        TaskState::Failed => ("✗", Tone::Error),
        TaskState::Cancelled => ("⊘", Tone::Muted),
    }
}

/// What the panel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskFilters {
    /// List tasks that are `done` or `cancelled` (failed ones always stay: they need a look).
    pub show_done: bool,
    /// Only tasks of this workspace; `None`: every workspace.
    pub workspace: Option<WorkspaceId>,
}

impl TaskFilters {
    fn keeps(&self, task: &Task) -> bool {
        self.workspace.is_none_or(|w| w == task.workspace_id)
            && (self.show_done || !matches!(task.state, TaskState::Done | TaskState::Cancelled))
    }
}

/// One listed task, as the panel draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRowView {
    pub id: TaskId,
    pub workspace: WorkspaceId,
    pub depth: usize,
    pub title: String,
    pub state: TaskState,
    pub glyph: &'static str,
    pub tone: Tone,
    /// `main` or `wt <branch>`.
    pub placement: String,
    /// The branch of a worktree placement, when it is a worktree task.
    pub branch: Option<String>,
    /// `shell`, `$ cargo test`, `codex chat`.
    pub run: String,
    /// Running time, or how long it took; none before it started.
    pub elapsed: Option<String>,
    /// Why it ended.
    pub outcome: Option<String>,
    /// The other tasks it waits for besides its parent in the tree.
    pub also_after: Vec<String>,
    pub auto_start: bool,
    pub actions: Vec<TaskAction>,
    /// The session that runs it, or ran it last.
    pub session: Option<SessionId>,
    /// It has (or had) a checkout worth reviewing: a worktree task, or one that ran.
    pub reviewable: bool,
}

/// `wt <branch>` / `main`, the branch for a worktree task (its default when the task names none).
pub fn placement_label(task: &Task) -> (String, Option<String>) {
    match task.placement.kind {
        TaskPlacementKind::Main => ("main".to_owned(), None),
        TaskPlacementKind::Worktree => {
            let branch = task
                .placement
                .branch
                .clone()
                .unwrap_or_else(|| default_branch(&task.title));
            (format!("wt {branch}"), Some(branch))
        }
    }
}

/// The run of a task in a few words.
pub fn run_label(run: Option<&TaskRun>) -> String {
    match run {
        None => "shell".to_owned(),
        Some(TaskRun::Terminal { argv }) => {
            let line = argv.join(" ");
            let mut short: String = line.chars().take(40).collect();
            if line.chars().count() > 40 {
                short.push('…');
            }
            format!("$ {short}")
        }
        Some(TaskRun::Chat { agent }) => format!("{agent} chat"),
    }
}

fn elapsed(task: &Task, now_ms: u64) -> Option<String> {
    let started = task.started_at_ms?;
    let end = if task.state.is_active() {
        now_ms
    } else {
        task.ended_at_ms?
    };
    Some(format_duration(end.saturating_sub(started)))
}

/// The tasks to list as a dependency tree: every task once, under the dependency it waits for
/// last (the others are named in `also_after`), dependencies before dependants, roots oldest
/// first. A task whose dependencies are all filtered out is a root.
pub fn tree_rows(book: &TaskBook, filters: TaskFilters, now_ms: u64) -> Vec<TaskRowView> {
    let visible: Vec<&Task> = book
        .ordered()
        .into_iter()
        .filter(|t| filters.keeps(t))
        .collect();
    let position: HashMap<TaskId, usize> = visible
        .iter()
        .enumerate()
        .map(|(at, t)| (t.id, at))
        .collect();
    let mut children: HashMap<TaskId, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    let mut also: HashMap<TaskId, Vec<String>> = HashMap::new();
    for (at, task) in visible.iter().enumerate() {
        let mut deps: Vec<TaskId> = task
            .depends_on
            .iter()
            .copied()
            .filter(|d| position.get(d).is_some_and(|p| *p < at))
            .collect();
        deps.sort_by_key(|d| position[d]);
        deps.dedup();
        match deps.pop() {
            Some(parent) => {
                children.entry(parent).or_default().push(at);
                let others: Vec<String> = deps
                    .iter()
                    .filter_map(|d| book.get(*d))
                    .map(|d| d.title.clone())
                    .collect();
                if !others.is_empty() {
                    also.insert(task.id, others);
                }
            }
            None => roots.push(at),
        }
    }
    let mut rows = Vec::with_capacity(visible.len());
    // Depth-first, an explicit stack: the dependency chains of a plan can be long.
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|at| (at, 0)).collect();
    while let Some((at, depth)) = stack.pop() {
        let task = visible[at];
        let (glyph, tone) = state_glyph(task.state);
        let (placement, branch) = placement_label(task);
        rows.push(TaskRowView {
            id: task.id,
            workspace: task.workspace_id,
            depth,
            title: task.title.clone(),
            state: task.state,
            glyph,
            tone,
            placement,
            reviewable: branch.is_some() || !task.sessions.is_empty(),
            branch,
            run: run_label(task.run.as_ref()),
            elapsed: elapsed(task, now_ms),
            outcome: task.outcome.clone(),
            also_after: also.remove(&task.id).unwrap_or_default(),
            auto_start: task.auto_start,
            actions: actions_for(task.state),
            session: task.sessions.last().copied(),
        });
        for child in children
            .remove(&task.id)
            .unwrap_or_default()
            .into_iter()
            .rev()
        {
            stack.push((child, depth + 1));
        }
    }
    rows
}

/// What a new task runs, as the form offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunChoice {
    /// A terminal: the command, or the login shell when there is none.
    Terminal,
    Codex,
    /// An `omp` chat (the ACP harness, daemon capability `harness.acp`).
    Omp,
}

impl RunChoice {
    pub fn label(self) -> &'static str {
        match self {
            RunChoice::Terminal => "Terminal",
            RunChoice::Codex => "Codex chat",
            RunChoice::Omp => "omp chat",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            RunChoice::Terminal => "terminal",
            RunChoice::Codex => "codex",
            RunChoice::Omp => "omp",
        }
    }

    /// The agent of a chat run.
    fn agent(self) -> Option<&'static str> {
        match self {
            RunChoice::Terminal => None,
            RunChoice::Codex => Some("codex"),
            RunChoice::Omp => Some("omp"),
        }
    }
}

/// The values of the new-task form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    pub title: String,
    pub prompt: String,
    pub after: Vec<TaskId>,
    /// Run in a worktree of `branch` instead of the main checkout.
    pub worktree: bool,
    /// The worktree's branch; empty: the daemon's default (`task/<slug of the title>`).
    pub branch: String,
    pub run: RunChoice,
    /// The terminal command; empty: a login shell.
    pub command: String,
    pub auto_start: bool,
}

/// The `task.create` request for `form`, or what is wrong with it (the daemon checks again).
pub fn build_create(workspace: WorkspaceId, form: &NewTask) -> Result<TaskCreate, String> {
    let title = form.title.trim();
    if title.is_empty() {
        return Err("A task needs a title.".to_owned());
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(format!(
            "The title is longer than {TITLE_MAX_CHARS} characters."
        ));
    }
    let prompt = form.prompt.trim();
    if prompt.len() > PROMPT_MAX_BYTES {
        return Err("The prompt is longer than 64 KiB.".to_owned());
    }
    let run = match form.run.agent() {
        Some(agent) => {
            if prompt.is_empty() {
                return Err(format!(
                    "A {} needs a prompt: it is the first message.",
                    form.run.label()
                ));
            }
            Some(TaskRun::Chat {
                agent: agent.to_owned(),
            })
        }
        None => {
            let argv = split_command(&form.command)?;
            (!argv.is_empty()).then_some(TaskRun::Terminal { argv })
        }
    };
    let branch = form.branch.trim();
    let placement = form.worktree.then(|| TaskPlacement {
        kind: TaskPlacementKind::Worktree,
        branch: (!branch.is_empty()).then(|| branch.to_owned()),
        base: None,
    });
    Ok(TaskCreate {
        workspace_id: workspace,
        title: title.to_owned(),
        prompt: (!prompt.is_empty()).then(|| prompt.to_owned()),
        depends_on: form.after.clone(),
        placement,
        run,
        auto_start: form.auto_start,
    })
}

/// A command line as words: whitespace separates, single and double quotes group, a backslash
/// escapes the next character outside single quotes. No expansion of any kind.
pub fn split_command(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => word.push(c),
            (_, '\\') => match chars.next() {
                Some(next) => {
                    word.push(next);
                    started = true;
                }
                None => return Err("The command ends with a backslash.".to_owned()),
            },
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("The command has an unclosed quote.".to_owned());
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(title: &str, age: u64, state: TaskState, deps: &[&Task]) -> Task {
        Task {
            id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            title: title.to_owned(),
            prompt: None,
            state,
            depends_on: deps.iter().map(|t| t.id).collect(),
            placement: TaskPlacement::default(),
            run: None,
            auto_start: false,
            sessions: Vec::new(),
            created_at_ms: age,
            started_at_ms: None,
            ended_at_ms: None,
            outcome: None,
        }
    }

    fn book(tasks: &[&Task]) -> TaskBook {
        let mut book = TaskBook::default();
        book.merge_listed(tasks.iter().map(|t| (*t).clone()).collect());
        book
    }

    const ALL: TaskFilters = TaskFilters {
        show_done: true,
        workspace: None,
    };

    #[test]
    fn the_tree_puts_each_task_under_the_dependency_it_waits_for_last() {
        let build = task("Build", 1, TaskState::Done, &[]);
        let lint = task("Lint", 2, TaskState::Ready, &[]);
        let test = task("Test", 3, TaskState::Ready, &[&build]);
        let ship = task("Ship", 4, TaskState::Todo, &[&test, &lint]);
        let rows = tree_rows(&book(&[&ship, &test, &lint, &build]), ALL, 0);
        let shape: Vec<(&str, usize)> = rows.iter().map(|r| (r.title.as_str(), r.depth)).collect();
        assert_eq!(
            shape,
            [("Build", 0), ("Test", 1), ("Ship", 2), ("Lint", 0)],
            "Ship waits for Test last (Test comes after Lint in dependency order) and names Lint as `also_after`"
        );
        assert_eq!(rows[2].also_after, ["Lint"]);
        assert!(rows[0].also_after.is_empty());
    }

    #[test]
    fn hidden_done_tasks_make_their_dependants_roots() {
        let build = task("Build", 1, TaskState::Done, &[]);
        let old = task("Old", 2, TaskState::Cancelled, &[]);
        let test = task("Test", 3, TaskState::Ready, &[&build]);
        let failed = task("Failed", 4, TaskState::Failed, &[&old]);
        let all = book(&[&build, &old, &test, &failed]);
        let rows = tree_rows(
            &all,
            TaskFilters {
                show_done: false,
                workspace: None,
            },
            0,
        );
        let shape: Vec<(&str, usize)> = rows.iter().map(|r| (r.title.as_str(), r.depth)).collect();
        assert_eq!(
            shape,
            [("Test", 0), ("Failed", 0)],
            "failed tasks always show"
        );
        assert_eq!(tree_rows(&all, ALL, 0).len(), 4);
    }

    #[test]
    fn the_workspace_filter_keeps_one_workspace() {
        let a = task("A", 1, TaskState::Todo, &[]);
        let b = task("B", 2, TaskState::Todo, &[]);
        let rows = tree_rows(
            &book(&[&a, &b]),
            TaskFilters {
                show_done: true,
                workspace: Some(b.workspace_id),
            },
            0,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "B");
    }

    #[test]
    fn row_views_carry_placement_run_elapsed_and_outcome() {
        let mut t = task("Fix login bug", 1, TaskState::Running, &[]);
        t.placement = TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: None,
            base: None,
        };
        t.run = Some(TaskRun::Terminal {
            argv: vec!["cargo".into(), "test".into()],
        });
        t.started_at_ms = Some(10_000);
        let row = &tree_rows(&book(&[&t]), ALL, 75_000)[0];
        assert_eq!(row.placement, "wt task/fix-login-bug");
        assert_eq!(row.branch.as_deref(), Some("task/fix-login-bug"));
        assert_eq!(row.run, "$ cargo test");
        assert_eq!(row.elapsed.as_deref(), Some("1m 05s"));
        assert!(row.reviewable);
        assert_eq!(row.glyph, "▶");

        t.state = TaskState::Failed;
        t.ended_at_ms = Some(40_000);
        t.outcome = Some("exited 3".into());
        t.run = Some(TaskRun::Chat {
            agent: "omp".into(),
        });
        let row = &tree_rows(&book(&[&t]), ALL, 75_000)[0];
        assert_eq!(
            row.elapsed.as_deref(),
            Some("30s"),
            "ended: how long it took"
        );
        assert_eq!(row.outcome.as_deref(), Some("exited 3"));
        assert_eq!(row.run, "omp chat");

        let plain = task("Plain", 2, TaskState::Todo, &[]);
        let row = &tree_rows(&book(&[&plain]), ALL, 0)[0];
        assert_eq!(
            (row.placement.as_str(), row.run.as_str()),
            ("main", "shell")
        );
        assert_eq!(row.elapsed, None);
        assert!(
            !row.reviewable,
            "a main task that never ran has nothing to review"
        );
    }

    #[test]
    fn actions_follow_the_state_machine() {
        use TaskAction::*;
        assert_eq!(actions_for(TaskState::Ready), [Start, Done, Cancel, Delete]);
        assert_eq!(actions_for(TaskState::Todo), [Done, Cancel, Delete]);
        assert_eq!(actions_for(TaskState::Running), [Done, Cancel]);
        assert_eq!(actions_for(TaskState::Blocked), [Done, Cancel]);
        assert_eq!(actions_for(TaskState::Failed), [Done, Reset, Delete]);
        assert_eq!(actions_for(TaskState::Done), [Reset, Delete]);
        assert_eq!(actions_for(TaskState::Cancelled), [Reset, Delete]);
    }

    #[test]
    fn an_event_during_a_list_is_not_undone_by_the_older_list() {
        let a = task("A", 1, TaskState::Todo, &[]);
        let mut book = TaskBook::default();
        book.start_loading();
        let mut newer = a.clone();
        newer.state = TaskState::Done;
        book.updated(newer);
        let gone = task("Gone", 2, TaskState::Todo, &[]);
        book.removed(gone.id);
        // The list was answered before the events happened.
        book.merge_listed(vec![a.clone(), gone.clone()]);
        assert_eq!(book.get(a.id).map(|t| t.state), Some(TaskState::Done));
        assert!(book.get(gone.id).is_none(), "a removed task stays removed");
        assert!(book.loaded());

        // A later list drops what the daemon no longer has.
        book.start_loading();
        book.merge_listed(vec![]);
        assert!(book.is_empty());
    }

    fn form() -> NewTask {
        NewTask {
            title: "  Build it  ".into(),
            prompt: String::new(),
            after: vec![],
            worktree: false,
            branch: String::new(),
            run: RunChoice::Terminal,
            command: String::new(),
            auto_start: false,
        }
    }

    #[test]
    fn a_form_becomes_a_create_request() {
        let ws = WorkspaceId::new();
        let dep = TaskId::new();
        let plain = build_create(ws, &form()).unwrap();
        assert_eq!(plain.title, "Build it");
        assert_eq!(
            (plain.run, plain.placement, plain.prompt),
            (None, None, None)
        );

        let full = build_create(
            ws,
            &NewTask {
                prompt: " do it ".into(),
                after: vec![dep],
                worktree: true,
                branch: " feature/x ".into(),
                command: "cargo test -p \"my crate\"".into(),
                auto_start: true,
                ..form()
            },
        )
        .unwrap();
        assert_eq!(full.depends_on, [dep]);
        assert!(full.auto_start);
        assert_eq!(full.prompt.as_deref(), Some("do it"));
        assert_eq!(
            full.run,
            Some(TaskRun::Terminal {
                argv: vec![
                    "cargo".into(),
                    "test".into(),
                    "-p".into(),
                    "my crate".into()
                ]
            })
        );
        assert_eq!(
            full.placement,
            Some(TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some("feature/x".into()),
                base: None
            })
        );

        let default_branch = build_create(
            ws,
            &NewTask {
                worktree: true,
                ..form()
            },
        )
        .unwrap();
        assert_eq!(default_branch.placement.unwrap().branch, None);
    }

    #[test]
    fn a_form_with_a_problem_says_which() {
        let ws = WorkspaceId::new();
        let err = |f: NewTask| build_create(ws, &f).unwrap_err();
        assert!(
            err(NewTask {
                title: "  ".into(),
                ..form()
            })
            .contains("title")
        );
        assert!(
            err(NewTask {
                title: "x".repeat(201),
                ..form()
            })
            .contains("200")
        );
        assert!(
            err(NewTask {
                run: RunChoice::Omp,
                ..form()
            })
            .contains("omp chat needs a prompt")
        );
        assert!(
            err(NewTask {
                command: "echo 'x".into(),
                ..form()
            })
            .contains("quote")
        );
        let chat = build_create(
            ws,
            &NewTask {
                run: RunChoice::Codex,
                prompt: "hi".into(),
                command: "ignored".into(),
                ..form()
            },
        )
        .unwrap();
        assert_eq!(
            chat.run,
            Some(TaskRun::Chat {
                agent: "codex".into()
            })
        );
    }

    #[test]
    fn commands_split_like_a_shell_without_expanding() {
        let words = |l: &str| split_command(l).unwrap();
        assert_eq!(words("  a   b "), ["a", "b"]);
        assert_eq!(words("a 'b c' \"d e\""), ["a", "b c", "d e"]);
        assert_eq!(words("a\\ b"), ["a b"]);
        assert_eq!(words("echo $HOME *"), ["echo", "$HOME", "*"]);
        assert_eq!(words("''"), [""], "an empty quoted word is a word");
        assert!(words("").is_empty());
        assert!(split_command("a \\").is_err());
    }
}
