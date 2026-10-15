//! Codex app-server notifications → updates of the session's agent observation tree
//! (`docs/integrations.md` § Observation tree). The conversation's own thread is the main
//! agent; every other thread the process reports on is a subagent.
//!
//! Sources, from the 0.162 schema: `thread/started` (`Thread.parentThreadId`, `agentNickname`,
//! `agentRole`, `model`), `thread/status/changed` (`active` with `waitingOnApproval` or
//! `waitingOnUserInput` is blocked), `thread/tokenUsage/updated` (usage kind `codex.total`),
//! `turn/started` / `turn/completed` (a subagent's turn ending is the subagent ending),
//! `collabAgentToolCall.agentsStates` and `subAgentActivity` items, and tool items for what a
//! thread is doing. **A finished tool call is not a finished child**: only the child's own
//! status (`agentsStates`, `subAgentActivity`, its turn) can end it.
//!
//! Pure: JSON in, [`NodeUpdate`]s out.

use std::collections::HashSet;

use serde_json::Value;
use staffy_core::{NodeKind, NodeOutcome, NodeState, NodeUpdate, Patch, Usage};

/// The reporting source of Staffy-owned Codex chats.
pub const TREE_SOURCE: &str = "codex-app-server";
/// Threads remembered as seen; past this the memory starts over (names are then re-derived).
const MAX_SEEN: usize = 512;

#[derive(Debug)]
pub struct NodeFeed {
    /// The conversation Staffy opened: the main agent.
    thread_id: String,
    seen: HashSet<String>,
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

fn num_at(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

impl NodeFeed {
    pub fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            seen: HashSet::new(),
        }
    }

    /// The main agent is up (a thread was opened or resumed).
    pub fn started(&self, model: &str) -> NodeUpdate {
        NodeUpdate {
            model: (!model.is_empty()).then(|| model.to_owned()),
            state: Some(NodeState::Idle),
            ..self.base(&self.thread_id.clone())
        }
    }

    fn base(&self, thread: &str) -> NodeUpdate {
        NodeUpdate {
            id: Some(thread.to_owned()),
            kind: if thread == self.thread_id {
                NodeKind::Main
            } else {
                NodeKind::Sub
            },
            source: TREE_SOURCE.to_owned(),
            agent: "codex".to_owned(),
            ..NodeUpdate::default()
        }
    }

    /// A subagent's update, with its parent when known; `fresh` is true the first time the
    /// thread is seen (then it may be named from weaker evidence).
    fn child(&mut self, thread: &str, parent: Option<&str>) -> (NodeUpdate, bool) {
        if self.seen.len() >= MAX_SEEN {
            self.seen.clear();
        }
        let fresh = self.seen.insert(thread.to_owned());
        let mut update = self.base(thread);
        update.parent = parent.map(str::to_owned);
        (update, fresh)
    }

    fn node(&mut self, thread: &str, parent: Option<&str>) -> NodeUpdate {
        if thread == self.thread_id {
            self.base(thread)
        } else {
            self.child(thread, parent).0
        }
    }

    pub fn notification(&mut self, method: &str, params: &Value) -> Vec<NodeUpdate> {
        let thread = str_at(params, "/threadId");
        match (method, thread) {
            ("thread/started", _) => self.thread_started(params.get("thread")),
            ("thread/status/changed", Some(thread)) => {
                let mut update = self.node(thread, None);
                let is_sub = thread != self.thread_id;
                match params.pointer("/status/type").and_then(Value::as_str) {
                    Some("idle") => update.state = Some(NodeState::Idle),
                    Some("active") => {
                        let waiting = params
                            .pointer("/status/activeFlags")
                            .and_then(Value::as_array)
                            .is_some_and(|flags| {
                                flags.iter().filter_map(Value::as_str).any(|flag| {
                                    matches!(flag, "waitingOnApproval" | "waitingOnUserInput")
                                })
                            });
                        update.state = Some(if waiting {
                            NodeState::Blocked
                        } else {
                            NodeState::Working
                        });
                    }
                    Some("systemError") => {
                        update.state = Some(NodeState::Idle);
                        update.outcome = is_sub.then_some(NodeOutcome::Failed);
                    }
                    Some("notLoaded") => update.state = Some(NodeState::Unknown),
                    _ => return Vec::new(),
                }
                vec![update]
            }
            ("thread/tokenUsage/updated", Some(thread)) => {
                let mut update = self.node(thread, None);
                update.usage = Some(Usage {
                    kind: "codex.total".to_owned(),
                    input: num_at(params, "/tokenUsage/total/inputTokens"),
                    output: num_at(params, "/tokenUsage/total/outputTokens"),
                    cache_read: num_at(params, "/tokenUsage/total/cachedInputTokens"),
                    cache_write: num_at(params, "/tokenUsage/total/cacheWriteInputTokens"),
                    reasoning: num_at(params, "/tokenUsage/total/reasoningOutputTokens"),
                    context_window: num_at(params, "/tokenUsage/modelContextWindow"),
                    ..Usage::default()
                });
                vec![update]
            }
            ("turn/started", Some(thread)) => {
                let mut update = self.node(thread, None);
                update.state = Some(NodeState::Working);
                vec![update]
            }
            ("turn/completed", Some(thread)) => {
                let mut update = self.node(thread, None);
                if thread == self.thread_id {
                    update.state = Some(NodeState::Idle);
                } else {
                    // A subagent's task is its turn.
                    match str_at(params, "/turn/status") {
                        Some("completed") => update.outcome = Some(NodeOutcome::Completed),
                        Some("interrupted") => update.outcome = Some(NodeOutcome::Interrupted),
                        Some("failed") => update.outcome = Some(NodeOutcome::Failed),
                        _ => return Vec::new(),
                    }
                }
                vec![update]
            }
            ("item/started" | "item/completed", Some(thread)) => {
                let started = method == "item/started";
                params
                    .get("item")
                    .map(|item| self.item(thread, item, started))
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    fn thread_started(&mut self, thread: Option<&Value>) -> Vec<NodeUpdate> {
        let Some(thread) = thread else {
            return Vec::new();
        };
        let Some(id) = str_at(thread, "/id") else {
            return Vec::new();
        };
        let mut update = self.node(id, str_at(thread, "/parentThreadId"));
        update.model = str_at(thread, "/model").map(str::to_owned);
        if id != self.thread_id {
            update.name = str_at(thread, "/agentNickname").map(str::to_owned);
            update.agent_type = str_at(thread, "/agentRole").map(str::to_owned);
        }
        vec![update]
    }

    fn item(&mut self, thread: &str, item: &Value, started: bool) -> Vec<NodeUpdate> {
        match str_at(item, "/type") {
            Some("collabAgentToolCall") => self.collab(thread, item),
            Some("subAgentActivity") => {
                let Some(child) = str_at(item, "/agentThreadId") else {
                    return Vec::new();
                };
                let (mut update, fresh) = self.child(child, Some(thread));
                update.model = str_at(item, "/model").map(str::to_owned);
                if fresh {
                    update.name = str_at(item, "/agentPath")
                        .and_then(|path| path.rsplit('/').find(|part| !part.is_empty()))
                        .map(str::to_owned);
                }
                match str_at(item, "/kind") {
                    Some("started" | "interacted") => update.state = Some(NodeState::Working),
                    Some("interrupted") => update.outcome = Some(NodeOutcome::Interrupted),
                    Some("completed") => update.outcome = Some(NodeOutcome::Completed),
                    _ => {}
                }
                vec![update]
            }
            Some(
                kind @ ("commandExecution" | "mcpToolCall" | "dynamicToolCall" | "fileChange"
                | "webSearch"),
            ) => {
                let mut update = self.node(thread, None);
                update.activity = if started {
                    Patch::Set(tool_activity(kind, item))
                } else {
                    Patch::Clear
                };
                vec![update]
            }
            _ => Vec::new(),
        }
    }

    /// `agentsStates` says how each receiver stands; the call's own status says nothing about
    /// the receivers.
    fn collab(&mut self, thread: &str, item: &Value) -> Vec<NodeUpdate> {
        let sender = str_at(item, "/senderThreadId").unwrap_or(thread).to_owned();
        let model = str_at(item, "/model");
        let spawns = str_at(item, "/tool") == Some("spawnAgent");
        let receivers: Vec<&str> = item
            .get("receiverThreadIds")
            .and_then(Value::as_array)
            .map(|ids| ids.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut updates = Vec::new();
        for receiver in receivers {
            let status = str_at(item, &format!("/agentsStates/{receiver}/status"));
            if status.is_none() && !spawns {
                continue;
            }
            let (mut update, _) = self.child(receiver, Some(&sender));
            if spawns {
                update.model = model.map(str::to_owned);
            }
            match status {
                Some("pendingInit") => {
                    update.state = Some(NodeState::Working);
                    update.activity = Patch::Set("starting".to_owned());
                }
                Some("running") => update.state = Some(NodeState::Working),
                Some("completed") => update.outcome = Some(NodeOutcome::Completed),
                Some("errored") => update.outcome = Some(NodeOutcome::Failed),
                Some("interrupted" | "shutdown") => update.outcome = Some(NodeOutcome::Interrupted),
                Some("notFound") => update.outcome = Some(NodeOutcome::Disconnected),
                _ => {}
            }
            updates.push(update);
        }
        updates
    }
}

fn tool_activity(kind: &str, item: &Value) -> String {
    match kind {
        "commandExecution" => format!("shell: {}", str_at(item, "/command").unwrap_or("")),
        "mcpToolCall" => format!(
            "{}.{}",
            str_at(item, "/server").unwrap_or(""),
            str_at(item, "/tool").unwrap_or("")
        ),
        "dynamicToolCall" => str_at(item, "/tool").unwrap_or("tool").to_owned(),
        "fileChange" => "apply_patch".to_owned(),
        _ => format!("web_search: {}", str_at(item, "/query").unwrap_or("")),
    }
}
