//! Tasks with dependencies (roadmap M7-B): the model, its state machine, and the pure rules the
//! daemon applies to a task graph — readiness, cycle detection, ordering, and which ready tasks
//! start next within a parallelism limit. No I/O lives here.
//!
//! A task is stored in one of five states (`todo`, `running`, `done`, `failed`, `cancelled`).
//! Two more are *derived* when a task is shown: `ready` is a `todo` task whose dependencies are
//! all `done`, and `blocked` is a `running` task whose agent waits for the user. Neither is
//! stored as truth: [`effective_state`] computes them.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, TaskId, WorkspaceId};

/// Longest task title, in characters.
pub const TITLE_MAX_CHARS: usize = 200;
/// Longest task prompt, in bytes.
pub const PROMPT_MAX_BYTES: usize = 64 * 1024;
/// Most dependencies one task lists.
pub const MAX_DEPENDENCIES: usize = 64;
/// Most words and bytes of a terminal run's `argv`.
pub const MAX_ARGV_WORDS: usize = 256;
pub const MAX_ARGV_BYTES: usize = 64 * 1024;
/// Longest slug in a default branch name.
const SLUG_MAX_CHARS: usize = 40;

/// Where a task is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Waiting for its dependencies (or for somebody to start it).
    Todo,
    /// Every dependency is `done`; it can be started. Derived, see [`effective_state`].
    Ready,
    /// A session works on it.
    Running,
    /// `running`, and the agent in its session waits for a decision. Derived.
    Blocked,
    Done,
    Failed,
    Cancelled,
}

impl TaskState {
    pub const ALL: [Self; 7] = [
        Self::Todo,
        Self::Ready,
        Self::Running,
        Self::Blocked,
        Self::Done,
        Self::Failed,
        Self::Cancelled,
    ];

    /// The wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// `done`, `failed` or `cancelled`: nothing more happens to the task unless it is reset.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// A session works on the task (`running` or `blocked`).
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Blocked)
    }

    /// The state that is written to storage: derived states fold into the one they derive from.
    pub const fn stored(self) -> Self {
        match self {
            Self::Ready => Self::Todo,
            Self::Blocked => Self::Running,
            other => other,
        }
    }

    /// Applies `event`; the state it leads to.
    ///
    /// ```text
    /// todo/ready ──start──▶ running ◀──unblock── blocked
    ///                          │  └────block────────▲
    ///                          ├─▶ done   (complete; also from todo/ready/blocked/failed)
    ///                          └─▶ failed (fail)
    /// todo/ready/running/blocked ──cancel──▶ cancelled
    /// done/failed/cancelled ──reset──▶ todo
    /// ```
    pub fn transition(self, event: TaskEvent) -> Result<Self, InvalidTaskTransition> {
        use TaskEvent as E;
        use TaskState as S;
        let next = match (self, event) {
            (S::Todo | S::Ready, E::Start) => S::Running,
            (S::Running, E::Block) => S::Blocked,
            (S::Blocked, E::Unblock) => S::Running,
            (S::Todo | S::Ready | S::Running | S::Blocked | S::Failed, E::Complete) => S::Done,
            (S::Running | S::Blocked, E::Fail) => S::Failed,
            (S::Todo | S::Ready | S::Running | S::Blocked, E::Cancel) => S::Cancelled,
            (S::Todo | S::Ready | S::Done | S::Failed | S::Cancelled, E::Reset) => S::Todo,
            _ => return Err(InvalidTaskTransition { from: self, event }),
        };
        Ok(next)
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| format!("unknown task state `{s}`"))
    }
}

/// What happens to a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskEvent {
    /// A session was started for it.
    Start,
    /// Its agent waits for the user.
    Block,
    /// Its agent works again.
    Unblock,
    /// It finished successfully (or the user says so).
    Complete,
    /// It failed.
    Fail,
    /// The user gave it up.
    Cancel,
    /// Back to `todo`: it will be run again.
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {from} task cannot take {event:?}")]
pub struct InvalidTaskTransition {
    pub from: TaskState,
    pub event: TaskEvent,
}

/// Where a task's session works.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskPlacement {
    pub kind: TaskPlacementKind,
    /// `worktree` only: the branch; absent means `task/<slug of the title>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `worktree` only: where a new branch starts; absent means `HEAD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPlacementKind {
    /// The repository's main checkout (or the workspace's first root when it is no repository).
    #[default]
    Main,
    /// A git worktree: made for the task, or the existing one of its branch.
    Worktree,
}

/// What a task runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskRun {
    /// A terminal session running `argv` (never empty).
    Terminal { argv: Vec<String> },
    /// A chat session with an agent (`codex`); the task's prompt is the first message.
    Chat { agent: String },
}

/// A unit of work in a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// As clients see it: `ready` and `blocked` are derived (see [`effective_state`]).
    pub state: TaskState,
    /// Tasks that must be `done` first.
    pub depends_on: Vec<TaskId>,
    pub placement: TaskPlacement,
    /// Absent: starting the task opens a login shell in the placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<TaskRun>,
    /// Start as soon as the task is ready and a `[tasks] max_parallel` slot is free.
    #[serde(default)]
    pub auto_start: bool,
    /// Every session that ran it, oldest first.
    pub sessions: Vec<SessionId>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// Why it ended: `exited 0`, `exited 3`, `agent finished`, `turn completed`, `interrupted`,
    /// `marked done`, … Short, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// The state a task is shown in. `stored` is what the daemon keeps; `blocked` says the agent of
/// a running task waits for the user; `state_of` gives the stored state of a dependency (a
/// dependency that does not exist is not `done`).
pub fn effective_state(
    stored: TaskState,
    blocked: bool,
    depends_on: &[TaskId],
    state_of: impl Fn(TaskId) -> Option<TaskState>,
) -> TaskState {
    match stored.stored() {
        TaskState::Todo
            if depends_on
                .iter()
                .all(|dep| state_of(*dep) == Some(TaskState::Done)) =>
        {
            TaskState::Ready
        }
        TaskState::Running if blocked => TaskState::Blocked,
        other => other,
    }
}

/// The cycle that giving `id` the dependencies `new_deps` would create, as the path
/// `[id, a, b, …, id]` (each task depends on the next), or `None`. `edges` holds the current
/// dependencies of every task; what it says about `id` itself is ignored in favour of
/// `new_deps`.
pub fn find_cycle(
    edges: &HashMap<TaskId, Vec<TaskId>>,
    id: TaskId,
    new_deps: &[TaskId],
) -> Option<Vec<TaskId>> {
    let deps_of = |node: TaskId| -> &[TaskId] {
        if node == id {
            new_deps
        } else {
            edges.get(&node).map_or(&[], Vec::as_slice)
        }
    };
    let mut visited: HashSet<TaskId> = HashSet::from([id]);
    // The path from `id` to the node being expanded, with how many of its dependencies were
    // looked at already.
    let mut path: Vec<(TaskId, usize)> = vec![(id, 0)];
    while let Some((node, next)) = path.last().copied() {
        let Some(dep) = deps_of(node).get(next).copied() else {
            path.pop();
            continue;
        };
        if let Some(top) = path.last_mut() {
            top.1 += 1;
        }
        if dep == id {
            let mut cycle: Vec<TaskId> = path.iter().map(|(n, _)| *n).collect();
            cycle.push(id);
            return Some(cycle);
        }
        if visited.insert(dep) {
            path.push((dep, 0));
        }
    }
    None
}

/// Every task after the tasks it depends on; among tasks that could go in any order, the older
/// first (`created_at_ms`, then id). Dependencies outside `tasks` are ignored. Tasks in a cycle
/// (which the daemon never lets in) come last, by age.
pub fn topological_order<'a>(tasks: impl IntoIterator<Item = &'a Task>) -> Vec<TaskId> {
    let tasks: Vec<&Task> = tasks.into_iter().collect();
    let known: HashSet<TaskId> = tasks.iter().map(|t| t.id).collect();
    let key = |t: &Task| (t.created_at_ms, t.id);
    let mut waiting_on: HashMap<TaskId, usize> = HashMap::new();
    let mut dependants: HashMap<TaskId, Vec<&Task>> = HashMap::new();
    for task in &tasks {
        let deps: HashSet<TaskId> = task
            .depends_on
            .iter()
            .copied()
            .filter(|dep| known.contains(dep) && *dep != task.id)
            .collect();
        waiting_on.insert(task.id, deps.len());
        for dep in deps {
            dependants.entry(dep).or_default().push(task);
        }
    }
    let mut ready: BinaryHeap<Reverse<(u64, TaskId)>> = tasks
        .iter()
        .filter(|t| waiting_on.get(&t.id) == Some(&0))
        .map(|t| Reverse(key(t)))
        .collect();
    let mut order = Vec::with_capacity(tasks.len());
    while let Some(Reverse((_, id))) = ready.pop() {
        order.push(id);
        for dependant in dependants.get(&id).into_iter().flatten() {
            if let Some(count) = waiting_on.get_mut(&dependant.id) {
                *count -= 1;
                if *count == 0 {
                    ready.push(Reverse(key(dependant)));
                }
            }
        }
    }
    if order.len() < tasks.len() {
        let placed: HashSet<TaskId> = order.iter().copied().collect();
        let mut rest: Vec<&Task> = tasks
            .iter()
            .copied()
            .filter(|t| !placed.contains(&t.id))
            .collect();
        rest.sort_by_key(|t| key(t));
        order.extend(rest.into_iter().map(|t| t.id));
    }
    order
}

/// The default branch of a worktree task: `task/<slug of the title>`.
pub fn default_branch(title: &str) -> String {
    format!("task/{}", slug(title))
}

/// A title as a branch-name part: lower-case ASCII letters and digits, anything else a single
/// `-`, at most 40 characters, never empty.
pub fn slug(title: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c.to_ascii_lowercase());
            if slug.len() >= SLUG_MAX_CHARS {
                break;
            }
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        slug.push_str("task");
    }
    slug
}

/// Decides which ready tasks that start by themselves (`auto_start`) are started next: they
/// wait in the order they became ready, and as many leave the queue as `max_parallel` allows
/// next to the tasks already active.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scheduler {
    queue: VecDeque<TaskId>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues `id` behind the others; a task already waiting keeps its place.
    pub fn enqueue(&mut self, id: TaskId) {
        if !self.queue.contains(&id) {
            self.queue.push_back(id);
        }
    }

    /// Forgets `id` (it started, stopped being ready, or was deleted).
    pub fn remove(&mut self, id: TaskId) {
        self.queue.retain(|queued| *queued != id);
    }

    pub fn contains(&self, id: TaskId) -> bool {
        self.queue.contains(&id)
    }

    /// The tasks waiting, in the order they will start.
    pub fn waiting(&self) -> impl Iterator<Item = TaskId> + '_ {
        self.queue.iter().copied()
    }

    /// Takes the tasks to start now: `max_parallel - active` of them (none when the limit is
    /// reached), oldest first.
    pub fn take(&mut self, active: usize, max_parallel: usize) -> Vec<TaskId> {
        let free = max_parallel.saturating_sub(active).min(self.queue.len());
        self.queue.drain(..free).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(created: u64, deps: &[TaskId]) -> Task {
        Task {
            id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            title: format!("t{created}"),
            prompt: None,
            state: TaskState::Todo,
            depends_on: deps.to_vec(),
            placement: TaskPlacement::default(),
            run: None,
            auto_start: false,
            sessions: Vec::new(),
            created_at_ms: created,
            started_at_ms: None,
            ended_at_ms: None,
            outcome: None,
        }
    }

    #[test]
    fn the_life_of_a_task() {
        use TaskEvent as E;
        use TaskState as S;
        assert_eq!(S::Todo.transition(E::Start), Ok(S::Running));
        assert_eq!(S::Ready.transition(E::Start), Ok(S::Running));
        assert_eq!(S::Running.transition(E::Block), Ok(S::Blocked));
        assert_eq!(S::Blocked.transition(E::Unblock), Ok(S::Running));
        assert_eq!(S::Blocked.transition(E::Complete), Ok(S::Done));
        assert_eq!(S::Running.transition(E::Fail), Ok(S::Failed));
        assert_eq!(S::Blocked.transition(E::Fail), Ok(S::Failed));
        assert_eq!(S::Running.transition(E::Cancel), Ok(S::Cancelled));
        assert_eq!(S::Todo.transition(E::Cancel), Ok(S::Cancelled));
        // The user may say a task is done without it ever running, or after it failed.
        assert_eq!(S::Todo.transition(E::Complete), Ok(S::Done));
        assert_eq!(S::Failed.transition(E::Complete), Ok(S::Done));
        for ended in [S::Done, S::Failed, S::Cancelled] {
            assert_eq!(ended.transition(E::Reset), Ok(S::Todo));
        }
    }

    #[test]
    fn what_cannot_happen_is_refused() {
        use TaskEvent as E;
        use TaskState as S;
        for ended in [S::Done, S::Failed, S::Cancelled] {
            for event in [E::Start, E::Block, E::Unblock, E::Fail, E::Cancel] {
                assert!(ended.transition(event).is_err(), "{ended} took {event:?}");
            }
        }
        assert!(S::Cancelled.transition(E::Complete).is_err());
        assert!(S::Done.transition(E::Complete).is_err());
        assert!(S::Running.transition(E::Start).is_err());
        assert!(S::Running.transition(E::Reset).is_err());
        assert!(S::Blocked.transition(E::Reset).is_err());
        assert!(S::Todo.transition(E::Fail).is_err());
        assert!(S::Todo.transition(E::Block).is_err());
        assert!(S::Running.transition(E::Unblock).is_err());
        let error = S::Done.transition(E::Start).unwrap_err();
        assert_eq!(error.to_string(), "a done task cannot take Start");
    }

    #[test]
    fn derived_states_are_stored_as_the_state_they_derive_from() {
        assert_eq!(TaskState::Ready.stored(), TaskState::Todo);
        assert_eq!(TaskState::Blocked.stored(), TaskState::Running);
        for state in [
            TaskState::Todo,
            TaskState::Running,
            TaskState::Done,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            assert_eq!(state.stored(), state);
        }
        for state in TaskState::ALL {
            assert_eq!(state.as_str().parse::<TaskState>(), Ok(state));
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                format!("\"{}\"", state.as_str())
            );
        }
        assert!("paused".parse::<TaskState>().is_err());
    }

    #[test]
    fn a_task_is_ready_when_every_dependency_is_done() {
        let (a, b, c) = (TaskId::new(), TaskId::new(), TaskId::new());
        let states: HashMap<TaskId, TaskState> = [
            (a, TaskState::Done),
            (b, TaskState::Running),
            (c, TaskState::Failed),
        ]
        .into();
        let shown = |stored, blocked, deps: &[TaskId]| {
            effective_state(stored, blocked, deps, |id| states.get(&id).copied())
        };
        assert_eq!(shown(TaskState::Todo, false, &[]), TaskState::Ready);
        assert_eq!(shown(TaskState::Todo, false, &[a]), TaskState::Ready);
        assert_eq!(shown(TaskState::Todo, false, &[a, b]), TaskState::Todo);
        assert_eq!(
            shown(TaskState::Todo, false, &[c]),
            TaskState::Todo,
            "a failed dependency does not release its dependants"
        );
        assert_eq!(
            shown(TaskState::Todo, false, &[TaskId::new()]),
            TaskState::Todo,
            "a dependency that is gone is not done"
        );
        assert_eq!(shown(TaskState::Running, false, &[b]), TaskState::Running);
        assert_eq!(shown(TaskState::Running, true, &[]), TaskState::Blocked);
        // Blocked is only for running tasks; the stored states pass through.
        assert_eq!(shown(TaskState::Done, true, &[b]), TaskState::Done);
        assert_eq!(
            shown(TaskState::Cancelled, false, &[]),
            TaskState::Cancelled
        );
        // A derived state handed back in is folded first.
        assert_eq!(shown(TaskState::Ready, false, &[b]), TaskState::Todo);
    }

    fn edges(tasks: &[&Task]) -> HashMap<TaskId, Vec<TaskId>> {
        tasks.iter().map(|t| (t.id, t.depends_on.clone())).collect()
    }

    #[test]
    fn a_cycle_is_found_and_named() {
        let a = task(1, &[]);
        let b = task(2, &[a.id]);
        let c = task(3, &[b.id]);
        let graph = edges(&[&a, &b, &c]);

        // a → c would close a → c → b → a.
        assert_eq!(
            find_cycle(&graph, a.id, &[c.id]),
            Some(vec![a.id, c.id, b.id, a.id])
        );
        // A task cannot depend on itself.
        assert_eq!(find_cycle(&graph, a.id, &[a.id]), Some(vec![a.id, a.id]));
        assert_eq!(find_cycle(&graph, c.id, &[c.id]), Some(vec![c.id, c.id]));
        // Direct and indirect, with an innocent branch beside it.
        let d = task(4, &[]);
        let mut graph = graph;
        graph.insert(d.id, vec![]);
        assert_eq!(
            find_cycle(&graph, a.id, &[d.id, b.id]),
            Some(vec![a.id, b.id, a.id])
        );
        // Fine: diamonds and new tasks that nothing depends on.
        assert_eq!(find_cycle(&graph, c.id, &[a.id, b.id]), None);
        assert_eq!(find_cycle(&graph, d.id, &[a.id, c.id]), None);
        let fresh = TaskId::new();
        assert_eq!(find_cycle(&graph, fresh, &[c.id, d.id]), None);
        // What the graph says about the task that is being changed does not count.
        assert_eq!(find_cycle(&graph, a.id, &[]), None);
        graph.insert(a.id, vec![c.id]);
        assert_eq!(find_cycle(&graph, a.id, &[d.id]), None);
    }

    #[test]
    fn a_long_chain_does_not_overflow_the_stack() {
        let mut graph = HashMap::new();
        let ids: Vec<TaskId> = (0..50_000).map(|_| TaskId::new()).collect();
        for pair in ids.windows(2) {
            graph.insert(pair[1], vec![pair[0]]);
        }
        graph.insert(ids[0], vec![]);
        let last = *ids.last().unwrap();
        let cycle = find_cycle(&graph, ids[0], &[last]).unwrap();
        assert_eq!(cycle.len(), 50_001);
    }

    #[test]
    fn tasks_come_after_what_they_depend_on_and_older_ones_first() {
        let a = task(10, &[]);
        let b = task(20, &[a.id]);
        let c = task(5, &[b.id]);
        let d = task(15, &[]);
        let e = task(30, &[a.id, d.id, TaskId::new()]);
        let order = topological_order([&a, &b, &c, &d, &e]);
        // c is the oldest but needs b, which needs a; d (15) is free before b (20).
        assert_eq!(order, [a.id, d.id, b.id, c.id, e.id]);

        // Ties by age; the input order does not matter.
        let shuffled = topological_order([&e, &d, &c, &b, &a]);
        assert_eq!(shuffled, order);
        assert!(topological_order(std::iter::empty::<&Task>()).is_empty());
    }

    #[test]
    fn tasks_in_a_cycle_are_listed_last_instead_of_lost() {
        let mut a = task(1, &[]);
        let b = task(2, &[a.id]);
        a.depends_on = vec![b.id];
        let free = task(3, &[]);
        let order = topological_order([&a, &b, &free]);
        assert_eq!(order, [free.id, a.id, b.id]);
    }

    #[test]
    fn titles_become_branch_names() {
        assert_eq!(slug("Fix the login bug"), "fix-the-login-bug");
        assert_eq!(slug("  --Add `foo()` & bar!  "), "add-foo-bar");
        assert_eq!(slug("ÄÖ ünï"), "n");
        assert_eq!(slug("???"), "task");
        assert_eq!(slug(""), "task");
        assert_eq!(slug(&"a".repeat(100)).len(), 40);
        assert_eq!(default_branch("Write docs"), "task/write-docs");
    }

    #[test]
    fn ready_tasks_start_in_the_order_they_became_ready_within_the_limit() {
        let ids: Vec<TaskId> = (0..5).map(|_| TaskId::new()).collect();
        let mut scheduler = Scheduler::new();
        for id in &ids {
            scheduler.enqueue(*id);
        }
        scheduler.enqueue(ids[0]);
        assert_eq!(scheduler.waiting().count(), 5, "queued once");

        // Three slots, one taken.
        assert_eq!(scheduler.take(1, 3), [ids[0], ids[1]]);
        // The limit is reached: nothing leaves, nobody loses a place.
        assert!(scheduler.take(3, 3).is_empty());
        assert!(scheduler.take(7, 3).is_empty());
        assert_eq!(scheduler.waiting().collect::<Vec<_>>(), ids[2..]);

        // A task that stops being ready leaves; one that is queued again keeps its place.
        scheduler.remove(ids[3]);
        scheduler.enqueue(ids[2]);
        assert!(scheduler.contains(ids[2]) && !scheduler.contains(ids[3]));
        // A slot freed: the longest waiting goes first.
        assert_eq!(scheduler.take(2, 3), [ids[2]]);
        assert_eq!(scheduler.take(0, 3), [ids[4]]);
        assert!(scheduler.take(0, 3).is_empty());
    }

    #[test]
    fn a_task_round_trips_as_json() {
        let mut t = task(7, &[TaskId::new()]);
        t.run = Some(TaskRun::Terminal {
            argv: vec!["cargo".into(), "test".into()],
        });
        t.placement = TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("task/x".into()),
            base: None,
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["run"]["kind"], "terminal");
        assert_eq!(json["placement"]["kind"], "worktree");
        assert!(json.get("prompt").is_none());
        assert_eq!(serde_json::from_value::<Task>(json).unwrap(), t);
        let chat = serde_json::to_value(TaskRun::Chat {
            agent: "codex".into(),
        })
        .unwrap();
        assert_eq!(chat, serde_json::json!({"kind": "chat", "agent": "codex"}));
    }
}
