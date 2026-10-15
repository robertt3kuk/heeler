//! The observation tree of one session as the registry keeps it (protocol 0.10, capability
//! `agent.tree`): the core reducer plus what the daemon adds around it — coalescing changes into
//! `agent.tree_updated` events (at most one per [`EVENT_INTERVAL`] and session), the rollup of
//! the tree into the session's `agent` summary for clients that predate the tree, and the
//! stored form that lets a restarted daemon say which agents lost contact.

use std::collections::HashMap;
use std::time::Duration;

use hutan_core::{
    AgentNode, Changes, NodeOutcome, NodeState, NodeUpdate, ObservationTree, SessionId,
};
use hutan_protocol::messages::{AgentReport, AgentState, Event};
use hutan_storage::{Storage, StorageError};
use tokio::time::Instant;
use tracing::warn;

/// Shortest time between two `agent.tree_updated` events of one session: at most 4 per second.
pub const EVENT_INTERVAL: Duration = Duration::from_millis(250);

/// Blocking: the stored trees that can be read.
pub fn load(storage: &Storage) -> Result<HashMap<SessionId, ObservationTree>, StorageError> {
    let mut trees = HashMap::new();
    for (id, json) in storage.agent_trees().list()? {
        match serde_json::from_str::<ObservationTree>(&json) {
            Ok(tree) if tree.session_id() == id => {
                trees.insert(id, tree);
            }
            Ok(_) | Err(_) => warn!(session = %id, "ignoring an unreadable stored agent tree"),
        }
    }
    Ok(trees)
}

/// One session's tree, the changes not yet announced, and when the last announcement was.
#[derive(Debug)]
pub struct TreeState {
    tree: ObservationTree,
    /// What happened since the last `agent.tree_updated`.
    pending: Changes,
    last_event: Option<Instant>,
    /// The stored copy is behind.
    dirty: bool,
}

impl TreeState {
    pub fn new(session: SessionId) -> Self {
        Self::from_tree(ObservationTree::new(session))
    }

    pub fn from_tree(tree: ObservationTree) -> Self {
        Self {
            tree,
            pending: Changes::default(),
            last_event: None,
            dirty: false,
        }
    }

    pub fn tree(&self) -> &ObservationTree {
        &self.tree
    }

    /// Notes what an operation on the tree changed, to be announced.
    pub fn record(&mut self, changes: Changes) {
        if changes.is_empty() {
            return;
        }
        self.pending.merge(changes);
        self.dirty = true;
    }

    /// Notes a change nobody needs to be told about (the tree was rebuilt at start).
    pub fn record_silently(&mut self, changes: &Changes) {
        self.dirty |= !changes.is_empty();
    }

    /// The event for what is pending, when one may go out at `now`: nothing pending, or the
    /// last event less than [`EVENT_INTERVAL`] ago, gives `None`.
    pub fn take_event(&mut self, now: Instant) -> Option<Event> {
        if self.pending.is_empty()
            || self
                .last_event
                .is_some_and(|last| last + EVENT_INTERVAL > now)
        {
            return None;
        }
        let pending = std::mem::take(&mut self.pending);
        self.last_event = Some(now);
        let nodes_changed: Vec<AgentNode> = pending
            .changed
            .iter()
            .filter_map(|id| self.tree.node(id).cloned())
            .collect();
        let nodes_removed: Vec<String> = pending.removed.into_iter().collect();
        if nodes_changed.is_empty() && nodes_removed.is_empty() {
            return None;
        }
        Some(Event::AgentTreeUpdated {
            session_id: self.tree.session_id(),
            nodes_changed,
            nodes_removed,
        })
    }

    /// When the pending changes may be announced; `None` when none are pending.
    pub fn event_due(&self) -> Option<Instant> {
        if self.pending.is_empty() {
            return None;
        }
        Some(
            self.last_event
                .map_or_else(Instant::now, |last| last + EVENT_INTERVAL),
        )
    }

    /// Whether the stored form is behind.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether the stored form changed since it was last taken; clears the mark.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// The document to store, or `None` while the tree is empty (the row goes away). What the
    /// agents are doing right now is left out: it is transient, and it can name commands.
    pub fn stored(&self) -> Option<String> {
        if self.tree.is_empty() {
            return None;
        }
        let mut tree = self.tree.clone();
        tree.clear_activity();
        match serde_json::to_string(&tree) {
            Ok(json) => Some(json),
            Err(error) => {
                warn!(session = %tree.session_id(), %error, "could not encode an agent tree");
                None
            }
        }
    }

    // Operations: each records its changes and returns whether anything changed. -----------------

    pub fn accept_seq(&mut self, source: &str, node: Option<&str>, seq: u64) -> bool {
        self.tree.accept_seq(source, node, seq)
    }

    /// Forgets the sequence numbers: a new agent process starts counting again.
    pub fn reset_seqs(&mut self) {
        self.tree.reset_seqs();
    }

    pub fn apply(&mut self, update: NodeUpdate, now_ms: u64) {
        let changes = self.tree.apply(update, now_ms);
        self.record(changes);
    }

    /// `true` when `source` held a node to release.
    pub fn release(
        &mut self,
        source: &str,
        node: Option<&str>,
        outcome: NodeOutcome,
        now_ms: u64,
    ) -> bool {
        match self.tree.release(source, node, outcome, now_ms) {
            Some(changes) => {
                self.record(changes);
                true
            }
            None => false,
        }
    }

    pub fn session_ended(&mut self, now_ms: u64) {
        let changes = self.tree.session_ended(now_ms);
        self.record(changes);
    }

    /// The session came back after a daemon restart; nobody needs telling.
    pub fn restored(&mut self, now_ms: u64) {
        let changes = self.tree.restored(now_ms);
        self.record_silently(&changes);
    }

    /// The session's process is gone and nobody needs telling (loaded at start).
    pub fn ended_silently(&mut self, now_ms: u64) {
        let changes = self.tree.session_ended(now_ms);
        self.record_silently(&changes);
    }

    pub fn prune(&mut self, now_ms: u64) {
        let changes = self.tree.prune(now_ms);
        self.record(changes);
    }

    pub fn prune_silently(&mut self, now_ms: u64) {
        let changes = self.tree.prune(now_ms);
        self.record_silently(&changes);
    }
}

/// The session's `agent` summary, as clients that predate the tree read it: the main agent's own
/// state, `blocked` while any subagent waits for the user too (then `blocked_sub` names the
/// first one). `None` while the session is over, or while no agent is known to run: the main
/// agent finished or lost, and nothing blocked.
pub fn summary(
    live: bool,
    tree: &ObservationTree,
    message: Option<&str>,
    agent_session_id: Option<&str>,
) -> Option<AgentReport> {
    if !live {
        return None;
    }
    let blocked = tree.blocked_sub();
    let own = tree
        .root()
        .filter(|root| !root.is_finished() && root.state != NodeState::Unknown);
    match (own, blocked) {
        (Some(root), _) => {
            let by_sub = (root.state != NodeState::Blocked)
                .then_some(blocked)
                .flatten();
            Some(AgentReport {
                source: root.source.clone(),
                agent: root.agent.clone(),
                state: if by_sub.is_some() {
                    AgentState::Blocked
                } else {
                    agent_state(root.state)
                },
                message: message.map(str::to_owned),
                agent_session_id: agent_session_id.map(str::to_owned),
                updated_at_ms: root.last_activity_ms,
                blocked_sub: by_sub.map(|sub| sub.label().to_owned()),
            })
        }
        (None, Some(sub)) => {
            let owner = tree.root().unwrap_or(sub);
            Some(AgentReport {
                source: owner.source.clone(),
                agent: owner.agent.clone(),
                state: AgentState::Blocked,
                message: None,
                agent_session_id: agent_session_id.map(str::to_owned),
                updated_at_ms: sub.last_activity_ms,
                blocked_sub: Some(sub.label().to_owned()),
            })
        }
        (None, None) => None,
    }
}

/// `a` and `b` say the same to a client (their timestamps aside).
pub fn same_summary(a: Option<&AgentReport>, b: Option<&AgentReport>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            AgentReport {
                updated_at_ms: b.updated_at_ms,
                ..a.clone()
            } == *b
        }
        (None, None) => true,
        _ => false,
    }
}

/// A node state as a report state; `unknown` reads as `idle` to clients that predate the tree.
fn agent_state(state: NodeState) -> AgentState {
    match state {
        NodeState::Working => AgentState::Working,
        NodeState::Blocked => AgentState::Blocked,
        NodeState::Idle | NodeState::Unknown => AgentState::Idle,
    }
}

/// The node state a report state stands for.
pub fn node_state(state: AgentState) -> NodeState {
    match state {
        AgentState::Idle => NodeState::Idle,
        AgentState::Working => NodeState::Working,
        AgentState::Blocked => NodeState::Blocked,
    }
}

#[cfg(test)]
mod tests {
    use hutan_core::{NodeKind, Patch};

    use super::*;

    fn update(id: Option<&str>, kind: NodeKind, state: NodeState) -> NodeUpdate {
        NodeUpdate {
            id: id.map(str::to_owned),
            kind,
            source: "hooks".into(),
            agent: "fake".into(),
            state: Some(state),
            activity: Patch::Keep,
            ..NodeUpdate::default()
        }
    }

    fn state() -> TreeState {
        TreeState::new(SessionId::new())
    }

    #[tokio::test]
    async fn the_first_change_goes_out_at_once_and_the_rest_wait_for_the_interval() {
        let mut state = state();
        state.apply(update(None, NodeKind::Main, NodeState::Working), 1_000);
        let now = Instant::now();
        let Some(Event::AgentTreeUpdated {
            nodes_changed,
            nodes_removed,
            ..
        }) = state.take_event(now)
        else {
            panic!("the first change is announced at once");
        };
        assert_eq!(nodes_changed.len(), 1);
        assert!(nodes_removed.is_empty());
        assert!(state.event_due().is_none(), "nothing is pending now");

        // Changes within the interval are held back and announced together.
        state.apply(update(Some("a"), NodeKind::Sub, NodeState::Working), 1_010);
        state.apply(update(Some("b"), NodeKind::Sub, NodeState::Working), 1_020);
        state.apply(update(Some("a"), NodeKind::Sub, NodeState::Blocked), 1_030);
        assert!(state.take_event(now + Duration::from_millis(100)).is_none());
        assert_eq!(state.event_due(), Some(now + EVENT_INTERVAL));
        let Some(Event::AgentTreeUpdated { nodes_changed, .. }) =
            state.take_event(now + EVENT_INTERVAL)
        else {
            panic!("announced once the interval has passed");
        };
        let ids: Vec<&str> = nodes_changed.iter().map(|n| n.node_id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(
            nodes_changed[0].state,
            NodeState::Blocked,
            "the node as it is now"
        );
    }

    #[tokio::test]
    async fn a_node_that_came_and_went_inside_one_interval_is_only_removed() {
        let mut state = state();
        state.apply(update(None, NodeKind::Main, NodeState::Working), 1_000);
        let now = Instant::now();
        state.take_event(now);
        state.apply(update(Some("a"), NodeKind::Sub, NodeState::Working), 1_010);
        let mut done = update(Some("a"), NodeKind::Sub, NodeState::Idle);
        done.outcome = Some(NodeOutcome::Completed);
        state.apply(done, 1_020);
        let Some(Event::AgentTreeUpdated { nodes_changed, .. }) =
            state.take_event(now + EVENT_INTERVAL)
        else {
            panic!("announced");
        };
        assert_eq!(nodes_changed.len(), 1);
        assert_eq!(nodes_changed[0].outcome, Some(NodeOutcome::Completed));
    }

    #[test]
    fn an_empty_tree_stores_nothing_and_activity_is_not_stored() {
        let mut state = state();
        assert!(state.stored().is_none());
        let mut report = update(None, NodeKind::Main, NodeState::Working);
        report.activity = Patch::Set("Bash: rm -rf secrets".into());
        state.apply(report, 1_000);
        let json = state.stored().unwrap();
        assert!(!json.contains("secrets"), "{json}");
        let back: ObservationTree = serde_json::from_str(&json).unwrap();
        assert_eq!(back.root().unwrap().activity, None);
        assert!(state.take_dirty());
        assert!(!state.take_dirty());
    }

    #[test]
    fn the_summary_rolls_a_blocked_subagent_into_the_main_agents_state() {
        let mut state = state();
        let live = |state: &TreeState| summary(true, state.tree(), Some("m"), Some("sid"));
        assert!(live(&state).is_none());
        state.apply(update(None, NodeKind::Main, NodeState::Working), 1_000);
        let own = live(&state).unwrap();
        assert_eq!(
            (
                own.state,
                own.blocked_sub.as_deref(),
                own.message.as_deref()
            ),
            (AgentState::Working, None, Some("m"))
        );
        let mut sub = update(Some("agent-1"), NodeKind::Sub, NodeState::Blocked);
        sub.name = Some("Explore".into());
        state.apply(sub, 1_010);
        let rolled = live(&state).unwrap();
        assert_eq!(
            (rolled.state, rolled.blocked_sub.as_deref()),
            (AgentState::Blocked, Some("Explore"))
        );
        // The main agent waiting itself is not the subagent's doing.
        state.apply(update(None, NodeKind::Main, NodeState::Blocked), 1_020);
        let own = live(&state).unwrap();
        assert_eq!((own.state, own.blocked_sub), (AgentState::Blocked, None));
        // Nothing to say once the session is over.
        assert!(summary(false, state.tree(), None, None).is_none());
    }

    #[test]
    fn a_blocked_subagent_alone_still_makes_a_summary() {
        let mut state = state();
        let mut sub = update(Some("agent-1"), NodeKind::Sub, NodeState::Blocked);
        sub.agent_type = Some("Explore".into());
        state.apply(sub, 1_000);
        let rolled = summary(true, state.tree(), None, None).unwrap();
        assert_eq!(rolled.state, AgentState::Blocked);
        assert_eq!(rolled.blocked_sub.as_deref(), Some("Explore"));
    }

    #[test]
    fn a_finished_or_lost_main_agent_has_no_summary() {
        let mut state = state();
        state.apply(update(None, NodeKind::Main, NodeState::Working), 1_000);
        state.restored(2_000);
        assert!(
            summary(true, state.tree(), None, None).is_none(),
            "unknown is not idle"
        );
        state.apply(update(None, NodeKind::Main, NodeState::Idle), 3_000);
        assert_eq!(
            summary(true, state.tree(), None, None).unwrap().state,
            AgentState::Idle
        );
        assert!(state.release("hooks", None, NodeOutcome::Completed, 4_000));
        assert!(summary(true, state.tree(), None, None).is_none());
    }
}
