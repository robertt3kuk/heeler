//! Codex app-server messages → [`AgentEventDraft`]s. Pure state machine: no I/O, so every
//! notification kind is tested by feeding it JSON.
//!
//! It keeps the correlation state the unified model needs: provider turn/item ids ↔ local ids,
//! and the server requests (approvals) that are waiting for an answer.

use std::collections::HashMap;

use heeler_core::agent::{
    FileChange, FileChangeKind, MAX_OUTPUT_BYTES, MAX_PAYLOAD_BYTES, MessageRole, NoticeLevel,
    PermissionKind, PermissionOption, PermissionOptionKind, PermissionOutcome, PlanStep,
    PlanStepStatus, ReasoningPart, TokenCounts, ToolKind, ToolStatus,
};
use heeler_core::{AgentEventDraft, AgentEventKind, ApprovalId, ItemId, TurnId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracing::debug;

use super::wire::{
    AgentMessageItem, CommandApprovalParams, CommandExecutionItem, DeltaNotification,
    DynamicToolCallItem, ErrorNotification, FileChangeApprovalParams, FileChangeItem,
    FileUpdateChange, ItemNotification, McpToolCallItem, PatchUpdatedNotification, PlanItem,
    PermissionsApprovalParams, ProgressNotification, ReasoningItem, ServerRequestResolved,
    TokenBreakdown, TokenUsageNotification, TurnDiffNotification, TurnNotification,
    TurnPlanNotification, UserMessageItem, WarningNotification, WebSearchItem,
};

/// JSON-RPC "method not found".
const METHOD_NOT_FOUND: i64 = -32601;

/// Notifications that carry nothing a conversation needs: connection and account chatter.
const IGNORED: &[&str] = &[
    "account/updated",
    "account/rateLimits/updated",
    "account/login/completed",
    "mcpServer/startupStatus/updated",
    "remoteControl/status/changed",
    "thread/started",
    "thread/status/changed",
    "thread/goal/cleared",
    "thread/goal/updated",
    "skills/changed",
    "deprecationNotice",
    "app/list/updated",
    "hook/started",
    "hook/completed",
];

/// What the adapter does with a request of the server.
#[derive(Debug, Default)]
pub struct Mapped {
    pub drafts: Vec<AgentEventDraft>,
    /// Answer to send right away (requests Heeler does not support); approvals are answered
    /// later through [`Mapper::answer`].
    pub reply: Option<Reply>,
}

#[derive(Debug)]
pub struct Reply {
    pub id: Value,
    pub outcome: Result<Value, (i64, String)>,
}

#[derive(Debug)]
struct PendingRequest {
    rpc_id: Value,
    approval: ApprovalId,
    provider_turn: String,
    /// `(option id, the response result that choosing it sends)`.
    results: Vec<(String, Value)>,
    answered: Option<String>,
}

#[derive(Debug)]
pub struct Mapper {
    thread_id: String,
    turns: HashMap<String, TurnId>,
    /// Provider item id → (provider turn id, local id); dropped when the turn ends.
    items: HashMap<String, (String, ItemId)>,
    /// By the JSON text of the request id.
    pending: HashMap<String, PendingRequest>,
}

/// Why an answer to a pending approval cannot be sent.
#[derive(Debug, PartialEq, Eq)]
pub enum AnswerError {
    Unknown,
    NotOffered(String),
}

impl Mapper {
    pub fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            turns: HashMap::new(),
            items: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    /// The local id of a provider turn (created on first sight).
    pub fn turn_id(&mut self, provider_turn: &str) -> TurnId {
        *self
            .turns
            .entry(provider_turn.to_owned())
            .or_insert_with(TurnId::new)
    }

    fn item_id(&mut self, provider_turn: &str, provider_item: &str) -> ItemId {
        self.items
            .entry(provider_item.to_owned())
            .or_insert_with(|| (provider_turn.to_owned(), ItemId::new()))
            .1
    }

    fn draft(
        &mut self,
        turn: Option<&str>,
        item: Option<&str>,
        kind: AgentEventKind,
    ) -> AgentEventDraft {
        let mut draft = AgentEventDraft::new(kind);
        draft.provider.thread_id = Some(self.thread_id.clone());
        if let Some(turn) = turn {
            draft.turn_id = Some(self.turn_id(turn));
            draft.provider.turn_id = Some(turn.to_owned());
            if let Some(item) = item {
                draft.item_id = Some(self.item_id(turn, item));
                draft.provider.item_id = Some(item.to_owned());
            }
        }
        draft
    }

    /// The event kind for a conversation that was just opened or resumed.
    pub fn resume_locator(&mut self, model: &str) -> AgentEventDraft {
        let thread = self.thread_id.clone();
        self.draft(
            None,
            None,
            AgentEventKind::ResumeLocator {
                agent: "codex".to_owned(),
                thread_id: thread,
                model: Some(model.to_owned()),
            },
        )
    }

    /// Whether any approval still waits for an answer.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    // Notifications ----------------------------------------------------------------------------

    pub fn notification(&mut self, method: &str, params: Value) -> Vec<AgentEventDraft> {
        if let Some(thread) = params.get("threadId").and_then(Value::as_str)
            && thread != self.thread_id
        {
            // Another conversation on the same process (a sub-agent's thread).
            debug!(method, thread, "ignoring a notification of another thread");
            return Vec::new();
        }
        let drafts = match method {
            "turn/started" => self.with(&params, method, Self::turn_started),
            "turn/completed" => self.with(&params, method, Self::turn_completed),
            "item/started" => self.with(&params, method, |m, n: ItemNotification| {
                m.item_started(&n.turn_id, n.item)
            }),
            "item/completed" => self.with(&params, method, |m, n: ItemNotification| {
                m.item_completed(&n.turn_id, n.item)
            }),
            "item/agentMessage/delta" => self.with(&params, method, |m, n: DeltaNotification| {
                vec![m.draft(
                    Some(&n.turn_id),
                    Some(&n.item_id),
                    AgentEventKind::MessageDelta { text: n.delta },
                )]
            }),
            "item/reasoning/summaryTextDelta" => {
                self.with(&params, method, |m, n: DeltaNotification| {
                    vec![m.reasoning_delta(n, ReasoningPart::Summary)]
                })
            }
            "item/reasoning/textDelta" => self.with(&params, method, |m, n: DeltaNotification| {
                vec![m.reasoning_delta(n, ReasoningPart::Content)]
            }),
            "item/commandExecution/outputDelta" => {
                self.with(&params, method, |m, n: DeltaNotification| {
                    vec![m.draft(
                        Some(&n.turn_id),
                        Some(&n.item_id),
                        AgentEventKind::CommandOutput { text: n.delta },
                    )]
                })
            }
            "item/fileChange/outputDelta" => {
                self.with(&params, method, |m, n: DeltaNotification| {
                    vec![m.draft(
                        Some(&n.turn_id),
                        Some(&n.item_id),
                        AgentEventKind::ToolUpdate { message: n.delta },
                    )]
                })
            }
            "item/mcpToolCall/progress" => {
                self.with(&params, method, |m, n: ProgressNotification| {
                    vec![m.draft(
                        Some(&n.turn_id),
                        Some(&n.item_id),
                        AgentEventKind::ToolUpdate { message: n.message },
                    )]
                })
            }
            "item/fileChange/patchUpdated" => {
                self.with(&params, method, |m, n: PatchUpdatedNotification| {
                    let kind = AgentEventKind::FileDiff {
                        changes: n.changes.iter().map(file_change).collect(),
                        patch: None,
                    };
                    vec![m.draft(Some(&n.turn_id), Some(&n.item_id), kind)]
                })
            }
            "turn/diff/updated" => self.with(&params, method, |m, n: TurnDiffNotification| {
                let patch = truncated(n.diff).0;
                vec![m.draft(
                    Some(&n.turn_id),
                    None,
                    AgentEventKind::FileDiff {
                        changes: Vec::new(),
                        patch: Some(patch),
                    },
                )]
            }),
            "turn/plan/updated" => self.with(&params, method, |m, n: TurnPlanNotification| {
                let steps = n
                    .plan
                    .into_iter()
                    .map(|s| PlanStep {
                        text: s.step,
                        status: match s.status.as_str() {
                            "completed" => PlanStepStatus::Completed,
                            "inProgress" => PlanStepStatus::InProgress,
                            _ => PlanStepStatus::Pending,
                        },
                    })
                    .collect();
                vec![m.draft(
                    Some(&n.turn_id),
                    None,
                    AgentEventKind::Plan {
                        explanation: n.explanation,
                        steps,
                        text: None,
                    },
                )]
            }),
            "thread/tokenUsage/updated" => {
                self.with(&params, method, |m, n: TokenUsageNotification| {
                    let kind = AgentEventKind::Usage {
                        total: n.token_usage.total.map(counts),
                        last: n.token_usage.last.map(counts),
                        context_window: n.token_usage.model_context_window,
                    };
                    vec![m.draft(n.turn_id.as_deref(), None, kind)]
                })
            }
            "serverRequest/resolved" => {
                self.with(&params, method, |m, n: ServerRequestResolved| {
                    m.request_resolved(&n.request_id)
                })
            }
            "error" => self.with(&params, method, |m, n: ErrorNotification| {
                let (level, prefix) = if n.will_retry {
                    (NoticeLevel::Warning, "the agent hit an error and will retry: ")
                } else {
                    (NoticeLevel::Error, "")
                };
                vec![m.notice(level, format!("{prefix}{}", n.error.message), &params)]
            }),
            "warning" => self.with(&params, method, |m, n: WarningNotification| {
                vec![m.notice(NoticeLevel::Warning, n.message, &params)]
            }),
            _ if IGNORED.contains(&method) => Vec::new(),
            _ => vec![self.opaque(method, params)],
        };
        drafts
    }

    /// Decodes `params` for `handler`; a message that does not fit its schema becomes an
    /// opaque event instead of an error.
    fn with<T: DeserializeOwned>(
        &mut self,
        params: &Value,
        method: &str,
        handler: impl FnOnce(&mut Self, T) -> Vec<AgentEventDraft>,
    ) -> Vec<AgentEventDraft> {
        match serde_json::from_value::<T>(params.clone()) {
            Ok(typed) => handler(self, typed),
            Err(error) => {
                debug!(method, %error, "notification does not match the expected shape");
                vec![self.opaque(method, params.clone())]
            }
        }
    }

    fn opaque(&mut self, method: &str, params: Value) -> AgentEventDraft {
        let turn = params
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut draft = self.draft(
            turn.as_deref(),
            None,
            AgentEventKind::Opaque {
                method: method.to_owned(),
            },
        );
        draft.payload = bounded(params);
        draft
    }

    fn notice(&mut self, level: NoticeLevel, message: String, params: &Value) -> AgentEventDraft {
        let turn = params
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut draft = self.draft(
            turn.as_deref(),
            None,
            AgentEventKind::Notice { level, message },
        );
        draft.payload = bounded(params.clone());
        draft
    }

    fn reasoning_delta(&mut self, n: DeltaNotification, part: ReasoningPart) -> AgentEventDraft {
        self.draft(
            Some(&n.turn_id),
            Some(&n.item_id),
            AgentEventKind::ReasoningDelta {
                part,
                text: n.delta,
            },
        )
    }

    fn turn_started(&mut self, n: TurnNotification) -> Vec<AgentEventDraft> {
        vec![self.draft(Some(&n.turn.id), None, AgentEventKind::TurnStarted)]
    }

    fn turn_completed(&mut self, n: TurnNotification) -> Vec<AgentEventDraft> {
        let turn = n.turn.id.clone();
        // Requests nobody answered die with the turn; requests we answered but whose
        // `serverRequest/resolved` never came count as answered.
        let mut drafts = self.resolve_pending_of(&turn);
        let kind = match n.turn.status.as_str() {
            "completed" => Some(AgentEventKind::TurnCompleted {
                duration_ms: n.turn.duration_ms,
            }),
            "interrupted" => Some(AgentEventKind::TurnCancelled),
            "failed" => {
                let error = n.turn.error;
                Some(AgentEventKind::TurnFailed {
                    message: error
                        .as_ref()
                        .map_or_else(|| "the turn failed".to_owned(), |e| e.message.clone()),
                    details: error.and_then(|e| e.additional_details),
                })
            }
            other => {
                debug!(status = other, "turn/completed with an unexpected status");
                None
            }
        };
        if let Some(kind) = kind {
            drafts.push(self.draft(Some(&turn), None, kind));
        }
        // The turn's items are done; keep the maps from growing for the life of the session.
        self.items.retain(|_, (item_turn, _)| *item_turn != turn);
        self.turns.remove(&turn);
        drafts
    }

    // Items ------------------------------------------------------------------------------------

    fn item_started(&mut self, turn: &str, item: Value) -> Vec<AgentEventDraft> {
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "userMessage" => self.typed_item(turn, item, "item/started", |m, i: UserMessageItem| {
                vec![m.draft(
                    Some(turn),
                    Some(&i.id),
                    AgentEventKind::MessageStart {
                        role: MessageRole::User,
                        phase: None,
                    },
                )]
            }),
            "agentMessage" => {
                self.typed_item(turn, item, "item/started", |m, i: AgentMessageItem| {
                    vec![m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::MessageStart {
                            role: MessageRole::Assistant,
                            phase: i.phase,
                        },
                    )]
                })
            }
            "reasoning" => self.typed_item(turn, item, "item/started", |m, i: ReasoningItem| {
                vec![m.draft(Some(turn), Some(&i.id), AgentEventKind::ReasoningStart)]
            }),
            "commandExecution" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/started", |m, i: CommandExecutionItem| {
                    let mut draft = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolStart {
                            tool: ToolKind::Command,
                            name: "shell".to_owned(),
                            title: i.command.clone(),
                            input: Some(json!({
                                "command": i.command,
                                "cwd": i.cwd,
                                "actions": i.command_actions,
                            })),
                        },
                    );
                    draft.payload = bounded(raw);
                    vec![draft]
                })
            }
            "fileChange" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/started", |m, i: FileChangeItem| {
                    let changes: Vec<FileChange> = i.changes.iter().map(file_change).collect();
                    let mut start = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolStart {
                            tool: ToolKind::FileChange,
                            name: "apply_patch".to_owned(),
                            title: file_change_title(&changes),
                            input: None,
                        },
                    );
                    start.payload = bounded(raw);
                    let mut drafts = vec![start];
                    if !changes.is_empty() {
                        drafts.push(m.draft(
                            Some(turn),
                            Some(&i.id),
                            AgentEventKind::FileDiff {
                                changes,
                                patch: None,
                            },
                        ));
                    }
                    drafts
                })
            }
            "mcpToolCall" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/started", |m, i: McpToolCallItem| {
                    let mut draft = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolStart {
                            tool: ToolKind::Mcp,
                            name: format!("{}.{}", i.server, i.tool),
                            title: format!("{}/{}", i.server, i.tool),
                            input: i.arguments,
                        },
                    );
                    draft.payload = bounded(raw);
                    vec![draft]
                })
            }
            "dynamicToolCall" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/started", |m, i: DynamicToolCallItem| {
                    let name = match &i.namespace {
                        Some(ns) => format!("{ns}.{}", i.tool),
                        None => i.tool.clone(),
                    };
                    let mut draft = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolStart {
                            tool: ToolKind::Dynamic,
                            title: name.clone(),
                            name,
                            input: i.arguments,
                        },
                    );
                    draft.payload = bounded(raw);
                    vec![draft]
                })
            }
            "webSearch" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/started", |m, i: WebSearchItem| {
                    let mut draft = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolStart {
                            tool: ToolKind::WebSearch,
                            name: "web_search".to_owned(),
                            title: i.query,
                            input: None,
                        },
                    );
                    draft.payload = bounded(raw);
                    vec![draft]
                })
            }
            // A plan arrives complete in `item/completed`.
            "plan" => Vec::new(),
            _ => self.opaque_item(turn, "item/started", item),
        }
    }

    fn item_completed(&mut self, turn: &str, item: Value) -> Vec<AgentEventDraft> {
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "userMessage" => {
                self.typed_item(turn, item, "item/completed", |m, i: UserMessageItem| {
                    let text = i
                        .content
                        .iter()
                        .filter(|part| part.kind == "text")
                        .filter_map(|part| part.text.as_deref())
                        .collect::<Vec<_>>()
                        .join("\n");
                    vec![m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::MessageEnd {
                            role: MessageRole::User,
                            phase: None,
                            text,
                        },
                    )]
                })
            }
            "agentMessage" => {
                self.typed_item(turn, item, "item/completed", |m, i: AgentMessageItem| {
                    vec![m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::MessageEnd {
                            role: MessageRole::Assistant,
                            phase: i.phase,
                            text: i.text,
                        },
                    )]
                })
            }
            "reasoning" => self.typed_item(turn, item, "item/completed", |m, i: ReasoningItem| {
                vec![m.draft(
                    Some(turn),
                    Some(&i.id),
                    AgentEventKind::ReasoningEnd {
                        summary: i.summary,
                        content: i.content,
                    },
                )]
            }),
            "commandExecution" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/completed", |m, i: CommandExecutionItem| {
                    let (output, truncated_flag) = match i.aggregated_output {
                        Some(text) => {
                            let (text, cut) = truncated(text);
                            (Some(text), cut)
                        }
                        None => (None, false),
                    };
                    let mut draft = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolEnd {
                            tool: ToolKind::Command,
                            status: tool_status(&i.status),
                            output,
                            truncated: truncated_flag,
                            exit_code: i.exit_code,
                            duration_ms: i.duration_ms,
                        },
                    );
                    draft.payload = bounded(raw);
                    vec![draft]
                })
            }
            "fileChange" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/completed", |m, i: FileChangeItem| {
                    let changes: Vec<FileChange> = i.changes.iter().map(file_change).collect();
                    let mut drafts = Vec::new();
                    if !changes.is_empty() {
                        drafts.push(m.draft(
                            Some(turn),
                            Some(&i.id),
                            AgentEventKind::FileDiff {
                                changes,
                                patch: None,
                            },
                        ));
                    }
                    let mut end = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolEnd {
                            tool: ToolKind::FileChange,
                            status: tool_status(&i.status),
                            output: None,
                            truncated: false,
                            exit_code: None,
                            duration_ms: None,
                        },
                    );
                    end.payload = bounded(raw);
                    drafts.push(end);
                    drafts
                })
            }
            "mcpToolCall" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/completed", |m, i: McpToolCallItem| {
                    let output = mcp_output(&i);
                    let (output, cut) = match output {
                        Some(text) => {
                            let (text, cut) = truncated(text);
                            (Some(text), cut)
                        }
                        None => (None, false),
                    };
                    let mut draft = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolEnd {
                            tool: ToolKind::Mcp,
                            status: tool_status(&i.status),
                            output,
                            truncated: cut,
                            exit_code: None,
                            duration_ms: i.duration_ms,
                        },
                    );
                    draft.payload = bounded(raw);
                    vec![draft]
                })
            }
            "dynamicToolCall" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/completed", |m, i: DynamicToolCallItem| {
                    let status = match i.success {
                        Some(false) => ToolStatus::Failed,
                        _ => tool_status(&i.status),
                    };
                    let mut draft = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolEnd {
                            tool: ToolKind::Dynamic,
                            status,
                            output: None,
                            truncated: false,
                            exit_code: None,
                            duration_ms: i.duration_ms,
                        },
                    );
                    draft.payload = bounded(raw);
                    vec![draft]
                })
            }
            "webSearch" => {
                let raw = item.clone();
                self.typed_item(turn, item, "item/completed", |m, i: WebSearchItem| {
                    let mut draft = m.draft(
                        Some(turn),
                        Some(&i.id),
                        AgentEventKind::ToolEnd {
                            tool: ToolKind::WebSearch,
                            status: ToolStatus::Completed,
                            output: None,
                            truncated: false,
                            exit_code: None,
                            duration_ms: None,
                        },
                    );
                    draft.payload = bounded(raw);
                    vec![draft]
                })
            }
            "plan" => self.typed_item(turn, item, "item/completed", |m, i: PlanItem| {
                vec![m.draft(
                    Some(turn),
                    Some(&i.id),
                    AgentEventKind::Plan {
                        explanation: None,
                        steps: Vec::new(),
                        text: Some(i.text),
                    },
                )]
            }),
            _ => self.opaque_item(turn, "item/completed", item),
        }
    }

    fn typed_item<T: DeserializeOwned>(
        &mut self,
        turn: &str,
        item: Value,
        method: &str,
        handler: impl FnOnce(&mut Self, T) -> Vec<AgentEventDraft>,
    ) -> Vec<AgentEventDraft> {
        match serde_json::from_value::<T>(item.clone()) {
            Ok(typed) => handler(self, typed),
            Err(error) => {
                debug!(method, %error, "item does not match the expected shape");
                self.opaque_item(turn, method, item)
            }
        }
    }

    fn opaque_item(&mut self, turn: &str, method: &str, item: Value) -> Vec<AgentEventDraft> {
        let id = item.get("id").and_then(Value::as_str).map(str::to_owned);
        let mut draft = self.draft(
            Some(turn),
            id.as_deref(),
            AgentEventKind::Opaque {
                method: method.to_owned(),
            },
        );
        draft.payload = bounded(item);
        vec![draft]
    }

    // Requests of the server -------------------------------------------------------------------

    /// A request of the server. Approvals become pending and are answered through
    /// [`answer`](Self::answer); the rest is refused so the agent never waits for a client
    /// that cannot answer.
    pub fn request(&mut self, id: Value, method: &str, params: Value) -> Mapped {
        if let Some(thread) = params.get("threadId").and_then(Value::as_str)
            && thread != self.thread_id
        {
            return Mapped {
                drafts: Vec::new(),
                reply: Some(Reply {
                    id,
                    outcome: Err((METHOD_NOT_FOUND, "request for an unknown thread".to_owned())),
                }),
            };
        }
        let built = match method {
            "item/commandExecution/requestApproval" => {
                serde_json::from_value::<CommandApprovalParams>(params.clone())
                    .ok()
                    .map(|p| self.command_approval(p))
            }
            "item/fileChange/requestApproval" => {
                serde_json::from_value::<FileChangeApprovalParams>(params.clone())
                    .ok()
                    .map(|p| self.file_change_approval(p))
            }
            "item/permissions/requestApproval" => {
                serde_json::from_value::<PermissionsApprovalParams>(params.clone())
                    .ok()
                    .map(|p| self.permissions_approval(p))
            }
            _ => None,
        };
        match built {
            Some(approval) => self.register(id, approval, params),
            None => self.refuse(id, method, params),
        }
    }

    fn register(&mut self, rpc_id: Value, built: BuiltApproval, params: Value) -> Mapped {
        let approval = ApprovalId::new();
        let options = built
            .options
            .iter()
            .map(|(option, _)| option.clone())
            .collect();
        let mut draft = self.draft(
            Some(&built.provider_turn),
            Some(&built.provider_item),
            AgentEventKind::PermissionRequested {
                approval_id: approval,
                kind: built.kind,
                title: built.title,
                detail: built.detail,
                options,
            },
        );
        draft.provider.request_id = Some(rpc_id.to_string());
        draft.payload = bounded(params);
        self.pending.insert(
            rpc_id.to_string(),
            PendingRequest {
                rpc_id,
                approval,
                provider_turn: built.provider_turn,
                results: built
                    .options
                    .into_iter()
                    .map(|(option, result)| (option.id, result))
                    .collect(),
                answered: None,
            },
        );
        Mapped {
            drafts: vec![draft],
            reply: None,
        }
    }

    fn refuse(&mut self, id: Value, method: &str, params: Value) -> Mapped {
        let notice = self.notice(
            NoticeLevel::Warning,
            format!("the agent sent `{method}`, which Heeler does not support yet; it was declined"),
            &params,
        );
        Mapped {
            drafts: vec![notice],
            reply: Some(Reply {
                id,
                outcome: Err((
                    METHOD_NOT_FOUND,
                    format!("`{method}` is not supported by this client"),
                )),
            }),
        }
    }

    fn command_approval(&self, p: CommandApprovalParams) -> BuiltApproval {
        let decisions = p
            .available_decisions
            .unwrap_or_else(|| default_decisions(&["accept", "acceptForSession", "decline", "cancel"]));
        let title = p.command.clone().unwrap_or_else(|| "run a command".to_owned());
        let mut detail = Vec::new();
        if let Some(reason) = &p.reason {
            detail.push(reason.clone());
        }
        if let Some(cwd) = &p.cwd {
            detail.push(format!("in {cwd}"));
        }
        BuiltApproval {
            provider_turn: p.turn_id,
            provider_item: p.item_id,
            kind: PermissionKind::Command,
            title,
            detail: (!detail.is_empty()).then(|| detail.join("\n")),
            options: decisions.iter().map(decision_option).collect(),
        }
    }

    fn file_change_approval(&self, p: FileChangeApprovalParams) -> BuiltApproval {
        let decisions = p
            .available_decisions
            .unwrap_or_else(|| default_decisions(&["accept", "acceptForSession", "decline", "cancel"]));
        let mut detail = Vec::new();
        if let Some(reason) = &p.reason {
            detail.push(reason.clone());
        }
        if let Some(root) = &p.grant_root {
            detail.push(format!("asks for write access to {root}"));
        }
        BuiltApproval {
            provider_turn: p.turn_id,
            provider_item: p.item_id,
            kind: PermissionKind::FileChange,
            title: "apply file changes".to_owned(),
            detail: (!detail.is_empty()).then(|| detail.join("\n")),
            options: decisions.iter().map(decision_option).collect(),
        }
    }

    fn permissions_approval(&self, p: PermissionsApprovalParams) -> BuiltApproval {
        let option = |id: &str, label: &str, kind, permissions: &Value, scope: &str| {
            (
                PermissionOption {
                    id: id.to_owned(),
                    label: label.to_owned(),
                    kind,
                    provider: Some(json!({"permissions": permissions, "scope": scope})),
                },
                json!({"permissions": permissions, "scope": scope}),
            )
        };
        let none = json!({});
        BuiltApproval {
            provider_turn: p.turn_id,
            provider_item: p.item_id,
            kind: PermissionKind::Permissions,
            title: "additional permissions".to_owned(),
            detail: Some(
                [p.reason, Some(p.permissions.to_string())]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            options: vec![
                option(
                    "grant",
                    "Grant for this turn",
                    PermissionOptionKind::AllowOnce,
                    &p.permissions,
                    "turn",
                ),
                option(
                    "grant_session",
                    "Grant for this session",
                    PermissionOptionKind::AllowAlways,
                    &p.permissions,
                    "session",
                ),
                option(
                    "deny",
                    "Deny",
                    PermissionOptionKind::RejectOnce,
                    &none,
                    "turn",
                ),
            ],
        }
    }

    /// Picks the response for `option_id` of a pending approval and marks it answered.
    /// Returns the JSON-RPC id to answer and the result to send.
    pub fn answer(
        &mut self,
        approval: ApprovalId,
        option_id: &str,
    ) -> Result<(Value, Value), AnswerError> {
        let pending = self
            .pending
            .values_mut()
            .find(|p| p.approval == approval && p.answered.is_none())
            .ok_or(AnswerError::Unknown)?;
        let result = pending
            .results
            .iter()
            .find(|(id, _)| id == option_id)
            .map(|(_, result)| result.clone())
            .ok_or_else(|| AnswerError::NotOffered(option_id.to_owned()))?;
        pending.answered = Some(option_id.to_owned());
        Ok((pending.rpc_id.clone(), result))
    }

    /// Undoes [`answer`](Self::answer) when the response could not be sent.
    pub fn unanswer(&mut self, approval: ApprovalId) {
        if let Some(pending) = self.pending.values_mut().find(|p| p.approval == approval) {
            pending.answered = None;
        }
    }

    fn request_resolved(&mut self, request_id: &Value) -> Vec<AgentEventDraft> {
        let Some(pending) = self.pending.remove(&request_id.to_string()) else {
            return Vec::new();
        };
        vec![self.resolved_draft(&pending)]
    }

    fn resolved_draft(&mut self, pending: &PendingRequest) -> AgentEventDraft {
        let outcome = match &pending.answered {
            Some(option_id) => PermissionOutcome::Selected {
                option_id: option_id.clone(),
            },
            None => PermissionOutcome::Expired,
        };
        let mut draft = self.draft(
            Some(&pending.provider_turn),
            None,
            AgentEventKind::PermissionResolved {
                approval_id: pending.approval,
                outcome,
            },
        );
        draft.provider.request_id = Some(pending.rpc_id.to_string());
        draft
    }

    fn resolve_pending_of(&mut self, turn: &str) -> Vec<AgentEventDraft> {
        let keys: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| p.provider_turn == turn)
            .map(|(key, _)| key.clone())
            .collect();
        let mut drafts = Vec::new();
        for key in keys {
            if let Some(pending) = self.pending.remove(&key) {
                drafts.push(self.resolved_draft(&pending));
            }
        }
        drafts
    }

    /// The process is gone: nothing pending can be answered any more.
    pub fn expire_all(&mut self) -> Vec<AgentEventDraft> {
        let pending: Vec<PendingRequest> = self.pending.drain().map(|(_, p)| p).collect();
        pending.iter().map(|p| self.resolved_draft(p)).collect()
    }
}

struct BuiltApproval {
    provider_turn: String,
    provider_item: String,
    kind: PermissionKind,
    title: String,
    detail: Option<String>,
    /// The option and the JSON-RPC result that choosing it sends.
    options: Vec<(PermissionOption, Value)>,
}

fn default_decisions(names: &[&str]) -> Vec<Value> {
    names.iter().map(|n| Value::String((*n).to_owned())).collect()
}

/// One of the server's `availableDecisions`: a bare string, or an object with one key whose
/// value carries the details (`{"acceptWithExecpolicyAmendment": {...}}`).
fn decision_option(decision: &Value) -> (PermissionOption, Value) {
    let (id, detail) = match decision {
        Value::String(name) => (name.clone(), None),
        Value::Object(map) => match map.iter().next() {
            Some((name, detail)) => (name.clone(), Some(detail)),
            None => ("unknown".to_owned(), None),
        },
        other => (other.to_string(), None),
    };
    let (label, kind) = match id.as_str() {
        "accept" => ("Allow once".to_owned(), PermissionOptionKind::AllowOnce),
        "acceptForSession" => (
            "Allow for this session".to_owned(),
            PermissionOptionKind::AllowAlways,
        ),
        "acceptWithExecpolicyAmendment" => {
            let words = detail
                .and_then(|d| d.get("execpolicy_amendment"))
                .and_then(Value::as_array)
                .map(|words| {
                    words
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            (
                format!("Always allow `{words}`"),
                PermissionOptionKind::AllowAlways,
            )
        }
        "applyNetworkPolicyAmendment" => (
            "Apply the proposed network rule".to_owned(),
            PermissionOptionKind::AllowAlways,
        ),
        "decline" => ("Deny".to_owned(), PermissionOptionKind::RejectOnce),
        "cancel" => (
            "Deny and stop the turn".to_owned(),
            PermissionOptionKind::Cancel,
        ),
        other => (other.to_owned(), PermissionOptionKind::Other),
    };
    (
        PermissionOption {
            id,
            label,
            kind,
            provider: Some(decision.clone()),
        },
        json!({"decision": decision}),
    )
}

fn file_change(change: &FileUpdateChange) -> FileChange {
    let kind = match change.kind.kind.as_str() {
        "add" => FileChangeKind::Add,
        "delete" => FileChangeKind::Delete,
        _ => FileChangeKind::Update,
    };
    // Added files carry their content, deleted files the old content; updates a unified diff.
    // Heeler's model keeps the provider's text in `patch` in every case.
    FileChange {
        path: change.path.clone(),
        kind,
        move_path: change.kind.move_path.clone(),
        patch: (!change.diff.is_empty()).then(|| truncated(change.diff.clone()).0),
        before: None,
        after: None,
    }
}

fn file_change_title(changes: &[FileChange]) -> String {
    match changes {
        [] => "file changes".to_owned(),
        [one] => format!("edit {}", one.path),
        [first, rest @ ..] => format!("edit {} and {} more", first.path, rest.len()),
    }
}

fn tool_status(status: &str) -> ToolStatus {
    match status {
        "completed" => ToolStatus::Completed,
        "declined" => ToolStatus::Declined,
        _ => ToolStatus::Failed,
    }
}

fn mcp_output(item: &McpToolCallItem) -> Option<String> {
    if let Some(error) = &item.error {
        return Some(
            error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_owned),
        );
    }
    let result = item.result.as_ref()?;
    let text: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if text.is_empty() {
        Some(result.to_string())
    } else {
        Some(text.join("\n"))
    }
}

fn counts(b: TokenBreakdown) -> TokenCounts {
    TokenCounts {
        input_tokens: b.input_tokens,
        cached_input_tokens: b.cached_input_tokens,
        output_tokens: b.output_tokens,
        reasoning_output_tokens: b.reasoning_output_tokens,
        total_tokens: b.total_tokens,
    }
}

/// Cuts `text` to [`MAX_OUTPUT_BYTES`] on a character boundary; the flag says whether it cut.
fn truncated(mut text: String) -> (String, bool) {
    if text.len() <= MAX_OUTPUT_BYTES {
        return (text, false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

/// The raw provider message, unless it is too large to keep on every event.
fn bounded(value: Value) -> Option<Value> {
    // Serializing to measure is the price of an exact bound; payloads are small by far.
    let len = serde_json::to_vec(&value).map_or(usize::MAX, |bytes| bytes.len());
    (len <= MAX_PAYLOAD_BYTES).then_some(value)
}
