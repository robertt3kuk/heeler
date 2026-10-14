//! The agent observation tree (protocol 0.10, capability `agent.tree`): DTOs of `agent.tree`,
//! of the node part of `agent.report`, and of the `agent.tree_updated` event. The nodes
//! themselves are the core [`AgentNode`](heeler_core::AgentNode).

use heeler_core::{AgentObservation, NodeKind, NodeOutcome, SessionId, Usage};
use serde::{Deserialize, Serialize};

/// The node an `agent.report` is about (protocol 0.10). A report without one is about the
/// main agent, as before.
///
/// Fields left out keep what the daemon holds, except `activity`, which every report replaces:
/// a report that says nothing about it clears it. `outcome` finishes the node (its state
/// becomes `idle`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeReport {
    /// The reporter's own id for the agent, unique within the session (`1-Explore`,
    /// Claude's `agent_id`).
    pub id: String,
    /// The spawning agent's node id; absent or unknown means the main agent. Only read the
    /// first time the node is reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// `main` without a `parent` is the session's main agent (the root of the tree).
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The agent definition or role (`Explore`, `task`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<NodeOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What the agent is doing now (at most 200 characters are kept).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// The agent's own transcript file (a finished Claude subagent's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
}

/// `agent.tree`: the observation of one session, or of every session that has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentTreeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// Result of `agent.tree`: sessions without agents (or whose finished agents were already
/// forgotten) are not listed. In the order the sessions were created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTree {
    pub observations: Vec<AgentObservation>,
}
