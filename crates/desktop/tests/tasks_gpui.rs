#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Tasks panel and the Changes view (M7-C) against an in-process daemon: tasks are created
//! and changed through the real `task.*` requests, git repositories are made by the tests in
//! temporary directories, and the panel, the form, the actions, the Changes view and the merge
//! flow go through the real requests too.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::{
    AfterPolicy, DeliveryActor, DeliveryState, ReviewDecision, Task, TaskId, TaskPlacement,
    TaskPlacementKind, TaskRun, TaskState, WorkspaceId,
};
use hutan_desktop::sessions::attention::Tone;
use hutan_desktop::sessions::tasks::{RunChoice, TaskAction, TaskRowView};
use hutan_desktop::settings::TasksSettings;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::{TaskFormField, TasksPanel};
use hutan_desktop::workbench::{ChangesView, CleanupDefault, DeliveryForm, Region, Workbench};
use hutan_protocol::messages::Request;
use hutan_protocol::task::{
    TaskCreate, TaskList, TaskListRequest, TaskRef, TaskResult, TaskReview, TaskReviewed,
    TaskStarted, TaskUpdate,
};
use hutan_protocol::worktree::{
    MergeStrategy, WorktreeCreate, WorktreeCreated, WorktreeList, WorktreeListRequest,
};
use hutan_settings::{SettingsPaths, SettingsStore};

// Fixtures ------------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// `<tmp>/repo` on `main` with `a.txt` and an identity the daemon's git calls find.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap().join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&root, &["config", key, value]);
    }
    fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    commit_all(&root, "first");
    (dir, root)
}

struct Env {
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    root: PathBuf,
    workspace: WorkspaceId,
}

impl Env {
    fn new() -> Self {
        let daemon = TestDaemon::start();
        let (keep, root) = repo();
        let workspace = daemon.create_workspace("proj", &[&root]).id;
        Self {
            daemon,
            _keep: keep,
            root,
            workspace,
        }
    }

    fn create(&self, title: &str, deps: &[&Task], build: impl FnOnce(&mut TaskCreate)) -> Task {
        let mut request = TaskCreate {
            workspace_id: self.workspace,
            title: title.to_owned(),
            prompt: None,
            depends_on: deps.iter().map(|t| t.id).collect(),
            placement: None,
            run: Some(TaskRun::Terminal {
                argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
            }),
            auto_start: false,
            after_policy: AfterPolicy::Done,
        };
        build(&mut request);
        let created: TaskResult = self.daemon.call(Request::TaskCreate(request));
        created.task
    }

    fn plain(&self, title: &str, deps: &[&Task]) -> Task {
        self.create(title, deps, |_| {})
    }

    fn set_state(&self, task: &Task, state: TaskState) -> Task {
        let updated: TaskResult = self.daemon.call(Request::TaskUpdate(TaskUpdate {
            id: task.id,
            title: None,
            prompt: None,
            depends_on: None,
            placement: None,
            run: None,
            auto_start: None,
            after_policy: None,
            state: Some(state),
        }));
        updated.task
    }

    fn tasks(&self) -> Vec<Task> {
        self.daemon
            .call::<TaskList>(Request::TaskList(TaskListRequest::default()))
            .tasks
    }

    fn task(&self, id: TaskId) -> Option<Task> {
        self.tasks().into_iter().find(|t| t.id == id)
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    env: &Env,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(env.daemon.home())),
        env.daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let workspace = env.workspace;
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    (view, cx)
}

fn show_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<TasksPanel> {
    cx.dispatch_action(actions::OpenPanel("tasks".into()));
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<TasksPanel>("tasks"))
        .expect("the tasks panel is drawn in the right dock")
}

fn rows(panel: &Entity<TasksPanel>, cx: &VisualTestContext) -> Vec<TaskRowView> {
    panel.read_with(cx, |p, cx| p.rows(0, cx))
}

fn shape(panel: &Entity<TasksPanel>, cx: &VisualTestContext) -> Vec<(String, usize, &'static str)> {
    rows(panel, cx)
        .into_iter()
        .map(|r| (r.title, r.depth, r.glyph))
        .collect()
}

fn wait_rows(panel: &Entity<TasksPanel>, cx: &mut VisualTestContext, what: &str, count: usize) {
    let panel = panel.clone();
    wait_until(cx, what, move |cx| rows(&panel, cx).len() == count);
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        region(view, cx, what).is_some()
    });
    let bounds = region(view, cx, what).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn settings(view: &Entity<Workbench>, cx: &VisualTestContext) -> SettingsStore {
    view.read_with(cx, |wb, _| wb.settings_store().clone())
}

// The panel -----------------------------------------------------------------------------------

#[gpui::test]
fn the_panel_shows_the_dependency_tree_and_follows_task_updates(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    let test = env.plain("Test", &[&build]);
    let ship = env.plain("Ship", &[&test]);
    let lint = env.plain("Lint", &[]);
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "four rows", 4);

    // Each task under the one it waits for; ready (◎) and waiting (○) tell apart.
    assert_eq!(
        shape(&panel, cx),
        [
            ("Build".to_owned(), 0, "◎"),
            ("Test".to_owned(), 1, "○"),
            ("Ship".to_owned(), 2, "○"),
            ("Lint".to_owned(), 0, "◎"),
        ]
    );
    let first = rows(&panel, cx).remove(0);
    assert_eq!(
        (first.placement.as_str(), first.run.as_str()),
        ("main", "$ sh -c sleep 60")
    );
    for index in 0..4 {
        wait_until(cx, "the rows to be drawn", |cx| {
            region(&view, cx, Region::TaskRow(index)).is_some()
        });
    }

    // task.updated: finishing Build makes Test ready; the tree follows without a refresh.
    env.set_state(&build, TaskState::Done);
    wait_until(cx, "Build done and Test ready", |cx| {
        shape(&panel, cx)[..2] == [("Build".to_owned(), 0, "✓"), ("Test".to_owned(), 1, "◎")]
    });

    // A new task appears; a deleted one goes (task.removed).
    let extra = env.plain("Extra", &[&lint]);
    wait_rows(&panel, cx, "five rows", 5);
    assert_eq!(rows(&panel, cx).last().unwrap().depth, 1);
    let _: hutan_protocol::task::TaskDeleted =
        env.daemon
            .call(Request::TaskDelete(hutan_protocol::task::TaskDelete {
                id: extra.id,
                cascade: false,
            }));
    wait_rows(&panel, cx, "four rows again", 4);
    let _ = ship;
}

#[gpui::test]
fn filters_hide_done_tasks_and_other_workspaces(cx: &mut TestAppContext) {
    let env = Env::new();
    let other = env
        .daemon
        .create_workspace("other", &[Path::new("/tmp")])
        .id;
    let done = env.plain("Done one", &[]);
    env.set_state(&done, TaskState::Done);
    env.plain("Open one", &[]);
    let _: TaskResult = env.daemon.call(Request::TaskCreate(TaskCreate {
        workspace_id: other,
        title: "Elsewhere".into(),
        prompt: None,
        depends_on: vec![],
        placement: None,
        run: None,
        auto_start: false,
        after_policy: AfterPolicy::Done,
    }));
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "this workspace's two tasks", 2);

    // tasks.show_done = false: the done one goes; failed ones would stay.
    settings(&view, cx)
        .write_user("tasks.show_done", toml::Value::Boolean(false))
        .unwrap();
    wait_rows(&panel, cx, "one open task", 1);
    assert_eq!(rows(&panel, cx)[0].title, "Open one");

    // tasks.scope = "all": the other workspace's task too.
    settings(&view, cx)
        .write_user("tasks.scope", toml::Value::String("all".into()))
        .unwrap();
    wait_rows(&panel, cx, "two open tasks of every workspace", 2);
    settings(&view, cx)
        .write_user("tasks.show_done", toml::Value::Boolean(true))
        .unwrap();
    wait_rows(&panel, cx, "all three", 3);

    // The chips write the same settings.
    click(&view, cx, Region::Tasks("chip-done"));
    assert!(!settings(&view, cx).get::<TasksSettings>().show_done);
    click(&view, cx, Region::Tasks("chip-scope"));
    assert_eq!(
        settings(&view, cx).get::<TasksSettings>().scope.key(),
        "workspace"
    );
    wait_rows(&panel, cx, "one open task of this workspace", 1);
}

#[gpui::test]
fn an_empty_panel_shows_the_mascot_and_offers_a_new_task(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_until(cx, "the empty state with its action", |cx| {
        region(&view, cx, Region::Tasks("empty-new")).is_some()
    });
    assert!(rows(&panel, cx).is_empty());
    click(&view, cx, Region::Tasks("empty-new"));
    assert!(panel.read_with(cx, |p, _| p.form_open()));
}

#[gpui::test]
fn the_palette_has_the_task_and_review_commands(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    cx.dispatch_action(actions::TogglePalette);
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    for title in [
        "Tasks: New Task…",
        "Tasks: Show Panel",
        "Worktree: Review Changes…",
    ] {
        assert!(titles.iter().any(|t| t.as_ref() == title), "{title}");
    }
}

#[gpui::test]
fn the_palette_command_opens_the_form_in_the_panel(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    cx.dispatch_action(actions::NewTask);
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<TasksPanel>("tasks"))
        .expect("the panel is revealed");
    assert!(panel.read_with(cx, |p, _| p.form_open()));
    wait_until(cx, "the title field", |cx| {
        region(&view, cx, Region::Tasks("form-title")).is_some()
    });
}

#[gpui::test]
fn row_actions_send_the_right_requests(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    let waits = env.plain("Waits", &[&build]);
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "two rows", 2);

    // A ready task offers Start (and the others); a waiting one does not.
    click(&view, cx, Region::TaskRow(0));
    let actions_of = |cx: &VisualTestContext, title: &str| {
        rows(&panel, cx)
            .into_iter()
            .find(|r| r.title == title)
            .unwrap()
            .actions
    };
    assert_eq!(
        actions_of(cx, "Build"),
        [
            TaskAction::Start,
            TaskAction::Done,
            TaskAction::Cancel,
            TaskAction::Delete
        ]
    );
    assert_eq!(
        actions_of(cx, "Waits"),
        [TaskAction::Done, TaskAction::Cancel, TaskAction::Delete]
    );

    // Start: the daemon starts it and a session works on it.
    click(&view, cx, Region::TaskAction(0, "start"));
    wait_until(cx, "the task to run", |_| {
        env.task(build.id)
            .is_some_and(|t| t.state == TaskState::Running && t.sessions.len() == 1)
    });
    wait_until(cx, "the row to say running", |cx| {
        rows(&panel, cx)[0].state == TaskState::Running
    });
    assert_eq!(
        actions_of(cx, "Build"),
        [TaskAction::Done, TaskAction::Cancel]
    );

    // Cancel stops it.
    click(&view, cx, Region::TaskRow(0));
    click(&view, cx, Region::TaskRow(0));
    wait_until(cx, "the cancel button", |cx| {
        region(&view, cx, Region::TaskAction(0, "cancel")).is_some()
    });
    click(&view, cx, Region::TaskAction(0, "cancel"));
    wait_until(cx, "the task to be cancelled", |_| {
        env.task(build.id)
            .is_some_and(|t| t.state == TaskState::Cancelled)
    });

    // Reset puts it back to todo (ready again: nothing to wait for).
    wait_until(cx, "the reset button", |cx| {
        region(&view, cx, Region::TaskAction(0, "reset")).is_some()
    });
    click(&view, cx, Region::TaskAction(0, "reset"));
    wait_until(cx, "the task to be ready again", |cx| {
        rows(&panel, cx)[0].state == TaskState::Ready
    });

    // Done marks it done; the waiting task becomes ready.
    click(&view, cx, Region::TaskAction(0, "done"));
    wait_until(cx, "Build done and Waits ready", |cx| {
        let rows = rows(&panel, cx);
        rows[0].state == TaskState::Done && rows[1].state == TaskState::Ready
    });
    assert_eq!(
        env.task(waits.id).unwrap().outcome.as_deref(),
        None,
        "only Build was marked"
    );
    assert_eq!(
        env.task(build.id).unwrap().outcome.as_deref(),
        Some("marked done")
    );
}

#[gpui::test]
fn deleting_a_task_others_wait_for_asks_about_its_dependants(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    env.plain("Test", &[&build]);
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "two rows", 2);

    click(&view, cx, Region::TaskRow(0));
    click(&view, cx, Region::TaskAction(0, "delete"));
    wait_until(cx, "the question", |cx| {
        panel.read_with(cx, |p, _| p.cascade_ask().is_some())
    });
    let message = panel.read_with(cx, |p, _| p.cascade_ask().unwrap().message.clone());
    assert!(message.contains("Test"), "{message}");
    assert_eq!(env.tasks().len(), 2, "nothing was deleted yet");

    // Keep: nothing changes. Ask again and delete them too.
    click(&view, cx, Region::Tasks("cascade-no"));
    assert_eq!(env.tasks().len(), 2);
    assert!(panel.read_with(cx, |p, _| p.cascade_ask().is_none()));
    click(&view, cx, Region::TaskAction(0, "delete"));
    wait_until(cx, "the question again", |cx| {
        panel.read_with(cx, |p, _| p.cascade_ask().is_some())
    });
    click(&view, cx, Region::Tasks("cascade-yes"));
    wait_until(cx, "both tasks to go", |_| env.tasks().is_empty());
    wait_rows(&panel, cx, "no rows", 0);
}

#[gpui::test]
fn clicking_a_row_focuses_the_session_of_its_task(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    let started: hutan_protocol::task::TaskStarted =
        env.daemon
            .call(Request::TaskStart(hutan_protocol::task::TaskRef {
                id: build.id,
            }));
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "one row", 1);
    wait_until(cx, "the row to carry its session", |cx| {
        rows(&panel, cx)[0].session == Some(started.session.id)
    });
    click(&view, cx, Region::TaskRow(0));
    let item = ItemRef::Terminal {
        session: started.session.id.to_string(),
    };
    wait_until(cx, "the session's tab", |cx| {
        view.read_with(cx, |wb, _| wb.state().find_item(&item).is_some())
    });
}

// The form ------------------------------------------------------------------------------------

#[gpui::test]
fn the_form_creates_a_task_with_dependencies_and_a_worktree_placement(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    let lint = env.plain("Lint", &[]);
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "two rows", 2);

    click(&view, cx, Region::Tasks("new"));
    assert!(panel.read_with(cx, |p, _| p.form_open()));

    // An empty form says what is missing.
    click(&view, cx, Region::Tasks("form-create"));
    assert_eq!(
        panel.read_with(cx, |p, _| p.form_error().map(str::to_owned)),
        Some("A task needs a title.".to_owned())
    );

    // Fill it in with the controls and the keyboard.
    click(&view, cx, Region::Tasks("form-title"));
    cx.simulate_input("Ship it");
    draw(cx);
    panel.update(cx, |p, cx| {
        p.set_field(TaskFormField::Prompt, "Cut the release", cx);
        p.set_run(RunChoice::Terminal, cx);
    });
    click(&view, cx, Region::Tasks("form-command"));
    cx.simulate_input("cargo build --release");
    click(&view, cx, Region::Tasks("place-worktree"));
    click(&view, cx, Region::Tasks("form-branch"));
    cx.simulate_input("release/1.0");
    click(&view, cx, Region::TaskDep(0));
    click(&view, cx, Region::TaskDep(1));
    click(&view, cx, Region::TaskDep(0)); // un-tick the first again
    click(&view, cx, Region::Tasks("form-auto"));
    assert_eq!(panel.read_with(cx, |p, _| p.form_dependencies()), [lint.id]);
    click(&view, cx, Region::Tasks("form-create"));

    wait_until(cx, "the task to exist", |_| {
        env.tasks().iter().any(|t| t.title == "Ship it")
    });
    let created = env
        .tasks()
        .into_iter()
        .find(|t| t.title == "Ship it")
        .unwrap();
    assert_eq!(created.depends_on, [lint.id]);
    assert_eq!(created.prompt.as_deref(), Some("Cut the release"));
    assert!(created.auto_start);
    assert_eq!(
        created.placement,
        TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("release/1.0".into()),
            base: None
        }
    );
    assert_eq!(
        created.run,
        Some(TaskRun::Terminal {
            argv: vec!["cargo".into(), "build".into(), "--release".into()]
        })
    );
    // The form closed and the new row is in the tree under Lint, with its badge.
    wait_until(cx, "the form to close and the row to appear", |cx| {
        panel.read_with(cx, |p, _| !p.form_open()) && rows(&panel, cx).len() == 3
    });
    let ship = rows(&panel, cx)
        .into_iter()
        .find(|r| r.title == "Ship it")
        .unwrap();
    assert_eq!(ship.depth, 1);
    assert_eq!(ship.placement, "wt release/1.0");
    let _ = build;
}

#[gpui::test]
fn the_form_offers_a_chat_run_that_needs_a_prompt(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    click(&view, cx, Region::Tasks("empty-new"));
    wait_until(cx, "the run choices", |cx| {
        region(&view, cx, Region::Tasks("run-codex")).is_some()
    });
    // omp chats are offered when the daemon runs the ACP harnesses (this one does).
    assert!(region(&view, cx, Region::Tasks("run-omp")).is_some());
    assert!(region(&view, cx, Region::Tasks("run-claude")).is_some());

    panel.update(cx, |p, cx| p.set_field(TaskFormField::Title, "Review", cx));
    click(&view, cx, Region::Tasks("run-codex"));
    click(&view, cx, Region::Tasks("form-create"));
    let error = panel.read_with(cx, |p, _| p.form_error().map(str::to_owned));
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("needs a prompt")),
        "{error:?}"
    );
    assert!(env.tasks().is_empty(), "nothing was sent");

    // The daemon's own refusal comes back in the form: it takes no branch with a space in it.
    panel.update(cx, |p, cx| {
        p.set_run(RunChoice::Terminal, cx);
        p.set_worktree(true, cx);
        p.set_field(TaskFormField::Branch, "bad branch name", cx);
    });
    click(&view, cx, Region::Tasks("form-create"));
    wait_until(cx, "the daemon's refusal in the form", |cx| {
        panel.read_with(cx, |p, _| {
            p.form_error()
                .is_some_and(|e| e.contains("is not a valid branch"))
        })
    });
    assert!(panel.read_with(cx, |p, _| p.form_open()));
    assert!(env.tasks().is_empty(), "nothing was created");

    // Fixing the branch lets it through.
    panel.update(cx, |p, cx| {
        p.set_field(TaskFormField::Branch, "review/it", cx);
    });
    click(&view, cx, Region::Tasks("form-create"));
    wait_until(cx, "the task to be created", |_| !env.tasks().is_empty());
    wait_until(cx, "the form to close", |cx| {
        panel.read_with(cx, |p, _| !p.form_open())
    });
}

// The Changes view ------------------------------------------------------------------------------

/// A worktree `feature/x` of `env.root` with a commit, a rename-free edit and an untracked file.
fn feature_worktree(env: &Env) -> PathBuf {
    let created: WorktreeCreated = env.daemon.call(Request::WorktreeCreate(WorktreeCreate {
        workspace_id: env.workspace,
        repo: None,
        branch: "feature/x".into(),
        base: None,
        path: None,
    }));
    let wt = created.worktree.path;
    fs::write(wt.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    fs::write(wt.join("b.txt"), "b\n").unwrap();
    commit_all(&wt, "feature work");
    fs::write(wt.join("untracked.txt"), "u1\nu2\n").unwrap();
    wt
}

fn changes(view: &Entity<Workbench>, cx: &VisualTestContext, wt: &Path) -> Entity<ChangesView> {
    view.read_with(cx, |wb, _| wb.changes(&wt.display().to_string()))
        .expect("the Changes view was opened")
}

#[gpui::test]
fn the_changes_view_lists_files_and_renders_a_diff_with_folding_hunks(cx: &mut TestAppContext) {
    let env = Env::new();
    // Thirty lines on main; the branch changes lines 2 and 28: two hunks.
    fs::write(
        env.root.join("a.txt"),
        (1..=30).map(|n| format!("line {n}\n")).collect::<String>(),
    )
    .unwrap();
    commit_all(&env.root, "thirty lines");
    let wt = feature_worktree(&env);
    let edited: String = (1..=30)
        .map(|n| match n {
            2 => "line two changed\n".to_owned(),
            28 => "line 28 changed\n".to_owned(),
            n => format!("line {n}\n"),
        })
        .collect();
    fs::write(wt.join("a.txt"), edited).unwrap();
    fs::remove_file(wt.join("untracked.txt")).unwrap();
    commit_all(&wt, "two hunks");
    // Uncommitted, and never staged.
    fs::write(wt.join("scratch.txt"), "s1\ns2\n").unwrap();

    let (view, cx) = open(cx, &env);
    view.update_in(cx, |wb, window, cx| wb.open_changes(wt.clone(), window, cx));
    draw(cx);
    // It is a tab of the active pane, titled with the branch once the worktree is listed.
    let item = ItemRef::changes(wt.display().to_string());
    assert!(view.read_with(cx, |wb, _| wb.state().find_item(&item).is_some()));
    let changes = changes(&view, cx, &wt);
    wait_until(cx, "the file list", |cx| {
        changes.read_with(cx, |c, _| c.files().len() == 3 && c.base() == Some("main"))
    });
    let listed: Vec<String> = changes.read_with(cx, |c, _| {
        c.files()
            .iter()
            .map(|f| {
                format!(
                    "{} {:?} +{} -{}",
                    f.path, f.status, f.additions, f.deletions
                )
            })
            .collect()
    });
    assert_eq!(
        listed,
        [
            "a.txt Modified +2 -2",
            "b.txt Added +1 -0",
            "scratch.txt Added +2 -0"
        ]
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.item_title(&item, cx).to_string()),
        "Changes · feature/x"
    );
    for index in 0..3 {
        wait_until(cx, "the file rows", |cx| {
            region(&view, cx, Region::ChangesFile(index)).is_some()
        });
    }

    // The first file's diff arrives: two hunks, with the changed lines in them.
    wait_until(cx, "the first patch", |cx| {
        changes.read_with(cx, |c, _| c.selected_diff().is_some())
    });
    let (hunks, adds, removes) = changes.read_with(cx, |c, _| {
        let diff = c.selected_diff().unwrap();
        (
            hutan_desktop::workbench::hunk_ranges(diff).len(),
            diff.added,
            diff.removed,
        )
    });
    assert_eq!((hunks, adds, removes), (2, 2, 2));
    wait_until(cx, "both hunk headers", |cx| {
        region(&view, cx, Region::ChangesHunk(0)).is_some()
            && region(&view, cx, Region::ChangesHunk(1)).is_some()
    });

    // Hunks fold and unfold.
    click(&view, cx, Region::ChangesHunk(0));
    assert_eq!(changes.read_with(cx, |c, _| c.collapsed_hunks()), [0]);
    click(&view, cx, Region::ChangesHunk(1));
    assert_eq!(changes.read_with(cx, |c, _| c.collapsed_hunks()), [0, 1]);
    click(&view, cx, Region::ChangesHunk(0));
    assert_eq!(changes.read_with(cx, |c, _| c.collapsed_hunks()), [1]);

    // Another file: an untracked one is a new file in full.
    click(&view, cx, Region::ChangesFile(2));
    wait_until(cx, "the untracked file's patch", |cx| {
        changes.read_with(cx, |c, _| {
            c.selected() == Some("scratch.txt") && c.selected_diff().is_some()
        })
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.selected_diff().map(|d| (d.added, d.removed))),
        Some((2, 0))
    );

    // Leaving out the uncommitted changes drops it from the list.
    click(&view, cx, Region::Changes("toggle"));
    assert!(
        !changes.read_with(cx, |c, _| c.include_uncommitted()),
        "the toggle was clicked"
    );
    wait_until(cx, "the committed-only list", |cx| {
        changes.read_with(cx, |c, _| !c.include_uncommitted() && c.files().len() == 2)
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.selected().map(str::to_owned)),
        Some("a.txt".to_owned()),
        "the selection moved to a file that is still listed"
    );
}

#[gpui::test]
fn a_view_of_a_checkout_without_changes_says_so_and_cannot_merge(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    let root = env.root.clone();
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(root.clone(), window, cx)
    });
    draw(cx);
    let changes = changes(&view, cx, &root);
    wait_until(cx, "the listing and the worktree", |cx| {
        changes.read_with(cx, |c, _| c.base().is_some() && c.info().is_some())
    });
    assert!(changes.read_with(cx, |c, _| c.files().is_empty() && !c.can_merge()));
    // The merge button of the main checkout is inert.
    click(&view, cx, Region::Changes("merge"));
    assert!(!changes.read_with(cx, |c, _| c.merge_open()));
}

/// Opens the Changes view of `wt` and waits for its list and worktree.
fn open_changes(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    wt: &Path,
) -> Entity<ChangesView> {
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(wt.to_owned(), window, cx)
    });
    draw(cx);
    let changes = changes(view, cx, wt);
    wait_until(cx, "the listing", |cx| {
        changes.read_with(cx, |c, _| !c.files().is_empty() && c.info().is_some())
    });
    changes
}

#[gpui::test]
fn merging_from_the_view_commits_in_the_main_checkout_and_offers_the_removal(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let wt = feature_worktree(&env);
    fs::remove_file(wt.join("untracked.txt")).unwrap();
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    // Main moves on, so this is a real merge commit.
    fs::write(env.root.join("main.txt"), "m\n").unwrap();
    commit_all(&env.root, "main work");

    click(&view, cx, Region::Changes("merge"));
    assert!(changes.read_with(cx, |c, _| c.merge_open()));
    assert_eq!(
        changes.read_with(cx, |c, _| c.merge_strategy()),
        Some(MergeStrategy::Merge)
    );
    changes.update(cx, |c, cx| c.set_merge_message("Merge the feature", cx));
    click(&view, cx, Region::Changes("merge-confirm"));
    wait_until(cx, "the merge result", |cx| {
        changes.read_with(cx, |c, _| c.merged().is_some())
    });
    let merged = changes.read_with(cx, |c, _| c.merged().cloned().unwrap());
    assert_eq!(
        (merged.target.as_str(), merged.branch.as_str()),
        ("main", "feature/x")
    );
    assert_eq!(
        merged.commit,
        git_out(&env.root, &["rev-parse", "HEAD"]).trim()
    );
    assert_eq!(
        git_out(&env.root, &["log", "-1", "--format=%s"]).trim(),
        "Merge the feature"
    );
    // The toast, and the form is gone.
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(|n| n.to_string()))
            .is_some_and(|n| n.starts_with("Merged feature/x into main (")),
    );
    assert!(!changes.read_with(cx, |c, _| c.merge_open()));
    wait_until(cx, "the remove offer", |cx| {
        region(&view, cx, Region::Changes("remove")).is_some()
    });

    // "Remove worktree and branch": gone, with its (merged) branch, and the tab closes.
    click(&view, cx, Region::Changes("remove"));
    wait_until(
        cx,
        "the worktree and its merged branch to be removed",
        |_| {
            !wt.exists()
                && !git_out(&env.root, &["branch", "--format=%(refname:short)"])
                    .lines()
                    .any(|b| b == "feature/x")
        },
    );
    wait_until(cx, "the tab to close", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .find_item(&ItemRef::changes(wt.display().to_string()))
                .is_none()
        })
    });
}

#[gpui::test]
fn keeping_the_worktree_after_a_squash_merge_dismisses_the_offer(cx: &mut TestAppContext) {
    let env = Env::new();
    let wt = feature_worktree(&env);
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    click(&view, cx, Region::Changes("merge"));
    click(&view, cx, Region::Changes("strategy-squash"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.merge_strategy()),
        Some(MergeStrategy::Squash)
    );
    changes.update(cx, |c, cx| c.set_merge_message("Add b", cx));
    click(&view, cx, Region::Changes("merge-confirm"));
    wait_until(cx, "the merge result", |cx| {
        changes.read_with(cx, |c, _| c.merged().is_some())
    });
    let merged = changes.read_with(cx, |c, _| c.merged().cloned().unwrap());
    assert_eq!(merged.strategy, MergeStrategy::Squash);
    assert!(
        merged.source_dirty,
        "the untracked file is not part of the merge"
    );
    assert_eq!(
        git_out(&env.root, &["log", "-1", "--format=%s"]).trim(),
        "Add b"
    );
    click(&view, cx, Region::Changes("keep"));
    assert!(changes.read_with(cx, |c, _| c.merged().is_none()));
    assert!(wt.exists());
}

#[gpui::test]
fn a_conflicting_merge_stays_open_with_the_files_and_changes_nothing(cx: &mut TestAppContext) {
    let env = Env::new();
    let wt = feature_worktree(&env);
    fs::write(env.root.join("a.txt"), "main side\n").unwrap();
    commit_all(&env.root, "main edit");
    let head = git_out(&env.root, &["rev-parse", "HEAD"]);
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);

    click(&view, cx, Region::Changes("merge"));
    click(&view, cx, Region::Changes("merge-confirm"));
    wait_until(cx, "the refusal", |cx| {
        changes.read_with(cx, |c, _| c.merge_error().is_some())
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.merge_conflicts().to_vec()),
        ["a.txt"]
    );
    let error = changes.read_with(cx, |c, _| c.merge_error().unwrap().to_owned());
    assert!(error.contains("conflicts in 1 file(s)"), "{error}");
    assert!(changes.read_with(cx, |c, _| c.merge_open() && c.merged().is_none()));
    assert_eq!(git_out(&env.root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_out(&env.root, &["status", "--porcelain"]), "");

    // A dirty main checkout is refused too, with its reason.
    changes.update(cx, |c, cx| c.close_merge(cx));
    fs::write(env.root.join("stray.txt"), "x\n").unwrap();
    fs::write(env.root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    commit_all(&env.root, "undo the edit");
    fs::write(env.root.join("stray.txt"), "dirty\n").unwrap();
    click(&view, cx, Region::Changes("merge"));
    click(&view, cx, Region::Changes("merge-confirm"));
    wait_until(cx, "the second refusal", |cx| {
        changes.read_with(cx, |c, _| c.merge_error().is_some())
    });
    let error = changes.read_with(cx, |c, _| c.merge_error().unwrap().to_owned());
    assert!(error.contains("uncommitted changes"), "{error}");
}

// Entry points ----------------------------------------------------------------------------------

#[gpui::test]
fn a_task_row_session_badge_and_the_palette_open_the_changes_view(cx: &mut TestAppContext) {
    let env = Env::new();
    let task = env.create("Fix login", &[], |request| {
        request.placement = Some(TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("task/fix-login".into()),
            base: None,
        });
    });
    let started: hutan_protocol::task::TaskStarted =
        env.daemon
            .call(Request::TaskStart(hutan_protocol::task::TaskRef {
                id: task.id,
            }));
    let wt = {
        let list: WorktreeList = env.daemon.call(Request::WorktreeList(
            hutan_protocol::worktree::WorktreeListRequest {
                workspace_id: Some(env.workspace),
            },
        ));
        list.worktrees
            .into_iter()
            .find(|w| w.branch.as_deref() == Some("task/fix-login"))
            .expect("the task's worktree exists")
            .path
    };
    fs::write(wt.join("new.txt"), "n\n").unwrap();
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "the task", 1);
    let item = ItemRef::changes(wt.display().to_string());
    let opened = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.state().find_item(&item).is_some())
    };

    // 1. The task row's "Changes".
    wait_until(cx, "the session on the row", |cx| {
        rows(&panel, cx)[0].session == Some(started.session.id)
    });
    click(&view, cx, Region::TaskRow(0));
    click(&view, cx, Region::TaskAction(0, "changes"));
    wait_until(cx, "the Changes tab of the task", |cx| opened(&view, cx));
    // Close it again to try the next way.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            if let Some((pane, index)) = s.find_item(&item) {
                s.close_item(pane, index).unwrap();
            }
        });
    });
    assert!(!opened(&view, cx));

    // 2. ⌘K "Worktree: Review Changes…": a picker of the checkouts.
    cx.dispatch_action(actions::ReviewChanges);
    wait_until(cx, "the picker rows", |cx| {
        view.read_with(cx, |wb, _| wb.picker_titles().len() == 2)
    });
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.picker_titles().iter().map(ToString::to_string).collect()
    });
    assert_eq!(titles, ["main", "task/fix-login"]);
    view.update_in(cx, |wb, window, cx| wb.picker_confirm(Some(1), window, cx));
    wait_until(cx, "the Changes tab from the picker", |cx| {
        opened(&view, cx)
    });
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            if let Some((pane, index)) = s.find_item(&item) {
                s.close_item(pane, index).unwrap();
            }
        });
    });

    // 3. The placement badge of the session's row.
    cx.dispatch_action(actions::OpenPanel("sessions".into()));
    let badge =
        Region::PlacementBadge(hutan_desktop::workbench::BadgeSite::Row, started.session.id);
    wait_until(cx, "the badge", |cx| region(&view, cx, badge).is_some());
    click(&view, cx, badge);
    let entries: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the badge opens a menu")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(entries, ["Review Changes…", "Copy Checkout Path"]);
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));
    wait_until(cx, "the Changes tab from the badge", |cx| opened(&view, cx));
}

#[gpui::test]
fn the_panel_requires_a_daemon_that_knows_tasks_and_a_connection(cx: &mut TestAppContext) {
    // Offline: the panel says so and the new-task button is inert.
    let (connection, settings, state) = common::offline_connection();
    let (view, cx) = open_with(cx, settings, connection, Some(state));
    cx.dispatch_action(actions::OpenPanel("tasks".into()));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<TasksPanel>("tasks"))
        .unwrap();
    assert!(panel.read_with(cx, |p, cx| p.rows(0, cx).is_empty()));
    click_no_effect(&view, cx);
    assert!(!panel.read_with(cx, |p, _| p.form_open()));
}

fn click_no_effect(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the new button", |cx| {
        region(view, cx, Region::Tasks("new")).is_some()
    });
    let bounds = region(view, cx, Region::Tasks("new")).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

// Delivery (DL-1) ------------------------------------------------------------------------------

/// A worktree task on `feature/x` whose run (`exit 0`) has finished: `done` and awaiting review,
/// with its worktree holding a commit (`a.txt`, `b.txt`) and an untracked `untracked.txt`.
fn awaiting_task(env: &Env, title: &str) -> (Task, PathBuf) {
    let task = env.create(title, &[], |request| {
        request.placement = Some(TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("feature/x".into()),
            base: None,
        });
        request.run = Some(TaskRun::Terminal {
            argv: vec!["sh".into(), "-c".into(), "exit 0".into()],
        });
    });
    let _: TaskStarted = env.daemon.call(Request::TaskStart(TaskRef { id: task.id }));
    let deadline = Instant::now() + Duration::from_secs(20);
    let done = loop {
        let now = env.task(task.id).expect("the task exists");
        if now.state == TaskState::Done && now.delivery == DeliveryState::AwaitingReview {
            break now;
        }
        assert!(
            Instant::now() < deadline,
            "the task did not finish: {:?} {:?}",
            now.state,
            now.delivery
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let list: WorktreeList = env.daemon.call(Request::WorktreeList(WorktreeListRequest {
        workspace_id: Some(env.workspace),
    }));
    let wt = list
        .worktrees
        .into_iter()
        .find(|w| w.branch.as_deref() == Some("feature/x"))
        .expect("the task's worktree exists")
        .path;
    fs::write(wt.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    fs::write(wt.join("b.txt"), "b\n").unwrap();
    commit_all(&wt, "feature work");
    fs::write(wt.join("untracked.txt"), "u1\nu2\n").unwrap();
    (done, wt)
}

fn review(env: &Env, task: &Task, decision: ReviewDecision, note: Option<&str>) -> Task {
    let reviewed: TaskReviewed = env.daemon.call(Request::TaskReview(TaskReview {
        id: task.id,
        decision,
        note: note.map(str::to_owned),
    }));
    reviewed.task
}

#[gpui::test]
fn a_finished_task_shows_its_delivery_pill_and_needs_you_without_a_badge(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, _wt) = awaiting_task(&env, "Fix login");
    let session = *task.sessions.last().expect("the run has a session");
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "the task", 1);
    wait_until(cx, "the delivery pill", |cx| {
        region(&view, cx, Region::TaskDelivery(0)).is_some()
    });
    let row = rows(&panel, cx).remove(0);
    assert_eq!(
        (row.delivery, row.delivery_label),
        (DeliveryState::AwaitingReview, Some("Awaiting review"))
    );

    // The session is looked at and ended long ago; it still needs the user, softly: the
    // sidebar's Needs you lists it, the menu bar and the Dock do not count it.
    let sessions = view.read_with(cx, |wb, _| wb.sessions().clone());
    wait_until(cx, "the session", |cx| {
        sessions.read_with(cx, |s, _| s.session(session).is_some())
    });
    sessions.update(cx, |s, cx| s.mark_read(session, cx));
    sessions.read_with(cx, |s, _| {
        let presentation = s.presentation(s.session(session).unwrap());
        assert_eq!(
            (presentation.label.as_str(), presentation.needs_you),
            ("review", true)
        );
        assert_eq!(presentation.tone, Tone::Accent);
        assert_eq!(s.needs_you(), [session]);
        assert!(s.attention_entries().is_empty(), "a soft item has no badge");
    });

    // A task that waits for review stays listed when finished tasks are hidden …
    settings(&view, cx)
        .write_user("tasks.show_done", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "the setting", |cx| {
        panel.read_with(cx, |p, cx| p.rows(0, cx).len() == 1)
    });
    draw(cx);
    assert_eq!(rows(&panel, cx).len(), 1);

    // … until it is accepted: the pill changes, the Needs-you item and the row go.
    review(&env, &task, ReviewDecision::Accept, None);
    wait_until(cx, "the accepted pill", |cx| {
        rows(&panel, cx).first().map(|r| r.delivery_label) == Some(Some("Accepted"))
            || rows(&panel, cx).is_empty()
    });
    wait_until(cx, "the Needs-you item to go", |cx| {
        sessions.read_with(cx, |s, _| s.needs_you().is_empty())
    });
    wait_until(cx, "the finished task to be hidden", |cx| {
        rows(&panel, cx).is_empty()
    });
}

#[gpui::test]
fn the_header_accepts_the_work_of_the_task_on_the_branch(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the task of the view", |cx| {
        changes.read_with(cx, |c, cx| {
            c.delivery_state(cx) == Some(DeliveryState::AwaitingReview)
        })
    });
    for name in ["delivery", "accept", "request-changes", "merge"] {
        wait_until(cx, name, |cx| {
            region(&view, cx, Region::Changes(name)).is_some()
        });
    }
    assert!(changes.read_with(cx, |c, cx| c.can_review(cx)));

    click(&view, cx, Region::Changes("accept"));
    wait_until(cx, "the daemon to accept", |_| {
        env.task(task.id)
            .is_some_and(|t| t.delivery == DeliveryState::Accepted)
    });
    assert_eq!(
        env.task(task.id)
            .unwrap()
            .delivery_history
            .last()
            .unwrap()
            .by,
        DeliveryActor::Desktop
    );
    wait_until(cx, "the view to follow", |cx| {
        changes.read_with(cx, |c, cx| {
            c.delivery_state(cx) == Some(DeliveryState::Accepted)
        })
    });
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n == "Accepted “Fix login”")
    );
}

#[gpui::test]
fn requesting_changes_sends_the_note_and_cancel_leaves_the_task_alone(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the task of the view", |cx| {
        changes.read_with(cx, |c, cx| c.can_review(cx))
    });

    click(&view, cx, Region::Changes("request-changes"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.delivery_form()),
        Some(DeliveryForm::RequestChanges)
    );
    wait_until(cx, "the note field", |cx| {
        region(&view, cx, Region::Changes("request-note")).is_some()
    });
    click(&view, cx, Region::Changes("request-cancel"));
    assert_eq!(changes.read_with(cx, |c, _| c.delivery_form()), None);
    assert_eq!(
        env.task(task.id).unwrap().delivery,
        DeliveryState::AwaitingReview,
        "cancelling asks for nothing"
    );

    click(&view, cx, Region::Changes("request-changes"));
    changes.update(cx, |c, cx| {
        c.set_delivery_text("Handle the empty password", cx)
    });
    click(&view, cx, Region::Changes("request-send"));
    wait_until(cx, "the daemon to record the request", |_| {
        env.task(task.id)
            .is_some_and(|t| t.delivery == DeliveryState::ChangesRequested)
    });
    let asked = env.task(task.id).unwrap();
    assert_eq!(asked.change_request(), Some("Handle the empty password"));
    assert_eq!(
        asked.delivery_history.last().unwrap().by,
        DeliveryActor::Desktop
    );
    // A terminal run cannot be written to: the note stays on the task, and the task says so.
    wait_until(cx, "the form to close", |cx| {
        changes.read_with(cx, |c, _| c.delivery_form().is_none())
    });
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n.contains("the note stays on the task"))
    );
    // The Tasks panel row shows the pill and the note.
    draw(cx);
    wait_until(cx, "the Changes requested pill", |cx| {
        rows(&panel, cx).first().map(|r| r.delivery_label) == Some(Some("Changes requested"))
    });
    assert_eq!(
        rows(&panel, cx)[0].change_request.as_deref(),
        Some("Handle the empty password")
    );
}

#[gpui::test]
fn the_scope_says_what_a_merge_takes_and_commit_all_brings_the_rest(cx: &mut TestAppContext) {
    let env = Env::new();
    let (_task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the scope", |cx| {
        changes.read_with(cx, |c, _| {
            !c.commits().is_empty() && !c.uncommitted_files().is_empty()
        })
    });
    let paths = |files: Vec<&hutan_protocol::worktree::DiffFile>| -> Vec<String> {
        files.into_iter().map(|f| f.path.clone()).collect()
    };
    let (commits, committed, uncommitted) = changes.read_with(cx, |c, _| {
        (
            c.commits()
                .iter()
                .map(|commit| commit.subject.clone())
                .collect::<Vec<_>>(),
            paths(c.committed_files()),
            paths(c.uncommitted_files()),
        )
    });
    assert_eq!(commits, ["feature work"]);
    assert_eq!(committed, ["a.txt", "b.txt"]);
    assert_eq!(uncommitted, ["untracked.txt"]);
    for name in ["scope-committed", "scope-uncommitted", "commit-all"] {
        wait_until(cx, name, |cx| {
            region(&view, cx, Region::Changes(name)).is_some()
        });
    }

    // Leaving the uncommitted changes out of the list does not hide that they exist.
    click(&view, cx, Region::Changes("toggle"));
    wait_until(cx, "the committed-only list", |cx| {
        changes.read_with(cx, |c, _| {
            c.uncommitted_files().is_empty() && c.files().len() == 2
        })
    });
    assert!(changes.read_with(cx, |c, _| c.has_uncommitted()));
    wait_until(cx, "the uncommitted section", |cx| {
        region(&view, cx, Region::Changes("scope-uncommitted")).is_some()
    });
    click(&view, cx, Region::Changes("toggle"));
    wait_until(cx, "the full list", |cx| {
        changes.read_with(cx, |c, _| c.files().len() == 3)
    });

    // The merge form lists exactly the commits it integrates and warns about the rest.
    click(&view, cx, Region::Changes("merge"));
    wait_until(cx, "the merge preview", |cx| {
        region(&view, cx, Region::Changes("merge-commits")).is_some()
    });
    click(&view, cx, Region::Changes("merge-cancel"));

    // Commit all…: a message is required; then the worktree is clean and the commit is new.
    click(&view, cx, Region::Changes("commit-all"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.delivery_form()),
        Some(DeliveryForm::Commit)
    );
    click(&view, cx, Region::Changes("commit-confirm"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.delivery_form_error().map(str::to_owned)),
        Some("A commit needs a message.".to_owned())
    );
    changes.update(cx, |c, cx| c.set_delivery_text("Add the scratch file", cx));
    click(&view, cx, Region::Changes("commit-confirm"));
    wait_until(cx, "the commit to show in the scope", |cx| {
        changes.read_with(cx, |c, _| {
            c.commits().len() == 2
                && c.uncommitted_files().is_empty()
                && c.delivery_form().is_none()
        })
    });
    assert_eq!(
        git_out(&wt, &["log", "-1", "--format=%s"]).trim(),
        "Add the scratch file"
    );
    assert!(!changes.read_with(cx, |c, _| c.has_uncommitted()));
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n.starts_with("Committed 1 file on feature/x ("))
    );
    draw(cx);
    assert!(region(&view, cx, Region::Changes("scope-uncommitted")).is_none());
}

#[gpui::test]
fn merging_marks_the_task_merged_and_leftovers_default_the_cleanup_to_keep(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the task of the view", |cx| {
        changes.read_with(cx, |c, cx| c.can_review(cx))
    });
    click(&view, cx, Region::Changes("merge"));
    changes.update(cx, |c, cx| c.set_merge_message("Merge Fix login", cx));
    click(&view, cx, Region::Changes("merge-confirm"));
    wait_until(cx, "the merge result", |cx| {
        changes.read_with(cx, |c, _| c.merged().is_some())
    });
    let merged = changes.read_with(cx, |c, _| c.merged().cloned().unwrap());
    assert!(
        merged.source_dirty,
        "untracked.txt is not part of the merge"
    );

    // The task is merged, with the commit the merge made.
    wait_until(cx, "the task to be merged", |_| {
        env.task(task.id)
            .is_some_and(|t| t.delivery == DeliveryState::Merged)
    });
    let record = env
        .task(task.id)
        .unwrap()
        .delivery_history
        .last()
        .cloned()
        .unwrap();
    assert_eq!(record.commit.as_deref(), Some(merged.commit.as_str()));
    assert_eq!(record.strategy.as_deref(), Some("merge"));
    wait_until(cx, "the view to follow", |cx| {
        changes.read_with(cx, |c, cx| {
            c.delivery_state(cx) == Some(DeliveryState::Merged) && !c.can_review(cx)
        })
    });

    // Uncommitted changes remain: removing the worktree would discard them, so Keep is the
    // default and the offer says so.
    assert_eq!(
        changes.read_with(cx, |c, _| c.cleanup_default()),
        Some(CleanupDefault::Keep)
    );
    draw(cx);
    assert!(region(&view, cx, Region::Changes("remove")).is_some());
    click(&view, cx, Region::Changes("keep"));
    assert!(changes.read_with(cx, |c, _| c.merged().is_none()));
    assert!(wt.join("untracked.txt").exists(), "nothing was discarded");
}

#[gpui::test]
fn after_commit_all_nothing_is_left_and_removal_is_the_default(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    click(&view, cx, Region::Changes("commit-all"));
    changes.update(cx, |c, cx| c.set_delivery_text("Add the scratch file", cx));
    click(&view, cx, Region::Changes("commit-confirm"));
    wait_until(cx, "the commit", |cx| {
        changes.read_with(cx, |c, _| c.commits().len() == 2 && !c.has_uncommitted())
    });
    click(&view, cx, Region::Changes("merge"));
    click(&view, cx, Region::Changes("merge-confirm"));
    wait_until(cx, "the merge result", |cx| {
        changes.read_with(cx, |c, _| c.merged().is_some())
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.cleanup_default()),
        Some(CleanupDefault::Remove)
    );
    // The merge took the committed scratch file too.
    assert!(env.root.join("untracked.txt").exists());
    click(&view, cx, Region::Changes("remove"));
    wait_until(cx, "the worktree to go", |_| !wt.exists());
    // Removing a merged worktree discards nothing: the task stays merged.
    assert_eq!(env.task(task.id).unwrap().delivery, DeliveryState::Merged);
}
