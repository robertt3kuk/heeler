//! ACP traffic → updates of the session's agent observation tree (`docs/integrations.md`
//! § Observation tree). The conversation's own agent is the root; subagents appear when the
//! agent's tool calls say they start one.
//!
//! ACP has no subagent events, so the evidence is the shape of a tool call's `rawInput`:
//!
//! - OMP's `task` tool: `{agent, task, name?}` for one subagent, `{tasks: [{agent, task,
//!   name?}, …]}` for a batch;
//! - Claude Code's `Task` tool: `{subagent_type, description, prompt}`.
//!
//! A subagent works from its tool call's start to the call's last update: a `completed` or
//! `failed` status finishes it (`completed` / `failed`), and a turn that ends first (cancelled)
//! leaves it `interrupted`. A tool call that does not look like that is just activity of the
//! agent that made it. Pure: JSON in, [`NodeUpdate`]s out.

use std::collections::HashMap;

use hutan_core::{NodeKind, NodeOutcome, NodeState, NodeUpdate, Patch, Usage};
use serde_json::Value;

/// The reporting source of Hutan-owned harness chats.
pub const TREE_SOURCE: &str = "acp";
/// Tool calls whose subagents are remembered at most.
const MAX_CALLS: usize = 256;

#[derive(Debug)]
pub struct NodeFeed {
    agent: String,
    root: String,
    /// Tool call id → the subagent nodes it started.
    subagents: HashMap<String, Vec<String>>,
    /// Tool calls that set the root's activity and have not ended.
    activity: HashMap<String, String>,
}

impl NodeFeed {
    pub fn new(agent: &str, session_id: String) -> Self {
        Self {
            agent: agent.to_owned(),
            root: session_id,
            subagents: HashMap::new(),
            activity: HashMap::new(),
        }
    }

    fn base(&self, id: &str) -> NodeUpdate {
        NodeUpdate {
            id: Some(id.to_owned()),
            kind: if id == self.root {
                NodeKind::Main
            } else {
                NodeKind::Sub
            },
            source: TREE_SOURCE.to_owned(),
            agent: self.agent.clone(),
            ..NodeUpdate::default()
        }
    }

    fn root_update(&self, state: NodeState) -> NodeUpdate {
        NodeUpdate {
            state: Some(state),
            ..self.base(&self.root)
        }
    }

    /// The main agent is up (a session was opened or resumed).
    pub fn started(&self, model: &str) -> NodeUpdate {
        NodeUpdate {
            model: (!model.is_empty()).then(|| model.to_owned()),
            ..self.root_update(NodeState::Idle)
        }
    }

    pub fn turn_started(&self) -> NodeUpdate {
        self.root_update(NodeState::Working)
    }

    /// A permission request waits for the user.
    pub fn blocked(&self) -> NodeUpdate {
        self.root_update(NodeState::Blocked)
    }

    /// The user answered; the agent works on.
    pub fn unblocked(&self) -> NodeUpdate {
        self.root_update(NodeState::Working)
    }

    /// The turn ended: the root is idle, subagents still running were cut short (`cancelled`)
    /// or are over.
    pub fn turn_ended(&mut self, cancelled: bool) -> Vec<NodeUpdate> {
        let mut updates = vec![NodeUpdate {
            activity: Patch::Clear,
            ..self.root_update(NodeState::Idle)
        }];
        let outcome = if cancelled {
            NodeOutcome::Interrupted
        } else {
            NodeOutcome::Completed
        };
        let ending: Vec<Vec<String>> = self.subagents.drain().map(|(_, nodes)| nodes).collect();
        for nodes in ending {
            for node in nodes {
                updates.push(NodeUpdate {
                    parent: Some(self.root.clone()),
                    outcome: Some(outcome),
                    ..self.base(&node)
                });
            }
        }
        self.activity.clear();
        updates
    }

    /// One `session/update` notification (its params).
    pub fn update(&mut self, params: &Value) -> Vec<NodeUpdate> {
        let Some(update) = params.get("update") else {
            return Vec::new();
        };
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("usage_update") => {
                let usage = Usage {
                    kind: "acp.context".to_owned(),
                    context_tokens: update.get("used").and_then(Value::as_u64),
                    context_window: update.get("size").and_then(Value::as_u64),
                    cost_usd: update
                        .pointer("/cost/amount")
                        .and_then(Value::as_f64)
                        .filter(|_| {
                            update.pointer("/cost/currency").and_then(Value::as_str) == Some("USD")
                        }),
                    ..Usage::default()
                };
                vec![NodeUpdate {
                    usage: Some(usage),
                    ..self.base(&self.root)
                }]
            }
            Some("tool_call") => self.tool_call(update),
            Some("tool_call_update") => self.tool_call_update(update),
            _ => Vec::new(),
        }
    }

    fn tool_call(&mut self, update: &Value) -> Vec<NodeUpdate> {
        let Some(call) = update.get("toolCallId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let mut updates = Vec::new();
        let title = update.get("title").and_then(Value::as_str).unwrap_or("");
        if !title.is_empty() && !self.activity.contains_key(call) {
            self.activity.insert(call.to_owned(), title.to_owned());
            updates.push(NodeUpdate {
                activity: Patch::Set(title.to_owned()),
                ..self.base(&self.root)
            });
        }
        if self.subagents.len() >= MAX_CALLS {
            self.subagents.clear();
        }
        let spawned = update.get("rawInput").map(subagents_of).unwrap_or_default();
        if spawned.is_empty() || self.subagents.contains_key(call) {
            return updates;
        }
        let mut ids = Vec::new();
        for (index, (name, kind)) in spawned.iter().enumerate() {
            let id = if spawned.len() == 1 {
                call.to_owned()
            } else {
                format!("{call}#{index}")
            };
            updates.push(NodeUpdate {
                parent: Some(self.root.clone()),
                name: name.clone(),
                agent_type: kind.clone(),
                state: Some(NodeState::Working),
                ..self.base(&id)
            });
            ids.push(id);
        }
        self.subagents.insert(call.to_owned(), ids);
        updates
    }

    fn tool_call_update(&mut self, update: &Value) -> Vec<NodeUpdate> {
        let Some(call) = update.get("toolCallId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let status = update.get("status").and_then(Value::as_str);
        let mut updates = Vec::new();
        if matches!(status, Some("completed" | "failed"))
            && self.activity.remove(call).is_some()
            && self.activity.is_empty()
        {
            updates.push(NodeUpdate {
                activity: Patch::Clear,
                ..self.base(&self.root)
            });
        }
        let outcome = match status {
            Some("completed") => NodeOutcome::Completed,
            Some("failed") => NodeOutcome::Failed,
            _ => return updates,
        };
        if let Some(nodes) = self.subagents.remove(call) {
            for node in nodes {
                updates.push(NodeUpdate {
                    parent: Some(self.root.clone()),
                    outcome: Some(outcome),
                    ..self.base(&node)
                });
            }
        }
        updates
    }
}

/// `(name, type)` of every subagent a tool call's input starts; empty for other calls.
fn subagents_of(input: &Value) -> Vec<(Option<String>, Option<String>)> {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    // OMP, batch.
    if let Some(tasks) = input.get("tasks").and_then(Value::as_array) {
        let found: Vec<_> = tasks
            .iter()
            .filter(|task| task.get("task").is_some_and(Value::is_string))
            .map(|task| (text(task, "name"), text(task, "agent")))
            .collect();
        if !found.is_empty() {
            return found;
        }
    }
    // OMP, single.
    if input.get("task").is_some_and(Value::is_string)
        && (input.get("agent").is_some_and(Value::is_string)
            || input.get("solutionSpace").is_some())
    {
        return vec![(text(input, "name"), text(input, "agent"))];
    }
    // Claude Code's `Task`.
    if input.get("subagent_type").is_some_and(Value::is_string) {
        return vec![(text(input, "description"), text(input, "subagent_type"))];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn update(value: Value) -> Value {
        json!({"sessionId": "s1", "update": value})
    }

    #[test]
    fn claude_code_task_calls_are_subagents_until_the_call_ends() {
        let mut feed = NodeFeed::new("claude", "s1".to_owned());
        let started = feed.update(&update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Task", "kind": "think",
            "rawInput": {"subagent_type": "Explore", "description": "map the repo", "prompt": "…"}
        })));
        let sub = started.iter().find(|u| u.kind == NodeKind::Sub).unwrap();
        assert_eq!(sub.id.as_deref(), Some("t1"));
        assert_eq!(sub.name.as_deref(), Some("map the repo"));
        assert_eq!(sub.agent_type.as_deref(), Some("Explore"));
        assert_eq!(sub.parent.as_deref(), Some("s1"));
        assert_eq!(sub.agent, "claude");

        let ended = feed.update(&update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "failed"
        })));
        let sub = ended.iter().find(|u| u.kind == NodeKind::Sub).unwrap();
        assert_eq!(sub.outcome, Some(NodeOutcome::Failed));
    }

    #[test]
    fn other_tool_calls_are_activity_of_the_main_agent() {
        let mut feed = NodeFeed::new("omp", "s1".to_owned());
        let started = feed.update(&update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "$ ls", "kind": "execute",
            "rawInput": {"command": "ls"}
        })));
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].kind, NodeKind::Main);
        assert_eq!(started[0].activity, Patch::Set("$ ls".to_owned()));
        let ended = feed.update(&update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"
        })));
        assert_eq!(ended[0].activity, Patch::Clear);
    }

    #[test]
    fn a_cancelled_turn_interrupts_running_subagents() {
        let mut feed = NodeFeed::new("omp", "s1".to_owned());
        feed.update(&update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "task", "kind": "other",
            "rawInput": {"agent": "task", "task": "x", "solutionSpace": "one fix"}
        })));
        let ended = feed.turn_ended(true);
        assert_eq!(ended[0].state, Some(NodeState::Idle));
        assert_eq!(ended[1].outcome, Some(NodeOutcome::Interrupted));
        assert!(feed.turn_ended(true).len() == 1, "nothing left to end");
    }

    #[test]
    fn usage_updates_are_context_usage_of_the_root() {
        let mut feed = NodeFeed::new("omp", "s1".to_owned());
        let updates = feed.update(&update(json!({
            "sessionUpdate": "usage_update", "size": 200000, "used": 5000,
            "cost": {"amount": 0.5, "currency": "USD"}
        })));
        let usage = updates[0].usage.as_ref().unwrap();
        assert_eq!(usage.kind, "acp.context");
        assert_eq!(
            (usage.context_tokens, usage.context_window),
            (Some(5000), Some(200_000))
        );
        assert_eq!(usage.cost_usd, Some(0.5));
    }
}
