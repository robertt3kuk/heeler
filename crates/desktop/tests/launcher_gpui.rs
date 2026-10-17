#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Start… sheet (S-1) against an in-process daemon: every entry point opens it with the right
//! project, a chat in a new worktree becomes a `task.create` with that placement and is started,
//! "Queue as task" does not start, "Start & new" keeps the sheet and its place, harnesses that
//! cannot run offer Install… / Log in…, the shared-checkout warning, ⌘⇧T in both keymap bases,
//! Tab order.
//! OMP is `hutan-acp-fixture replay`; the harnesses' own CLIs are shell scripts. No real
//! harness, login or model is touched.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use common::{TestDaemon, draw, open_options, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::{Task, TaskPlacementKind, TaskRun, TaskState, WorkspaceId};
use hutan_desktop::harness_install::FakeRunner;
use hutan_desktop::sessions::launch::LaunchKind;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::TasksPanel;
use hutan_desktop::workbench::{
    LaunchAgent, LaunchWhere, LauncherField, LauncherPreset, LauncherStop, Models, Readiness,
    Region, Workbench, WorkbenchOptions, WorktreeList,
};
use hutan_protocol::messages::Request;
use hutan_protocol::task::{TaskCreate, TaskList, TaskListRequest, TaskResult};
use hutan_protocol::worktree::{WorktreeList as DaemonWorktrees, WorktreeListRequest};
use hutan_settings::{SettingsPaths, SettingsStore};

// Fixtures ------------------------------------------------------------------------------------

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    bin
}

fn transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A daemon whose OMP replays `plain-turn` and is logged in to `anthropic`; Claude Code's CLI
/// reports a logged-out account unless `claude_cli` names a program that does not exist.
fn daemon(scripts: &Path, claude_cli: Option<&Path>) -> TestDaemon {
    let omp = script(
        scripts,
        "omp",
        r#"case "$1" in
  models) echo '{"models":[{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5"}]}' ;;
  usage) echo '{"reports":[]}' ;;
  *) echo "fake omp $*" ;;
esac"#,
    );
    let claude = claude_cli.map(Path::to_path_buf).unwrap_or_else(|| {
        script(
            scripts,
            "claude",
            r#"if [ "$1 $2" = "auth status" ]; then echo '{"loggedIn":false,"authMethod":"none"}'; else echo "fake claude $*"; fi"#,
        )
    });
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\nargs = [\"replay\", {t:?}, \"acp\"]\ncli = {omp:?}\n\n[harness.claude]\nbin = {missing:?}\ncli = {claude:?}\n",
        acp = acp_bin(),
        t = transcript("plain-turn"),
        missing = scripts.join("no-such-claude-code-acp"),
    ))
}

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

/// `<tmp>/<name>` on `main` with one commit and an identity the daemon's git calls find.
fn repo(dir: &Path, name: &str) -> PathBuf {
    let root = fs::canonicalize(dir).unwrap().join(name);
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
    fs::write(root.join("a.txt"), "one\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    root
}

struct Env {
    _scripts: tempfile::TempDir,
    folders: tempfile::TempDir,
    daemon: TestDaemon,
    api: PathBuf,
    api_id: WorkspaceId,
    web_id: WorkspaceId,
}

impl Env {
    fn new() -> Self {
        Self::with(false)
    }

    /// With `claude_missing`, Claude Code's CLI is not on the machine.
    fn with(claude_missing: bool) -> Self {
        let scripts = tempfile::tempdir().unwrap();
        let folders = tempfile::tempdir().unwrap();
        let missing = scripts.path().join("no-such-claude");
        let daemon = daemon(scripts.path(), claude_missing.then_some(missing.as_path()));
        let api = repo(folders.path(), "api");
        let web = repo(folders.path(), "web");
        let api_id = daemon.create_workspace("api", &[&api]).id;
        let web_id = daemon.create_workspace("web", &[&web]).id;
        Self {
            _scripts: scripts,
            folders,
            daemon,
            api,
            api_id,
            web_id,
        }
    }

    fn tasks(&self) -> Vec<Task> {
        self.daemon
            .call::<TaskList>(Request::TaskList(TaskListRequest::default()))
            .tasks
    }

    fn task(&self, title: &str) -> Option<Task> {
        self.tasks().into_iter().find(|t| t.title == title)
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    env: &Env,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings(env), env.daemon.connection(), None);
    synced(&view, cx);
    (view, cx)
}

/// Every project shows in the sidebar, sessions or not.
fn settings(env: &Env) -> SettingsStore {
    let settings = SettingsStore::new(SettingsPaths::in_dir(env.daemon.home()));
    settings
        .write_user("workspaces.hide_empty", toml::Value::Boolean(false))
        .unwrap();
    settings
}

fn synced(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced()
                && wb.sessions().read(cx).workspaces().len() >= 3
                && wb.active_workspace().is_some()
        })
    });
}

fn region(view: &Entity<Workbench>, cx: &VisualTestContext, region: Region) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, target: Region) {
    wait_until(cx, &format!("{target:?} to be drawn"), |cx| {
        region(view, cx, target)
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(target))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn launcher_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.launcher().is_some())
}

fn launcher_project(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<WorkspaceId> {
    view.read_with(cx, |wb, _| wb.launcher().and_then(|l| l.project()))
}

/// Waits until the sheet knows the worktrees of its project and OMP can be started.
fn ready(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the worktrees and the harnesses", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher()
                .is_some_and(|l| matches!(l.worktrees(), WorktreeList::Ready(_)))
                && wb
                    .launcher_harness_choices(cx)
                    .iter()
                    .any(|h| h.id == "omp" && h.enabled())
        })
    });
}

fn open_sheet(view: &Entity<Workbench>, cx: &mut VisualTestContext, preset: LauncherPreset) {
    view.update_in(cx, |wb, window, cx| wb.open_launcher(preset, window, cx));
    assert!(launcher_open(view, cx));
}

fn active_item(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<ItemRef> {
    view.read_with(cx, |wb, _| wb.state().active_item())
}

// Entry points --------------------------------------------------------------------------------

#[gpui::test]
fn cmd_shift_t_opens_the_sheet_in_both_keymap_bases(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    for base in ["zed", "hutan"] {
        view.read_with(cx, |wb, _| {
            wb.settings_store()
                .write_user("keymap.base", toml::Value::String(base.into()))
        })
        .unwrap();
        cx.run_until_parked();
        assert!(!launcher_open(&view, cx), "{base}: closed to begin with");
        cx.simulate_keystrokes("cmd-shift-t");
        assert!(launcher_open(&view, cx), "{base}: ⌘⇧T opens the sheet");
        assert!(
            view.read_with(cx, |wb, _| wb.agent_picker().is_none()),
            "{base}: and not the old picker"
        );
        cx.simulate_keystrokes("escape");
        assert!(!launcher_open(&view, cx), "{base}: Escape closes it");
    }
}

#[gpui::test]
fn the_sheet_opens_from_each_entry_point_with_the_right_project(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);

    // The project in focus: api.
    cx.dispatch_action(actions::OpenWorkspace(env.api_id.to_string()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.active_workspace()),
        Some(env.api_id)
    );

    // ⌘K "Start Agent or Task…" and the old "Session: New Agent…" are one command.
    cx.dispatch_action(actions::TogglePalette);
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    assert!(
        titles.iter().any(|t| t.as_ref() == "Start Agent or Task…"),
        "{titles:?}"
    );
    assert!(!titles.iter().any(|t| t.as_ref() == "Session: New Agent…"));
    view.update(cx, |wb, cx| wb.set_palette_query("Start Agent or Task", cx));
    cx.simulate_keystrokes("enter");
    assert!(launcher_open(&view, cx), "from the palette");
    assert_eq!(launcher_project(&view, cx), Some(env.api_id));
    assert!(
        !view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()),
        "Start now by default"
    );
    cx.simulate_keystrokes("escape");

    // The ▶ of the project row names its own project, not the one in focus.
    click(&view, cx, Region::ProjectStart(env.web_id));
    assert!(launcher_open(&view, cx), "from the project row");
    assert_eq!(launcher_project(&view, cx), Some(env.web_id));
    cx.simulate_keystrokes("escape");

    // The sidebar header's ▶ Start takes the project in focus.
    click(&view, cx, Region::SidebarControl("start"));
    assert!(launcher_open(&view, cx), "from the sidebar header");
    assert_eq!(launcher_project(&view, cx), Some(env.api_id));
    cx.simulate_keystrokes("escape");

    // The Tasks panel's "+ New task" opens it on "Queue as task".
    cx.dispatch_action(actions::ShowTasksPanel);
    draw(cx);
    click(&view, cx, Region::Tasks("new"));
    assert!(launcher_open(&view, cx), "from the Tasks panel");
    assert_eq!(launcher_project(&view, cx), Some(env.api_id));
    assert!(view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()));
    cx.simulate_keystrokes("escape");

    // So does the panel's empty state.
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<TasksPanel>("tasks"))
        .expect("the panel is open");
    assert!(panel.read_with(cx, |p, cx| p.rows(0, cx).is_empty()));
    click(&view, cx, Region::Tasks("empty-new"));
    assert!(launcher_open(&view, cx), "from the Tasks empty state");
    assert!(view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()));
    cx.simulate_keystrokes("escape");

    // And "Tasks: New Task…" in the palette.
    cx.dispatch_action(actions::NewTask);
    assert!(launcher_open(&view, cx), "from Tasks: New Task…");
    assert!(view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()));
}

#[gpui::test]
fn an_empty_project_offers_start_in_its_empty_pane(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    cx.dispatch_action(actions::OpenWorkspace(env.web_id.to_string()));
    wait_until(cx, "the empty pane's Start", |cx| {
        region(&view, cx, Region::Launcher("empty-start"))
    });
    click(&view, cx, Region::Launcher("empty-start"));
    assert_eq!(launcher_project(&view, cx), Some(env.web_id));
}

// Starting ------------------------------------------------------------------------------------

#[gpui::test]
fn a_chat_in_a_new_worktree_creates_the_task_with_that_placement_and_starts_it(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    click(&view, cx, Region::ProjectStart(env.api_id));
    ready(&view, cx);

    // Keyboard first: the caret is in the prompt; the title and the branch follow it.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().stop()),
        LauncherStop::Prompt
    );
    cx.simulate_input("Fix the flaky test");
    click(&view, cx, Region::LauncherChoice("where", "new"));
    view.read_with(cx, |wb, _| {
        let l = wb.launcher().unwrap();
        assert_eq!(l.kind(), LaunchKind::Chat);
        assert_eq!(l.agent(), &LaunchAgent::Harness("omp".into()));
        assert_eq!(l.prompt(), "Fix the flaky test");
        assert_eq!(l.title(), "Fix the flaky test");
        assert_eq!(l.place(), &LaunchWhere::NewWorktree);
        assert_eq!(l.branch(), "hutan/fix-the-flaky-test");
        assert_eq!(l.base(), "main", "from the branch the main checkout is on");
        assert!(!l.queue());
    });

    click(&view, cx, Region::Launcher("start"));
    // (A chat task is done when its first turn ends, which the replayed transcript does at once.)
    wait_until(cx, "the task to start", |_| {
        env.task("Fix the flaky test")
            .is_some_and(|t| t.sessions.len() == 1)
    });
    let task = env.task("Fix the flaky test").unwrap();
    assert!(matches!(task.state, TaskState::Running | TaskState::Done));
    assert_eq!(task.placement.kind, TaskPlacementKind::Worktree);
    assert_eq!(
        task.placement.branch.as_deref(),
        Some("hutan/fix-the-flaky-test")
    );
    assert_eq!(task.placement.base.as_deref(), Some("main"));
    assert_eq!(
        task.run,
        Some(TaskRun::Chat {
            agent: "omp".into(),
            model: None,
        })
    );
    assert_eq!(task.prompt.as_deref(), Some("Fix the flaky test"));
    assert!(!task.auto_start, "Start now is not the scheduler's");

    // The worktree is made, and the session works in it.
    let list: DaemonWorktrees = env.daemon.call(Request::WorktreeList(WorktreeListRequest {
        workspace_id: Some(env.api_id),
    }));
    let made = list
        .worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some("hutan/fix-the-flaky-test"))
        .expect("the task's worktree");
    assert!(!made.main);
    assert_eq!(made.live_sessions, task.sessions);

    // The sheet closed and the new session is open and focused in the center.
    wait_until(cx, "the chat tab", |cx| {
        !launcher_open(&view, cx)
            && active_item(&view, cx).is_some_and(|i| matches!(i, ItemRef::Chat { .. }))
    });
    let focused = view.read_with(cx, |wb, _| wb.focused_session());
    assert_eq!(focused, Some(task.sessions[0]));
}

#[gpui::test]
fn queue_as_task_does_not_start_it(cx: &mut TestAppContext) {
    let env = Env::new();
    // A task nobody finished: the queued one waits for it, so the scheduler leaves it alone.
    let first: TaskResult = env.daemon.call(Request::TaskCreate(TaskCreate {
        workspace_id: env.api_id,
        title: "Plan".into(),
        prompt: None,
        depends_on: Vec::new(),
        placement: None,
        run: Some(TaskRun::Terminal {
            argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
        }),
        auto_start: false,
        after_policy: Default::default(),
    }));
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Command),
            queue: true,
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Command, "cargo test", cx);
        wb.launcher_toggle_after(first.task.id, cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().title().to_owned()),
        "cargo test"
    );
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the task to exist", |_| {
        env.task("cargo test").is_some()
    });
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));

    let queued = env.task("cargo test").unwrap();
    assert!(queued.auto_start, "the scheduler starts it when ready");
    assert_eq!(queued.depends_on, [first.task.id]);
    assert_eq!(
        queued.run,
        Some(TaskRun::Terminal {
            argv: vec!["cargo".into(), "test".into()]
        })
    );
    // The sheet never asked for it to run.
    assert_eq!(queued.state, TaskState::Todo);
    assert!(queued.sessions.is_empty());
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len()),
        0,
        "no session was started"
    );
}

#[gpui::test]
fn start_and_new_keeps_the_sheet_and_what_and_where_and_clears_the_prompt(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Command),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Command, "sleep 61", cx);
        wb.launcher_set_place(LaunchWhere::NewWorktree, cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().branch().to_owned()),
        "hutan/sleep-61"
    );
    click(&view, cx, Region::Launcher("start-new"));
    wait_until(cx, "the first task to run", |_| {
        env.task("sleep 61")
            .is_some_and(|t| t.state == TaskState::Running)
    });
    wait_until(cx, "the sheet to be cleared", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .is_some_and(|l| !l.starting() && l.command().is_empty())
        })
    });
    view.read_with(cx, |wb, _| {
        let l = wb.launcher().expect("the sheet stays open");
        assert_eq!(l.project(), Some(env.api_id), "the project is kept");
        assert_eq!(l.kind(), LaunchKind::Command, "what is kept");
        assert_eq!(l.place(), &LaunchWhere::NewWorktree, "where is kept");
        assert_eq!(l.title(), "", "the title is cleared");
        assert_eq!(l.prompt(), "", "the prompt is cleared");
        assert_eq!(l.branch(), "", "the branch follows the (empty) title");
    });

    // The next one goes the same way, into a worktree of its own.
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Command, "sleep 62", cx);
    });
    cx.simulate_keystrokes("cmd-alt-enter");
    wait_until(cx, "the second task to run", |_| {
        env.task("sleep 62")
            .is_some_and(|t| t.state == TaskState::Running)
    });
    wait_until(cx, "the sheet to be ready again", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .is_some_and(|l| !l.starting() && l.command().is_empty())
        })
    });
    let one = env.task("sleep 61").unwrap();
    let two = env.task("sleep 62").unwrap();
    assert_eq!(one.placement.branch.as_deref(), Some("hutan/sleep-61"));
    assert_eq!(two.placement.branch.as_deref(), Some("hutan/sleep-62"));
    assert!(launcher_open(&view, cx), "still open for the next one");
    // ⌘↩ starts a last one and closes it.
    view.update(cx, |wb, cx| {
        wb.launcher_set_place(LaunchWhere::Main, cx);
        wb.launcher_set_field(LauncherField::Command, "sleep 63", cx);
    });
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the third task to run and the sheet to close", |cx| {
        env.task("sleep 63")
            .is_some_and(|t| t.state == TaskState::Running)
            && !launcher_open(&view, cx)
    });
}

#[gpui::test]
fn a_checkout_preset_chooses_that_worktree(cx: &mut TestAppContext) {
    let env = Env::new();
    // A worktree of api, made as `worktree.create` would.
    let wt = env.folders.path().join("api-wt-topic");
    git(
        &env.api,
        &["worktree", "add", "-q", "-b", "topic", wt.to_str().unwrap()],
    );
    let wt = fs::canonicalize(&wt).unwrap();
    let (view, cx) = open(cx, &env);
    view.update_in(cx, |wb, window, cx| {
        wb.start_agent_with(
            wt.clone(),
            "Address the review comments".into(),
            None,
            window,
            cx,
        );
    });
    // The worktree is not inside the project's folder: git says whose it is.
    wait_until(cx, "the sheet and its worktrees", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .is_some_and(|l| matches!(l.worktrees(), WorktreeList::Ready(_)))
        })
    });
    wait_until(cx, "the checkout to be chosen", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .is_some_and(|l| l.place() != &LaunchWhere::Main)
        })
    });
    view.read_with(cx, |wb, _| {
        let l = wb.launcher().unwrap();
        assert_eq!(l.project(), Some(env.api_id));
        assert_eq!(l.kind(), LaunchKind::Chat);
        assert_eq!(l.prompt(), "Address the review comments");
        assert_eq!(l.place(), &LaunchWhere::Existing("topic".into()));
    });
}

// Harnesses -----------------------------------------------------------------------------------

#[gpui::test]
fn a_harness_that_is_logged_out_is_disabled_with_log_in(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    wait_until(cx, "the accounts", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .all(|h| h.readiness != Readiness::Checking)
        })
    });
    view.read_with(cx, |wb, cx| {
        let choices = wb.launcher_harness_choices(cx);
        let of = |id: &str| choices.iter().find(|h| h.id == id).unwrap();
        assert!(of("omp").enabled(), "installed and logged in");
        assert_eq!(of("claude").readiness, Readiness::NeedsLogin);
        assert!(!of("claude").enabled());
    });
    // It cannot be picked…
    view.update(cx, |wb, cx| {
        wb.launcher_choose_agent(LaunchAgent::Harness("claude".into()), cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().agent().clone()),
        LaunchAgent::Harness("omp".into())
    );
    // …and offers Log in…, which starts the harness's own flow in a terminal.
    draw(cx);
    assert!(region(&view, cx, Region::LauncherChoice("login", "claude")));
    assert!(!region(&view, cx, Region::LauncherChoice("login", "omp")));
    click(&view, cx, Region::LauncherChoice("login", "claude"));
    wait_until(cx, "the login session to be shown", |cx| {
        view.read_with(cx, |wb, cx| {
            !launcher_open_in(wb)
                && wb.state().active_item().is_some_and(|item| match item {
                    ItemRef::Terminal { session } => session
                        .parse()
                        .ok()
                        .and_then(|id| wb.sessions().read(cx).session(id).cloned())
                        .is_some_and(|s| s.title.contains("Log in to Claude Code")),
                    _ => false,
                })
        })
    });
}

fn launcher_open_in(wb: &Workbench) -> bool {
    wb.launcher().is_some()
}

#[gpui::test]
fn a_harness_that_is_not_installed_is_disabled_with_install(cx: &mut TestAppContext) {
    let env = Env::with(true);
    let options = WorkbenchOptions::builtin(settings(&env), env.daemon.connection(), None)
        .with_package_runner(Arc::new(FakeRunner::new()));
    let (view, cx) = open_options(cx, options);
    synced(&view, cx);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    wait_until(cx, "Claude Code to be found missing", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "claude" && matches!(h.readiness, Readiness::NeedsInstall(_)))
        })
    });
    assert!(view.read_with(cx, |wb, cx| {
        let choices = wb.launcher_harness_choices(cx);
        choices.iter().any(|h| h.id == "omp" && h.enabled())
            && !choices.iter().find(|h| h.id == "claude").unwrap().enabled()
    }));
    view.update(cx, |wb, cx| {
        wb.launcher_choose_agent(LaunchAgent::Harness("claude".into()), cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().agent().clone()),
        LaunchAgent::Harness("omp".into()),
        "a missing harness cannot be picked"
    );
    draw(cx);
    assert!(region(
        &view,
        cx,
        Region::LauncherChoice("install", "claude")
    ));
    // Install… asks the package registry first; it never installs before the user confirms.
    click(&view, cx, Region::LauncherChoice("install", "claude"));
    wait_until(cx, "the install to be looked at", |cx| {
        view.read_with(cx, |wb, _| {
            wb.modal().is_some() || wb.notice().is_some() || wb.harness_install_checking()
        })
    });
    assert!(
        view.read_with(cx, |wb, _| wb.harness_installs_running().is_empty()),
        "nothing runs before the user confirms"
    );
}

// The shared checkout -------------------------------------------------------------------------

#[gpui::test]
fn the_sheet_warns_when_another_live_agent_works_in_the_chosen_checkout(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    // The first agent starts in api's main checkout.
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            prompt: Some("First agent".into()),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    assert!(
        view.read_with(cx, |wb, cx| wb.launcher_shared_warning(cx))
            .is_none(),
        "nobody works there yet"
    );
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the first agent to start", |_| {
        env.task("First agent")
            .is_some_and(|t| t.sessions.len() == 1)
    });
    wait_until(cx, "its chat tab", |cx| {
        !launcher_open(&view, cx)
            && active_item(&view, cx).is_some_and(|i| matches!(i, ItemRef::Chat { .. }))
    });

    // The second sheet, on the same main checkout, says so; a new worktree does not.
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            prompt: Some("Second agent".into()),
            ..LauncherPreset::default()
        },
    );
    wait_until(cx, "the warning", |cx| {
        view.read_with(cx, |wb, cx| wb.launcher_shared_warning(cx).is_some())
    });
    draw(cx);
    assert!(region(&view, cx, Region::Launcher("shared-warning")));
    let warning = view
        .read_with(cx, |wb, cx| wb.launcher_shared_warning(cx))
        .unwrap();
    assert!(warning.contains("Another live agent"), "{warning}");

    view.update(cx, |wb, cx| {
        wb.launcher_set_place(LaunchWhere::NewWorktree, cx)
    });
    draw(cx);
    assert!(
        view.read_with(cx, |wb, cx| wb.launcher_shared_warning(cx))
            .is_none()
    );
    assert!(!region(&view, cx, Region::Launcher("shared-warning")));

    // A plain terminal is not an agent: no warning for it either.
    view.update(cx, |wb, cx| {
        wb.launcher_set_place(LaunchWhere::Main, cx);
        wb.launcher_set_kind(LaunchKind::Terminal, cx);
    });
    assert!(
        view.read_with(cx, |wb, cx| wb.launcher_shared_warning(cx))
            .is_none()
    );
}

// Keyboard ------------------------------------------------------------------------------------

#[gpui::test]
fn tab_walks_the_fields_top_to_bottom_and_the_project_is_a_fuzzy_picker(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    wait_until(&mut *cx, "the models", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx).iter().any(|h| h.enabled())
                && matches!(wb.launcher_models(cx), Models::Ready(m) if !m.is_empty())
                && wb
                    .launcher()
                    .is_some_and(|l| matches!(l.worktrees(), WorktreeList::Ready(_)))
        })
    });
    let stop = |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.launcher().unwrap().stop());
    // The caret starts in the prompt; Tab walks down and around.
    let mut walked = vec![stop(cx)];
    for _ in 0..9 {
        cx.simulate_keystrokes("tab");
        walked.push(stop(cx));
    }
    assert_eq!(
        walked,
        [
            LauncherStop::Prompt,
            LauncherStop::Where,
            LauncherStop::Title,
            LauncherStop::More,
            LauncherStop::Mode,
            LauncherStop::Project,
            LauncherStop::What,
            LauncherStop::Agent,
            LauncherStop::Model,
            LauncherStop::Prompt,
        ]
    );
    // Shift-Tab goes back up.
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(stop(cx), LauncherStop::Model);

    // The project is a fuzzy picker: Tab around to it, type to filter, ↩ chooses.
    for _ in 0..3 {
        cx.simulate_keystrokes("shift-tab");
    }
    assert_eq!(stop(cx), LauncherStop::Project);
    cx.simulate_input("web");
    cx.simulate_keystrokes("enter");
    assert_eq!(launcher_project(&view, cx), Some(env.web_id));
}

#[gpui::test]
fn a_picked_model_goes_into_the_task_even_when_it_is_queued(cx: &mut TestAppContext) {
    let env = Env::new();
    let first: TaskResult = env.daemon.call(Request::TaskCreate(TaskCreate {
        workspace_id: env.api_id,
        title: "Plan".into(),
        prompt: None,
        depends_on: Vec::new(),
        placement: None,
        run: Some(TaskRun::Terminal {
            argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
        }),
        auto_start: false,
        after_policy: Default::default(),
    }));
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            prompt: Some("Review the diff".into()),
            queue: true,
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    wait_until(cx, "the OMP models", |cx| {
        view.read_with(
            cx,
            |wb, cx| matches!(wb.launcher_models(cx), Models::Ready(m) if !m.is_empty()),
        )
    });
    let model = view.read_with(cx, |wb, cx| match wb.launcher_models(cx) {
        Models::Ready(models) => models[0].id.clone(),
        _ => unreachable!(),
    });
    view.update(cx, |wb, cx| {
        wb.launcher_choose_model(Some(model.clone()), cx);
        wb.launcher_toggle_after(first.task.id, cx);
    });
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the task to exist", |_| {
        env.task("Review the diff").is_some()
    });
    let queued = env.task("Review the diff").unwrap();
    assert_eq!(
        queued.run,
        Some(TaskRun::Chat {
            agent: "omp".into(),
            model: Some(model)
        }),
        "the pick rides in the task, queued or not"
    );
    assert_eq!(queued.state, TaskState::Todo);
}
