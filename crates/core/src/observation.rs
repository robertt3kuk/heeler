//! The agent observation tree (roadmap M13, `docs/integrations.md` § Observation tree): what
//! the daemon knows about the coding agents running in one session — the main agent and the
//! subagents it spawned — as plain data, plus the reducer that folds reports into it. No I/O
//! and no clock live here: every operation takes `now_ms`.
//!
//! Three things are kept apart, because sources cannot always tell them apart: the runtime
//! **state** (working, blocked, idle), the **outcome** of a finished node (completed, failed,
//! interrupted, disconnected) and **liveness** (is the agent still there). Missing evidence is
//! `unknown` or `disconnected`, never invented, and every number a source did not report stays
//! absent rather than zero.
//!
//! The tree is bounded: at most [`MAX_NODES`] nodes (the oldest finished are dropped first) and
//! finished subtrees disappear [`FINISHED_KEEP_MS`] after they ended.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ids::SessionId;

/// Most nodes one session keeps.
pub const MAX_NODES: usize = 256;
/// Most `(source, node)` sequence numbers remembered per session.
pub const MAX_SEQ_ENTRIES: usize = 1024;
/// How long a finished subtree stays visible.
pub const FINISHED_KEEP_MS: u64 = 30 * 60 * 1000;
/// Longest [`AgentNode::activity`], in characters.
pub const MAX_ACTIVITY_CHARS: usize = 200;
/// A report that changes nothing still moves `last_activity_ms` when the stored value is this
/// old, so a long quiet stretch of identical reports stays visible without an event per report.
pub const ACTIVITY_REFRESH_MS: u64 = 5_000;
/// The node a report without a node id (protocol 0.4 style) is about.
pub const ROOT_NODE_ID: &str = "root";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// The agent the user talks to.
    #[default]
    Main,
    /// An agent a main agent (or another subagent) started.
    Sub,
}

/// What a node is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Working,
    /// Waiting for a decision (permission prompt, question).
    Blocked,
    /// Waiting for work; also the state of a finished node (see [`AgentNode::outcome`]).
    Idle,
    /// No evidence (before the first report, or since a daemon restart).
    Unknown,
}

/// How a finished node ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeOutcome {
    Completed,
    Failed,
    Interrupted,
    /// Contact was lost: the agent's process or session went away without saying how it ended.
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Alive,
    Gone,
    Unknown,
}

/// Token and cost counters as one source reports them. What a counter means differs per
/// source (OMP's `tokens` exclude cache reads, Claude's context figures are the latest context
/// and not a lifetime total, Codex has `total` and `last`), so `kind` names the semantics and
/// clients label by it. A counter the source did not send is absent, never zero.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Source-specific, e.g. `omp.tokens`, `claude.context`, `codex.total`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// One agent of the tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentNode {
    /// The source's own id for the agent, unique within the session.
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub kind: NodeKind,
    /// Who reports it (`omp-extension`, `claude-hooks`, `codex`).
    pub source: String,
    /// The agent program (`omp`, `claude`, `codex`).
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The subagent's definition or role (`Explore`, `task`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    pub state: NodeState,
    /// Set once the node has finished; `state` is then `idle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<NodeOutcome>,
    pub liveness: Liveness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// When the node last changed (or, at most every [`ACTIVITY_REFRESH_MS`], was reported
    /// unchanged).
    pub last_activity_ms: u64,
    /// What it is doing, e.g. the current tool; at most [`MAX_ACTIVITY_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// The agent's own transcript (a finished Claude subagent's `agent_transcript_path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
}

impl AgentNode {
    pub fn is_finished(&self) -> bool {
        self.outcome.is_some()
    }

    /// What clients show for the node: its name, else its type, else its id.
    pub fn label(&self) -> &str {
        self.name
            .as_deref()
            .or(self.agent_type.as_deref())
            .unwrap_or(&self.node_id)
    }
}

/// The tree of one session as clients see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentObservation {
    pub session_id: SessionId,
    /// The id of the main agent's node.
    pub root: String,
    pub nodes: BTreeMap<String, AgentNode>,
}

/// How an update treats a text field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Patch<T> {
    /// Leave the stored value.
    #[default]
    Keep,
    Set(T),
    Clear,
}

/// One report about one node, as the reducer takes it. Fields that are `None` or
/// [`Patch::Keep`] leave what is stored; a new node starts without them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeUpdate {
    /// `None`: the main agent of the session (a report without a node, protocol 0.4 style).
    pub id: Option<String>,
    /// Only read when the node is created; an unknown parent means the root.
    pub parent: Option<String>,
    pub kind: NodeKind,
    pub source: String,
    pub agent: String,
    pub name: Option<String>,
    pub agent_type: Option<String>,
    pub state: Option<NodeState>,
    /// Finishes the node (`state` becomes `idle`, liveness `gone`).
    pub outcome: Option<NodeOutcome>,
    pub model: Option<String>,
    pub activity: Patch<String>,
    pub usage: Option<Usage>,
    pub transcript_path: Option<String>,
    /// Only read when the node is created; default: now.
    pub started_at_ms: Option<u64>,
}

/// Which nodes an operation changed or removed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    pub changed: BTreeSet<String>,
    pub removed: BTreeSet<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }

    fn changed(&mut self, id: &str) {
        self.removed.remove(id);
        self.changed.insert(id.to_owned());
    }

    fn removed(&mut self, id: &str) {
        self.changed.remove(id);
        self.removed.insert(id.to_owned());
    }

    /// Folds `later` (which happened after `self`) into `self`.
    pub fn merge(&mut self, later: Changes) {
        for id in &later.changed {
            self.changed(id);
        }
        for id in &later.removed {
            self.removed(id);
        }
    }
}

/// The reducer: one session's nodes and the rules that fold reports into them.
///
/// Rules, in short:
/// - The **root** is the main agent. A report without a node id is about it; a node with kind
///   `main` and no parent is the root. The last source to report a root wins: a root of another
///   id (or a node-less report from another source) replaces the whole tree.
/// - A subagent that reports before any root exists gets a placeholder root (`unknown`) that
///   the real root takes over together with its children.
/// - A report with an `outcome` finishes its node; `working` and `blocked` revive a finished
///   node; `idle` never revives one.
/// - A node's parent is fixed when it is created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationTree {
    session_id: SessionId,
    root: Option<String>,
    nodes: BTreeMap<String, AgentNode>,
    /// The root was made up for subagents that reported first.
    #[serde(default)]
    placeholder_root: bool,
    /// Highest `seq` accepted per `(source, node)` (see [`accept_seq`](Self::accept_seq)). Not
    /// persisted: sources start a new sequence after a daemon restart.
    #[serde(skip)]
    seqs: BTreeMap<String, u64>,
}

impl ObservationTree {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            root: None,
            nodes: BTreeMap::new(),
            placeholder_root: false,
            seqs: BTreeMap::new(),
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Sequencing of reports: `seq` must increase per `(session, source, node)`, where `node`
    /// is the node the report or release names (none for the main agent of a report without a
    /// node). Returns whether `seq` is news; if so it is recorded. Reports are separate
    /// processes that can overtake each other, so the daemon orders by `seq`, not by arrival.
    pub fn accept_seq(&mut self, source: &str, node: Option<&str>, seq: u64) -> bool {
        let key = format!("{source}\u{1f}{}", node.unwrap_or(""));
        if let Some(last) = self.seqs.get_mut(&key) {
            if seq <= *last {
                return false;
            }
            *last = seq;
            return true;
        }
        if self.seqs.len() >= MAX_SEQ_ENTRIES {
            // Forget the key with the oldest news.
            let oldest = self
                .seqs
                .iter()
                .min_by_key(|(_, seq)| **seq)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.seqs.remove(&oldest);
            }
        }
        self.seqs.insert(key, seq);
        true
    }

    /// Forgets every sequence number: a new agent process starts counting again.
    pub fn reset_seqs(&mut self) {
        self.seqs.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn node(&self, id: &str) -> Option<&AgentNode> {
        self.nodes.get(id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &AgentNode> {
        self.nodes.values()
    }

    pub fn root(&self) -> Option<&AgentNode> {
        self.nodes.get(self.root.as_deref()?)
    }

    pub fn root_id(&self) -> Option<&str> {
        self.root.as_deref()
    }

    /// Forgets what every node is doing right now (activity is transient: it is not stored).
    pub fn clear_activity(&mut self) {
        for node in self.nodes.values_mut() {
            node.activity = None;
        }
    }

    /// Drops every node.
    pub fn clear(&mut self) -> Changes {
        let mut changes = Changes::default();
        for id in std::mem::take(&mut self.nodes).into_keys() {
            changes.removed(&id);
        }
        self.root = None;
        self.placeholder_root = false;
        changes
    }

    /// The tree as clients see it; `None` while it is empty.
    pub fn observation(&self) -> Option<AgentObservation> {
        Some(AgentObservation {
            session_id: self.session_id,
            root: self.root.clone()?,
            nodes: self.nodes.clone(),
        })
    }

    /// The subagent that needs the user, if any: blocked and not gone, the one that has been
    /// around longest.
    pub fn blocked_sub(&self) -> Option<&AgentNode> {
        let root = self.root.as_deref();
        self.nodes
            .values()
            .filter(|n| {
                Some(n.node_id.as_str()) != root
                    && n.state == NodeState::Blocked
                    && n.liveness != Liveness::Gone
            })
            .min_by(|a, b| (a.started_at_ms, &a.node_id).cmp(&(b.started_at_ms, &b.node_id)))
    }

    /// Whether any node, the root or a subagent, waits for the user.
    pub fn needs_attention(&self) -> bool {
        self.nodes
            .values()
            .any(|n| n.state == NodeState::Blocked && n.liveness != Liveness::Gone)
    }

    // Reports ----------------------------------------------------------------------------------

    /// Folds one report in; returns what changed.
    pub fn apply(&mut self, update: NodeUpdate, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        let id = self.resolve(&update, &mut changes, now_ms);
        match self.nodes.get(&id).cloned() {
            Some(old) => {
                let new = merged(old.clone(), &update, now_ms);
                if new != old {
                    self.nodes.insert(id.clone(), new);
                    changes.changed(&id);
                }
            }
            None => {
                self.make_room(&mut changes);
                let parent = self.parent_for(&id, update.parent.as_deref());
                let node = created(id.clone(), parent, &update, now_ms);
                self.nodes.insert(id.clone(), node);
                changes.changed(&id);
            }
        }
        changes.merge(self.prune(now_ms));
        changes
    }

    /// Picks (and prepares) the node a report is about.
    fn resolve(&mut self, update: &NodeUpdate, changes: &mut Changes, now_ms: u64) -> String {
        let main_root = update.kind == NodeKind::Main && update.parent.is_none();
        let about_root = match &update.id {
            None => true,
            Some(id) => main_root || self.root.as_deref() == Some(id.as_str()),
        };
        if !about_root {
            if self.root.is_none() {
                // A subagent before its main agent: it hangs off a placeholder.
                self.insert_placeholder(update, changes, now_ms);
            }
            return update.id.clone().unwrap_or_default();
        }
        let wanted = match (&update.id, &self.root) {
            (Some(id), _) => id.clone(),
            // No node id: the main agent that is there, while it is the same source's.
            (None, Some(root))
                if self.placeholder_root
                    || self
                        .nodes
                        .get(root)
                        .is_some_and(|r| r.source == update.source) =>
            {
                root.clone()
            }
            (None, _) => ROOT_NODE_ID.to_owned(),
        };
        self.take_root(&wanted, &update.source, changes);
        wanted
    }

    /// Makes `id` the root. The tree of another main agent (another id, or another source) is
    /// dropped; a placeholder root hands its children over instead.
    fn take_root(&mut self, id: &str, source: &str, changes: &mut Changes) {
        match self.root.clone() {
            None => {}
            Some(old) if self.placeholder_root => {
                if old != id {
                    self.nodes.remove(&old);
                    changes.removed(&old);
                    for node in self.nodes.values_mut() {
                        if node.parent.as_deref() == Some(old.as_str()) {
                            node.parent = Some(id.to_owned());
                            changes.changed(&node.node_id);
                        }
                    }
                }
            }
            Some(old) if old == id && self.nodes.get(&old).is_some_and(|r| r.source == source) => {}
            Some(_) => {
                for dropped in std::mem::take(&mut self.nodes).into_keys() {
                    changes.removed(&dropped);
                }
            }
        }
        self.root = Some(id.to_owned());
        self.placeholder_root = false;
    }

    fn insert_placeholder(&mut self, update: &NodeUpdate, changes: &mut Changes, now_ms: u64) {
        let node = AgentNode {
            node_id: ROOT_NODE_ID.to_owned(),
            parent: None,
            kind: NodeKind::Main,
            source: update.source.clone(),
            agent: update.agent.clone(),
            name: None,
            agent_type: None,
            state: NodeState::Unknown,
            outcome: None,
            liveness: Liveness::Unknown,
            model: None,
            started_at_ms: None,
            ended_at_ms: None,
            last_activity_ms: now_ms,
            activity: None,
            usage: None,
            transcript_path: None,
        };
        self.root = Some(ROOT_NODE_ID.to_owned());
        self.placeholder_root = true;
        self.nodes.insert(ROOT_NODE_ID.to_owned(), node);
        changes.changed(ROOT_NODE_ID);
    }

    /// The parent a new node gets: the one it names when that is known, else the root; the
    /// root itself has none.
    fn parent_for(&self, id: &str, wanted: Option<&str>) -> Option<String> {
        let root = self.root.as_deref()?;
        if id == root {
            return None;
        }
        Some(
            wanted
                .filter(|parent| *parent != id && self.nodes.contains_key(*parent))
                .unwrap_or(root)
                .to_owned(),
        )
    }

    /// Frees one slot when the tree is full: the oldest finished leaf, else the least recently
    /// active leaf. The root is never dropped.
    fn make_room(&mut self, changes: &mut Changes) {
        while self.nodes.len() >= MAX_NODES {
            let parents: BTreeSet<&str> = self
                .nodes
                .values()
                .filter_map(|n| n.parent.as_deref())
                .collect();
            let root = self.root.as_deref();
            let victim = self
                .nodes
                .values()
                .filter(|n| {
                    Some(n.node_id.as_str()) != root && !parents.contains(n.node_id.as_str())
                })
                .min_by_key(|n| {
                    (
                        !n.is_finished(),
                        n.ended_at_ms.unwrap_or(n.last_activity_ms),
                        n.last_activity_ms,
                    )
                })
                .map(|n| n.node_id.clone());
            let Some(victim) = victim else { return };
            self.nodes.remove(&victim);
            changes.removed(&victim);
        }
    }

    // Ending -----------------------------------------------------------------------------------

    /// The agent behind `source` is gone. `node: None` releases the root (and with it every
    /// subagent still running); `Some(id)` ends that node. `None` when `source` holds no such
    /// unfinished node: nothing was released.
    pub fn release(
        &mut self,
        source: &str,
        node: Option<&str>,
        outcome: NodeOutcome,
        now_ms: u64,
    ) -> Option<Changes> {
        let root = self.root.clone()?;
        let id = node.unwrap_or(&root);
        let target = self.nodes.get(id)?;
        // A node that is over holds nothing to release.
        if target.source != source || target.is_finished() {
            return None;
        }
        let mut changes = Changes::default();
        self.finish(id, outcome, Liveness::Gone, now_ms, &mut changes);
        if id == root {
            // The main agent exited: nothing below it is running any more.
            self.finish_all(
                NodeOutcome::Disconnected,
                Liveness::Gone,
                now_ms,
                &mut changes,
            );
        }
        changes.merge(self.prune(now_ms));
        Some(changes)
    }

    /// The session's process ended: every node still unfinished has lost its agent.
    pub fn session_ended(&mut self, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        self.finish_all(
            NodeOutcome::Disconnected,
            Liveness::Gone,
            now_ms,
            &mut changes,
        );
        if let Some(root) = self.root.clone() {
            self.finish(
                &root,
                NodeOutcome::Disconnected,
                Liveness::Gone,
                now_ms,
                &mut changes,
            );
        }
        changes
    }

    /// The daemon restarted and brought the session back. The main agent may or may not be
    /// running again, so it is `unknown` until it reports; its subagents died with the old
    /// process, and contact with them is lost (`disconnected`, liveness `unknown`).
    pub fn restored(&mut self, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        self.finish_all(
            NodeOutcome::Disconnected,
            Liveness::Unknown,
            now_ms,
            &mut changes,
        );
        if let Some(id) = self.root.clone()
            && let Some(node) = self.nodes.get_mut(&id)
            && !node.is_finished()
        {
            node.state = NodeState::Unknown;
            node.liveness = Liveness::Unknown;
            node.activity = None;
            node.last_activity_ms = now_ms;
            changes.changed(&id);
        }
        changes
    }

    /// Finishes every unfinished node except the root.
    fn finish_all(
        &mut self,
        outcome: NodeOutcome,
        liveness: Liveness,
        now_ms: u64,
        changes: &mut Changes,
    ) {
        // The root is the caller's business.
        let root = self.root.as_deref();
        let open: Vec<String> = self
            .nodes
            .values()
            .filter(|n| !n.is_finished() && Some(n.node_id.as_str()) != root)
            .map(|n| n.node_id.clone())
            .collect();
        for id in open {
            self.finish(&id, outcome, liveness, now_ms, changes);
        }
    }

    fn finish(
        &mut self,
        id: &str,
        outcome: NodeOutcome,
        liveness: Liveness,
        now_ms: u64,
        changes: &mut Changes,
    ) {
        let Some(node) = self.nodes.get_mut(id) else {
            return;
        };
        if node.is_finished() {
            return;
        }
        finish_node(node, outcome, liveness, now_ms);
        changes.changed(id);
    }

    // Pruning ----------------------------------------------------------------------------------

    /// Drops finished subtrees that ended more than [`FINISHED_KEEP_MS`] ago.
    pub fn prune(&mut self, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        let cutoff = now_ms.saturating_sub(FINISHED_KEEP_MS);
        loop {
            let parents: BTreeSet<&str> = self
                .nodes
                .values()
                .filter_map(|n| n.parent.as_deref())
                .collect();
            let stale: Vec<String> = self
                .nodes
                .values()
                .filter(|n| {
                    !parents.contains(n.node_id.as_str())
                        && n.is_finished()
                        && n.ended_at_ms.is_some_and(|ended| ended <= cutoff)
                })
                .map(|n| n.node_id.clone())
                .collect();
            if stale.is_empty() {
                break;
            }
            for id in stale {
                self.nodes.remove(&id);
                changes.removed(&id);
            }
        }
        if self
            .root
            .as_deref()
            .is_some_and(|root| !self.nodes.contains_key(root))
        {
            self.root = None;
            self.placeholder_root = false;
        }
        changes
    }
}

fn truncated(text: &str) -> String {
    text.chars().take(MAX_ACTIVITY_CHARS).collect()
}

fn finish_node(node: &mut AgentNode, outcome: NodeOutcome, liveness: Liveness, now_ms: u64) {
    node.state = NodeState::Idle;
    node.outcome = Some(outcome);
    node.liveness = liveness;
    node.activity = None;
    node.ended_at_ms.get_or_insert(now_ms);
}

/// A node as its first report describes it.
fn created(id: String, parent: Option<String>, update: &NodeUpdate, now_ms: u64) -> AgentNode {
    let mut node = AgentNode {
        node_id: id,
        parent,
        kind: update.kind,
        source: update.source.clone(),
        agent: update.agent.clone(),
        name: update.name.clone(),
        agent_type: update.agent_type.clone(),
        state: NodeState::Unknown,
        outcome: None,
        liveness: Liveness::Unknown,
        model: update.model.clone(),
        started_at_ms: Some(update.started_at_ms.unwrap_or(now_ms)),
        ended_at_ms: None,
        last_activity_ms: now_ms,
        activity: None,
        usage: update.usage.clone(),
        transcript_path: update.transcript_path.clone(),
    };
    apply_state(&mut node, update, now_ms);
    node
}

/// `old` after `update`. `last_activity_ms` moves only with a real change, or when the stored
/// value has gone stale ([`ACTIVITY_REFRESH_MS`]).
fn merged(old: AgentNode, update: &NodeUpdate, now_ms: u64) -> AgentNode {
    let mut node = old.clone();
    node.source.clone_from(&update.source);
    node.agent.clone_from(&update.agent);
    node.started_at_ms
        .get_or_insert(update.started_at_ms.unwrap_or(now_ms));
    if update.id.is_some() {
        node.kind = update.kind;
    }
    if let Some(name) = &update.name {
        node.name = Some(name.clone());
    }
    if let Some(agent_type) = &update.agent_type {
        node.agent_type = Some(agent_type.clone());
    }
    if let Some(model) = &update.model {
        node.model = Some(model.clone());
    }
    if let Some(usage) = &update.usage {
        node.usage = Some(usage.clone());
    }
    if let Some(path) = &update.transcript_path {
        node.transcript_path = Some(path.clone());
    }
    apply_state(&mut node, update, now_ms);
    if node != old || now_ms.saturating_sub(old.last_activity_ms) >= ACTIVITY_REFRESH_MS {
        node.last_activity_ms = now_ms;
    }
    node
}

/// State, outcome, liveness and activity of `node` after `update`. A finished node keeps the
/// outcome it ended with; only work (`working`, `blocked`) starts it again.
fn apply_state(node: &mut AgentNode, update: &NodeUpdate, now_ms: u64) {
    match &update.activity {
        Patch::Keep => {}
        Patch::Set(text) => node.activity = Some(truncated(text)).filter(|t| !t.is_empty()),
        Patch::Clear => node.activity = None,
    }
    if let Some(outcome) = update.outcome {
        if !node.is_finished() {
            finish_node(node, outcome, Liveness::Gone, now_ms);
        }
    } else {
        match update.state {
            Some(state @ (NodeState::Working | NodeState::Blocked)) => {
                if node.is_finished() {
                    // Another run of the agent: a new start.
                    node.started_at_ms = Some(now_ms);
                }
                node.state = state;
                node.outcome = None;
                node.ended_at_ms = None;
                node.liveness = Liveness::Alive;
            }
            Some(NodeState::Idle) if !node.is_finished() => {
                node.state = NodeState::Idle;
                node.liveness = Liveness::Alive;
            }
            Some(NodeState::Unknown) if !node.is_finished() => {
                node.state = NodeState::Unknown;
                node.liveness = Liveness::Unknown;
            }
            _ => {}
        }
    }
    // A finished node shows nothing it is doing.
    if node.is_finished() {
        node.activity = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;

    fn tree() -> ObservationTree {
        ObservationTree::new(SessionId::new())
    }

    fn update(id: Option<&str>, kind: NodeKind, source: &str, state: NodeState) -> NodeUpdate {
        NodeUpdate {
            id: id.map(str::to_owned),
            kind,
            source: source.to_owned(),
            agent: "fake".to_owned(),
            state: Some(state),
            ..NodeUpdate::default()
        }
    }

    fn main_report(state: NodeState) -> NodeUpdate {
        update(None, NodeKind::Main, "hooks", state)
    }

    fn sub(id: &str, parent: Option<&str>, state: NodeState) -> NodeUpdate {
        NodeUpdate {
            parent: parent.map(str::to_owned),
            name: Some(format!("name-{id}")),
            ..update(Some(id), NodeKind::Sub, "hooks", state)
        }
    }

    fn ids(set: &BTreeSet<String>) -> Vec<&str> {
        set.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_report_without_a_node_is_the_root() {
        let mut tree = tree();
        let changes = tree.apply(main_report(NodeState::Working), T0);
        assert_eq!(ids(&changes.changed), ["root"]);
        let root = tree.root().unwrap();
        assert_eq!(root.node_id, ROOT_NODE_ID);
        assert_eq!(root.parent, None);
        assert_eq!(root.kind, NodeKind::Main);
        assert_eq!(
            (root.state, root.liveness, root.outcome),
            (NodeState::Working, Liveness::Alive, None)
        );
        assert_eq!(root.started_at_ms, Some(T0));
        // Counters nobody reported are absent, not zero.
        assert_eq!(root.usage, None);
        assert_eq!(tree.observation().unwrap().root, "root");
    }

    #[test]
    fn identical_reports_change_nothing_until_the_stored_activity_is_stale() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        assert!(
            tree.apply(main_report(NodeState::Working), T0 + 100)
                .is_empty()
        );
        assert_eq!(tree.root().unwrap().last_activity_ms, T0);
        let changes = tree.apply(main_report(NodeState::Working), T0 + ACTIVITY_REFRESH_MS);
        assert_eq!(ids(&changes.changed), ["root"]);
        assert_eq!(
            tree.root().unwrap().last_activity_ms,
            T0 + ACTIVITY_REFRESH_MS
        );
    }

    #[test]
    fn subagents_hang_off_the_root_or_their_named_parent() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("a", None, NodeState::Working), T0 + 1);
        tree.apply(sub("b", Some("a"), NodeState::Working), T0 + 2);
        tree.apply(sub("c", Some("nobody"), NodeState::Working), T0 + 3);
        tree.apply(sub("d", Some("d"), NodeState::Working), T0 + 4);
        let parent = |tree: &ObservationTree, id: &str| tree.node(id).unwrap().parent.clone();
        assert_eq!(parent(&tree, "a").as_deref(), Some("root"));
        assert_eq!(parent(&tree, "b").as_deref(), Some("a"));
        assert_eq!(
            parent(&tree, "c").as_deref(),
            Some("root"),
            "an unknown parent means the root"
        );
        assert_eq!(
            parent(&tree, "d").as_deref(),
            Some("root"),
            "never its own parent"
        );
        // The parent is fixed at creation.
        tree.apply(sub("b", Some("root"), NodeState::Idle), T0 + 5);
        assert_eq!(parent(&tree, "b").as_deref(), Some("a"));
    }

    #[test]
    fn sticky_fields_survive_later_reports_but_activity_is_replaced() {
        let mut tree = tree();
        let mut first = sub("a", None, NodeState::Working);
        first.model = Some("sonnet".into());
        first.agent_type = Some("Explore".into());
        first.activity = Patch::Set("Bash: ls".into());
        first.usage = Some(Usage {
            kind: "claude.context".into(),
            context_tokens: Some(42),
            ..Usage::default()
        });
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(first, T0);
        let mut second = sub("a", None, NodeState::Working);
        second.name = None;
        second.activity = Patch::Clear;
        tree.apply(second, T0 + 10);
        let node = tree.node("a").unwrap();
        assert_eq!(node.model.as_deref(), Some("sonnet"));
        assert_eq!(node.agent_type.as_deref(), Some("Explore"));
        assert_eq!(node.name.as_deref(), Some("name-a"));
        assert_eq!(node.activity, None);
        assert_eq!(node.usage.as_ref().unwrap().context_tokens, Some(42));
        assert_eq!(node.usage.as_ref().unwrap().input, None);
    }

    #[test]
    fn activity_is_cut_to_200_characters() {
        let mut tree = tree();
        let mut report = main_report(NodeState::Working);
        report.activity = Patch::Set("é".repeat(500));
        tree.apply(report, T0);
        assert_eq!(
            tree.root()
                .unwrap()
                .activity
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            200
        );
    }

    #[test]
    fn an_outcome_finishes_a_node_and_state_idle_does_not_revive_it() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("a", None, NodeState::Working), T0 + 1);
        let mut done = sub("a", None, NodeState::Working);
        done.outcome = Some(NodeOutcome::Completed);
        done.activity = Patch::Set("ignored".into());
        tree.apply(done, T0 + 2);
        let node = tree.node("a").unwrap();
        assert_eq!(
            (node.state, node.outcome, node.liveness, node.ended_at_ms),
            (
                NodeState::Idle,
                Some(NodeOutcome::Completed),
                Liveness::Gone,
                Some(T0 + 2)
            )
        );
        assert_eq!(node.activity, None);

        tree.apply(sub("a", None, NodeState::Idle), T0 + 3);
        assert_eq!(
            tree.node("a").unwrap().outcome,
            Some(NodeOutcome::Completed)
        );
        assert_eq!(tree.node("a").unwrap().liveness, Liveness::Gone);

        // Work resumes: the agent was sent another message.
        tree.apply(sub("a", None, NodeState::Working), T0 + 4);
        let node = tree.node("a").unwrap();
        assert_eq!(
            (node.state, node.outcome, node.liveness, node.ended_at_ms),
            (NodeState::Working, None, Liveness::Alive, None)
        );
    }

    #[test]
    fn a_blocked_subagent_needs_attention_until_it_is_gone() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        assert!(!tree.needs_attention());
        tree.apply(sub("a", None, NodeState::Blocked), T0 + 1);
        tree.apply(sub("b", None, NodeState::Blocked), T0 + 2);
        assert!(tree.needs_attention());
        assert_eq!(
            tree.blocked_sub().unwrap().node_id,
            "a",
            "the oldest wait comes first"
        );
        let mut done = sub("a", None, NodeState::Idle);
        done.outcome = Some(NodeOutcome::Interrupted);
        tree.apply(done, T0 + 3);
        assert_eq!(tree.blocked_sub().unwrap().node_id, "b");
        tree.apply(sub("b", None, NodeState::Working), T0 + 4);
        assert!(!tree.needs_attention());
        // The root counts for attention but is not a subagent.
        tree.apply(main_report(NodeState::Blocked), T0 + 5);
        assert!(tree.needs_attention());
        assert!(tree.blocked_sub().is_none());
    }

    #[test]
    fn a_subagent_before_its_main_agent_gets_a_placeholder_root_the_main_agent_takes_over() {
        let mut tree = tree();
        tree.apply(sub("a", None, NodeState::Working), T0);
        let root = tree.root().unwrap();
        assert_eq!(
            (root.node_id.as_str(), root.state, root.liveness),
            ("root", NodeState::Unknown, Liveness::Unknown)
        );
        assert_eq!(tree.node("a").unwrap().parent.as_deref(), Some("root"));

        // An OMP-style main agent with its own id.
        let mut main = update(Some("Main"), NodeKind::Main, "hooks", NodeState::Working);
        main.name = Some("main".into());
        let changes = tree.apply(main, T0 + 1);
        assert_eq!(tree.observation().unwrap().root, "Main");
        assert_eq!(tree.node("a").unwrap().parent.as_deref(), Some("Main"));
        assert!(tree.node("root").is_none());
        assert_eq!(ids(&changes.removed), ["root"]);
        assert_eq!(ids(&changes.changed), ["Main", "a"]);
    }

    #[test]
    fn a_node_less_report_after_a_placeholder_fills_it_in_place() {
        let mut tree = tree();
        tree.apply(sub("a", None, NodeState::Working), T0);
        tree.apply(main_report(NodeState::Working), T0 + 1);
        assert_eq!(tree.root().unwrap().state, NodeState::Working);
        assert_eq!(tree.node("a").unwrap().parent.as_deref(), Some("root"));
        assert_eq!(tree.len(), 2);
    }

    #[test]
    fn another_sources_root_replaces_the_tree() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("a", None, NodeState::Working), T0 + 1);
        let other = update(None, NodeKind::Main, "other", NodeState::Idle);
        let changes = tree.apply(other, T0 + 2);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.root().unwrap().source, "other");
        assert_eq!(ids(&changes.removed), ["a"]);
        assert_eq!(ids(&changes.changed), ["root"]);

        // An explicit main node of another id replaces it too.
        let omp = update(Some("Main"), NodeKind::Main, "omp", NodeState::Working);
        let changes = tree.apply(omp, T0 + 3);
        assert_eq!(ids(&changes.removed), ["root"]);
        assert_eq!(tree.observation().unwrap().root, "Main");
    }

    #[test]
    fn releasing_the_root_ends_everything_below_it() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("a", None, NodeState::Working), T0 + 1);
        let mut finished = sub("b", None, NodeState::Working);
        finished.outcome = Some(NodeOutcome::Failed);
        tree.apply(finished, T0 + 2);
        assert!(
            tree.release("elsewhere", None, NodeOutcome::Completed, T0 + 3)
                .is_none()
        );
        let changes = tree
            .release("hooks", None, NodeOutcome::Completed, T0 + 4)
            .unwrap();
        assert_eq!(ids(&changes.changed), ["a", "root"]);
        assert_eq!(tree.root().unwrap().outcome, Some(NodeOutcome::Completed));
        assert_eq!(
            tree.node("a").unwrap().outcome,
            Some(NodeOutcome::Disconnected)
        );
        assert_eq!(
            tree.node("b").unwrap().outcome,
            Some(NodeOutcome::Failed),
            "kept as it ended"
        );
    }

    #[test]
    fn releasing_a_node_ends_only_that_node_and_only_for_its_source() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("a", None, NodeState::Working), T0 + 1);
        tree.apply(sub("b", Some("a"), NodeState::Working), T0 + 1);
        assert!(
            tree.release("elsewhere", Some("a"), NodeOutcome::Completed, T0 + 2)
                .is_none()
        );
        assert!(
            tree.release("hooks", Some("ghost"), NodeOutcome::Completed, T0 + 2)
                .is_none()
        );
        let changes = tree
            .release("hooks", Some("a"), NodeOutcome::Interrupted, T0 + 3)
            .unwrap();
        assert_eq!(ids(&changes.changed), ["a"]);
        assert_eq!(
            tree.node("a").unwrap().outcome,
            Some(NodeOutcome::Interrupted)
        );
        assert_eq!(tree.node("b").unwrap().outcome, None);
        assert_eq!(tree.root().unwrap().outcome, None);
    }

    #[test]
    fn finished_subtrees_are_pruned_after_thirty_minutes_children_first() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("a", None, NodeState::Working), T0);
        tree.apply(sub("b", Some("a"), NodeState::Working), T0);
        let mut end_b = sub("b", Some("a"), NodeState::Idle);
        end_b.outcome = Some(NodeOutcome::Completed);
        tree.apply(end_b, T0 + 1_000);
        let mut end_a = sub("a", None, NodeState::Idle);
        end_a.outcome = Some(NodeOutcome::Completed);
        tree.apply(end_a, T0 + 2_000);

        assert!(tree.prune(T0 + 1_000 + FINISHED_KEEP_MS - 1).is_empty());
        let changes = tree.prune(T0 + 1_000 + FINISHED_KEEP_MS);
        assert_eq!(ids(&changes.removed), ["b"], "a ended a second later");
        let changes = tree.prune(T0 + 2_000 + FINISHED_KEEP_MS);
        assert_eq!(ids(&changes.removed), ["a"]);
        let remaining: Vec<&str> = tree.nodes().map(|n| n.node_id.as_str()).collect();
        assert_eq!(remaining, ["root"]);
        // Unfinished nodes, however old, stay.
        assert!(tree.prune(T0 + 100 * FINISHED_KEEP_MS).is_empty());
    }

    #[test]
    fn a_released_root_goes_away_with_its_tree_and_the_tree_starts_over() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.release("hooks", None, NodeOutcome::Completed, T0 + 1)
            .unwrap();
        let changes = tree.prune(T0 + 1 + FINISHED_KEEP_MS);
        assert_eq!(ids(&changes.removed), ["root"]);
        assert!(tree.is_empty());
        assert!(tree.observation().is_none());
        tree.apply(main_report(NodeState::Working), T0 + 2 * FINISHED_KEEP_MS);
        assert_eq!(tree.root().unwrap().state, NodeState::Working);
    }

    #[test]
    fn the_tree_never_holds_more_than_256_nodes_and_drops_finished_ones_first() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        for n in 0..(MAX_NODES - 1) {
            tree.apply(
                sub(&format!("n{n:03}"), None, NodeState::Working),
                T0 + n as u64,
            );
        }
        assert_eq!(tree.len(), MAX_NODES);
        // Finish a node in the middle: it goes first, not the oldest running one.
        let mut end = sub("n100", None, NodeState::Idle);
        end.outcome = Some(NodeOutcome::Completed);
        tree.apply(end, T0 + 1_000);
        let changes = tree.apply(sub("fresh", None, NodeState::Working), T0 + 2_000);
        assert_eq!(tree.len(), MAX_NODES);
        assert_eq!(ids(&changes.removed), ["n100"]);
        assert!(tree.node("n000").is_some());

        // With nothing finished the least recently active leaf goes; the root stays.
        let changes = tree.apply(sub("fresher", None, NodeState::Working), T0 + 3_000);
        assert_eq!(ids(&changes.removed), ["n000"]);
        assert!(tree.root().is_some());
        assert_eq!(tree.len(), MAX_NODES);
    }

    #[test]
    fn a_node_with_children_is_not_dropped_for_room() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("parent", None, NodeState::Working), T0);
        for n in 0..(MAX_NODES - 2) {
            tree.apply(
                sub(&format!("c{n:03}"), Some("parent"), NodeState::Working),
                T0 + 1 + n as u64,
            );
        }
        tree.apply(sub("extra", Some("parent"), NodeState::Working), T0 + 1_000);
        assert!(tree.node("parent").is_some());
        assert_eq!(tree.len(), MAX_NODES);
        assert!(tree.node("c000").is_none(), "the oldest leaf made room");
    }

    #[test]
    fn restart_leaves_the_main_agent_unknown_and_disconnects_subagents() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Blocked), T0);
        tree.apply(sub("a", None, NodeState::Working), T0 + 1);
        let mut done = sub("b", None, NodeState::Idle);
        done.outcome = Some(NodeOutcome::Completed);
        tree.apply(done, T0 + 2);
        let changes = tree.restored(T0 + 10);
        assert_eq!(ids(&changes.changed), ["a", "root"]);
        let root = tree.root().unwrap();
        assert_eq!(
            (root.state, root.liveness, root.outcome),
            (NodeState::Unknown, Liveness::Unknown, None)
        );
        assert_eq!(
            tree.node("a").unwrap().outcome,
            Some(NodeOutcome::Disconnected)
        );
        assert_eq!(
            tree.node("b").unwrap().outcome,
            Some(NodeOutcome::Completed)
        );
        assert!(!tree.needs_attention(), "unknown is not blocked");

        // Until it reports again.
        tree.apply(main_report(NodeState::Working), T0 + 20);
        let root = tree.root().unwrap();
        assert_eq!(
            (root.state, root.liveness),
            (NodeState::Working, Liveness::Alive)
        );
    }

    #[test]
    fn a_session_that_ended_disconnects_every_open_node() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("a", None, NodeState::Blocked), T0 + 1);
        let changes = tree.session_ended(T0 + 5);
        assert_eq!(ids(&changes.changed), ["a", "root"]);
        for node in tree.nodes() {
            assert_eq!(
                (node.state, node.liveness, node.outcome, node.ended_at_ms),
                (
                    NodeState::Idle,
                    Liveness::Gone,
                    Some(NodeOutcome::Disconnected),
                    Some(T0 + 5)
                )
            );
        }
        assert!(tree.session_ended(T0 + 6).is_empty());
        assert!(!tree.needs_attention());
    }

    #[test]
    fn the_tree_survives_a_json_round_trip() {
        let mut tree = tree();
        tree.apply(sub("a", None, NodeState::Working), T0);
        let json = serde_json::to_string(&tree).unwrap();
        let back: ObservationTree = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tree);
        // The placeholder flag is part of it: the real root still takes the children over.
        let mut back = back;
        back.apply(
            update(Some("Main"), NodeKind::Main, "hooks", NodeState::Working),
            T0 + 1,
        );
        assert_eq!(back.node("a").unwrap().parent.as_deref(), Some("Main"));
    }

    #[test]
    fn nodes_serialize_without_absent_fields() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        let value = serde_json::to_value(tree.root().unwrap()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "node_id": "root",
                "kind": "main",
                "source": "hooks",
                "agent": "fake",
                "state": "working",
                "liveness": "alive",
                "started_at_ms": T0,
                "last_activity_ms": T0,
            })
        );
    }

    #[test]
    fn sequence_numbers_must_increase_per_source_and_node() {
        let mut tree = tree();
        assert!(tree.accept_seq("a", None, 10));
        assert!(!tree.accept_seq("a", None, 10));
        assert!(!tree.accept_seq("a", None, 9));
        assert!(tree.accept_seq("a", None, 11));
        // Another source, and another node, count on their own.
        assert!(tree.accept_seq("b", None, 1));
        assert!(tree.accept_seq("a", Some("n1"), 1));
        assert!(!tree.accept_seq("a", Some("n1"), 1));
        assert!(tree.accept_seq("a", Some("n2"), 1));
        // A source and a node whose names run together are not confused.
        assert!(tree.accept_seq("a\u{1f}n", None, 1));
    }

    #[test]
    fn only_the_quietest_sequence_numbers_are_forgotten() {
        let mut tree = tree();
        for n in 0..MAX_SEQ_ENTRIES as u64 {
            assert!(tree.accept_seq("s", Some(&format!("n{n}")), 100 + n));
        }
        assert!(tree.accept_seq("s", Some("new"), 5_000));
        assert!(tree.seqs.len() <= MAX_SEQ_ENTRIES);
        // The oldest key was dropped, the newest are still guarded.
        assert!(!tree.accept_seq("s", Some(&format!("n{}", MAX_SEQ_ENTRIES - 1)), 100));
        assert!(!tree.accept_seq("s", Some("new"), 5_000));
    }

    #[test]
    fn a_node_that_is_over_holds_nothing_to_release_and_a_new_run_starts_afresh() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        assert!(
            tree.release("hooks", None, NodeOutcome::Completed, T0 + 1)
                .is_some()
        );
        assert!(
            tree.release("hooks", None, NodeOutcome::Completed, T0 + 2)
                .is_none()
        );
        // The agent runs again in the same session.
        tree.apply(main_report(NodeState::Working), T0 + 3_000);
        let root = tree.root().unwrap();
        assert_eq!(
            (
                root.state,
                root.outcome,
                root.ended_at_ms,
                root.started_at_ms
            ),
            (NodeState::Working, None, None, Some(T0 + 3_000))
        );
    }

    #[test]
    fn a_finished_node_keeps_the_outcome_it_ended_with() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        let mut done = sub("a", None, NodeState::Idle);
        done.outcome = Some(NodeOutcome::Completed);
        tree.apply(done, T0 + 1);
        // A later "closed" or "failed" (a parent shutting the finished child down) is no news.
        let mut late = sub("a", None, NodeState::Idle);
        late.outcome = Some(NodeOutcome::Interrupted);
        tree.apply(late, T0 + 2);
        let node = tree.node("a").unwrap();
        assert_eq!(node.outcome, Some(NodeOutcome::Completed));
        assert_eq!(node.ended_at_ms, Some(T0 + 1));
        // Nor does "unknown" take it back.
        tree.apply(sub("a", None, NodeState::Unknown), T0 + 3);
        assert_eq!(tree.node("a").unwrap().state, NodeState::Idle);
    }

    #[test]
    fn restored_subagents_are_disconnected_with_unknown_liveness() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        tree.apply(sub("a", None, NodeState::Working), T0 + 1);
        tree.restored(T0 + 5);
        let node = tree.node("a").unwrap();
        assert_eq!(
            (node.outcome, node.liveness, node.state),
            (
                Some(NodeOutcome::Disconnected),
                Liveness::Unknown,
                NodeState::Idle
            )
        );
    }

    #[test]
    fn a_finished_run_of_the_session_keeps_a_transcript_path() {
        let mut tree = tree();
        tree.apply(main_report(NodeState::Working), T0);
        let mut done = sub("a", None, NodeState::Idle);
        done.outcome = Some(NodeOutcome::Completed);
        done.transcript_path = Some("/t/agent-a.jsonl".into());
        tree.apply(done, T0 + 1);
        assert_eq!(
            tree.node("a").unwrap().transcript_path.as_deref(),
            Some("/t/agent-a.jsonl")
        );
    }
}
