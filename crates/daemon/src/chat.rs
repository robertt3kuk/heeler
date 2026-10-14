//! Chat sessions (protocol 0.9 agents, 0.11 direct model chat): one adapter process per Codex
//! session or one [`DirectSession`] per direct chat, the approval engine, and the journal.
//!
//! The registry owns the sessions' records and lifecycle; [`ChatManager`] owns what runs: the
//! adapters, a pump task per session that numbers, journals and publishes every event, and the
//! pending approvals. Heeler's journal (`chat_items`) is the source of a conversation's
//! history; the provider's own copy is only used to continue the agent (ADR 0005).
//!
//! Approvals live here, in daemon state: they survive client disconnects, are answered only
//! with an option the agent offered, expire when their turn ends (or the agent goes away),
//! and every request and decision is written to the `approvals` table.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use heeler_agents::{
    AdapterMessage, AgentAdapter, AgentError, CodexAppServerAdapter, CodexConfig, CodexSession,
    ExitInfo, StartSpec, Started,
};
use heeler_core::agent::PermissionOutcome;
use heeler_core::{
    AgentEvent, AgentEventDraft, AgentEventKind, ApprovalId, ApprovalPolicy, ModelInfo,
    SandboxMode, SessionId, TurnId,
};
use heeler_protocol::chat::{
    ApprovalInfo, ApprovalList, ChatHistory, ChatHistoryRequest, ChatModels, HISTORY_MAX_LIMIT,
    SEND_MAX_BYTES,
};
use heeler_protocol::control::{ErrorBody, ErrorCode};
use heeler_protocol::messages::{ClientKind, Event};
use heeler_storage::{
    ApprovalOutcome, ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus, Storage,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::bus::EventBus;
use crate::context::Attached;
use crate::direct::{DirectSession, Target};
use crate::providers::ProviderService;
use crate::registry::{self, ChatNotifier};

/// The Codex app-server agent.
pub const AGENT_CODEX: &str = "codex";
/// The model gateway: no agent, a provider's chat API.
pub use heeler_protocol::assistant::AGENT_ASSISTANT;
pub use heeler_protocol::provider::AGENT_DIRECT;

/// Whether sessions of this agent are model gateway chats: a provider's API, no process.
pub fn is_gateway_agent(agent: &str) -> bool {
    agent == AGENT_DIRECT || agent == AGENT_ASSISTANT
}

/// How long a fetched model catalog is reused.
const MODEL_CACHE: Duration = Duration::from_secs(60);

/// What a chat session is, as stored in the session row's `spec_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSpec {
    pub agent: String,
    /// The model asked for; `None` takes the agent's catalog default.
    #[serde(default)]
    pub model: Option<String>,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxMode,
    /// The provider's conversation, once there is one: what a restart or a daemon restart
    /// resumes.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// `direct` only: the provider the conversation talks to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// `direct` only: sent to the model ahead of the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// What runs a session: an agent process, or the model gateway.
#[derive(Debug, Clone)]
enum Backend {
    Agent(Arc<dyn AgentAdapter>),
    Direct(Arc<DirectSession>),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
struct Live {
    id: SessionId,
    backend: Backend,
    /// The next event's `seq`.
    next_seq: AtomicU64,
    /// A turn is in progress, or `chat.send` is starting one.
    busy: AtomicBool,
    /// The turn the journal shows as open.
    open_turn: Mutex<Option<TurnId>>,
    /// The user (or a daemon shutdown) asked to end the session.
    stopping: AtomicBool,
}

#[derive(Debug)]
struct Pending {
    info: ApprovalInfo,
    decided_by: Option<&'static str>,
}

/// Runs the chat sessions of one daemon.
#[derive(Debug)]
pub struct ChatManager {
    bus: Arc<EventBus>,
    storage: Storage,
    notifier: ChatNotifier,
    codex: CodexConfig,
    providers: Arc<ProviderService>,
    max_context_chars: usize,
    live: Mutex<HashMap<SessionId, Arc<Live>>>,
    approvals: Mutex<HashMap<ApprovalId, Pending>>,
    models: Mutex<Option<(Instant, Vec<ModelInfo>)>>,
}

fn provider_error(error: &AgentError, program: &std::path::Path) -> ErrorBody {
    let hint = match error {
        AgentError::Spawn { .. } => format!(
            "; install Codex or set `bin` under `[codex]` in daemon.toml (looked for `{}`)",
            program.display()
        ),
        _ => String::new(),
    };
    ErrorBody::new(ErrorCode::ProviderError, format!("{error}{hint}"))
}

fn client_name(kind: Option<ClientKind>) -> &'static str {
    match kind {
        Some(ClientKind::Desktop) => "desktop",
        Some(ClientKind::Cli) => "cli",
        Some(ClientKind::Notifier) => "notifier",
        Some(ClientKind::Other) | None => "other",
    }
}

fn enum_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

impl ChatManager {
    pub(crate) fn new(
        bus: Arc<EventBus>,
        storage: Storage,
        notifier: ChatNotifier,
        codex: CodexConfig,
        providers: Arc<ProviderService>,
        max_context_chars: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            bus,
            storage,
            notifier,
            codex,
            providers,
            max_context_chars,
            live: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            models: Mutex::new(None),
        })
    }

    /// The model providers of this daemon.
    pub(crate) fn providers(&self) -> &Arc<ProviderService> {
        &self.providers
    }

    /// Whether the session has a running backend (not merely a record).
    pub(crate) fn is_running(&self, id: SessionId) -> bool {
        lock(&self.live).contains_key(&id)
    }

    /// The highest journaled `seq` of a session.
    async fn journal_end(&self, id: SessionId) -> Result<u64, ErrorBody> {
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || storage.chat().last_seq(id))
            .await
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))
    }

    /// Starts (or resumes, when `spec.thread_id` is set) the agent of a session and begins
    /// journaling its events.
    pub async fn start(
        self: &Arc<Self>,
        id: SessionId,
        cwd: PathBuf,
        spec: &ChatSpec,
    ) -> Result<Started, ErrorBody> {
        if is_gateway_agent(&spec.agent) {
            return self.start_direct(id, spec).await;
        }
        if spec.agent != AGENT_CODEX {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "unsupported agent `{}` (supported: {AGENT_CODEX}, {AGENT_DIRECT}, {AGENT_ASSISTANT})",
                    spec.agent
                ),
            ));
        }
        let last = self.journal_end(id).await?;
        let start = StartSpec {
            cwd,
            model: spec.model.clone(),
            approval_policy: spec.approval_policy,
            sandbox: spec.sandbox,
            resume_thread_id: spec.thread_id.clone(),
        };
        let CodexSession {
            adapter,
            started,
            events,
        } = CodexAppServerAdapter::start(&self.codex, &start)
            .await
            .map_err(|e| provider_error(&e, &self.codex.program))?;
        let live = Arc::new(Live {
            id,
            backend: Backend::Agent(adapter),
            next_seq: AtomicU64::new(last + 1),
            busy: AtomicBool::new(false),
            open_turn: Mutex::new(None),
            stopping: AtomicBool::new(false),
        });
        lock(&self.live).insert(id, live.clone());
        info!(session = %id, model = %started.model, resumed = started.resumed, "chat agent started");
        tokio::spawn(self.clone().pump(live, events));
        Ok(started)
    }

    /// Opens a direct chat: no process, just the session's target and its journal.
    async fn start_direct(
        self: &Arc<Self>,
        id: SessionId,
        spec: &ChatSpec,
    ) -> Result<Started, ErrorBody> {
        let (Some(provider_id), Some(model)) = (spec.provider_id.clone(), spec.model.clone())
        else {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "a direct chat needs a provider and a model",
            ));
        };
        let last = self.journal_end(id).await?;
        // The conversation's running token total continues from the newest `usage` event.
        let storage = self.storage.clone();
        let usage =
            tokio::task::spawn_blocking(move || storage.chat().items_of_kind(id, "usage", 1))
                .await
                .map_err(|e| {
                    ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}"))
                })?
                .map_err(|e| {
                    ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}"))
                })?
                .last()
                .and_then(|row| serde_json::from_str::<AgentEvent>(&row.event_json).ok())
                .and_then(|event| match event.kind {
                    AgentEventKind::Usage { total, .. } => total,
                    _ => None,
                })
                .unwrap_or_default();
        let (tx, events) = mpsc::channel(256);
        let session = DirectSession::new(
            id,
            self.storage.clone(),
            self.providers.clone(),
            tx,
            self.max_context_chars,
            Target {
                provider_id: provider_id.clone(),
                model: model.clone(),
                system_prompt: spec.system_prompt.clone(),
            },
            usage,
        );
        let live = Arc::new(Live {
            id,
            backend: Backend::Direct(session),
            next_seq: AtomicU64::new(last + 1),
            busy: AtomicBool::new(false),
            open_turn: Mutex::new(None),
            stopping: AtomicBool::new(false),
        });
        lock(&self.live).insert(id, live.clone());
        info!(session = %id, provider = %provider_id, %model, "direct chat started");
        tokio::spawn(self.clone().pump(live, events));
        Ok(Started {
            model,
            // There is no provider-side conversation: the session is its own thread.
            thread_id: id.to_string(),
            resumed: spec.thread_id.is_some(),
        })
    }

    /// Switches the model of a running direct chat; its next turn uses it.
    pub(crate) async fn set_model(
        &self,
        id: SessionId,
        provider_id: String,
        model: String,
    ) -> Result<(), ErrorBody> {
        let live = self.live(id)?;
        let Backend::Direct(session) = &live.backend else {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "only a direct chat has a switchable model",
            ));
        };
        session.set_model(provider_id, model).await;
        Ok(())
    }

    fn live(&self, id: SessionId) -> Result<Arc<Live>, ErrorBody> {
        lock(&self.live).get(&id).cloned().ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::InvalidState,
                format!("chat session {id} is not running"),
            )
        })
    }

    /// Asks the session's agent to end; the pump reports the exit.
    pub fn stop(&self, id: SessionId) {
        let Some(live) = lock(&self.live).get(&id).cloned() else {
            return;
        };
        live.stopping.store(true, Ordering::Release);
        tokio::spawn(async move {
            match &live.backend {
                Backend::Agent(adapter) => adapter.shutdown().await,
                Backend::Direct(session) => session.shutdown().await,
            }
        });
    }

    /// Sends the user's message; the turn's events follow on the bus.
    pub async fn send(
        &self,
        id: SessionId,
        text: String,
        attached: Option<Attached>,
    ) -> Result<TurnId, ErrorBody> {
        if text.trim().is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "the message is empty",
            ));
        }
        if text.len() > SEND_MAX_BYTES {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "the message is {} bytes; at most {SEND_MAX_BYTES} are allowed",
                    text.len()
                ),
            ));
        }
        let live = self.live(id)?;
        if live.busy.swap(true, Ordering::AcqRel) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "a turn is already in progress; wait for it or interrupt it",
            ));
        }
        let started = match &live.backend {
            Backend::Agent(_) if attached.is_some() => Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "attachments are only supported by direct and assistant chats",
            )),
            Backend::Agent(adapter) => adapter
                .send_message(text)
                .await
                .map(|turn| turn.turn_id)
                .map_err(|error| ErrorBody::new(ErrorCode::ProviderError, error.to_string())),
            Backend::Direct(session) => session.send(text, attached).await,
        };
        if started.is_err() {
            live.busy.store(false, Ordering::Release);
        }
        started
    }

    pub async fn interrupt(&self, id: SessionId) -> Result<(), ErrorBody> {
        let live = self.live(id)?;
        let adapter = match &live.backend {
            Backend::Agent(adapter) => adapter,
            Backend::Direct(session) => {
                return if session.interrupt() {
                    Ok(())
                } else {
                    Err(ErrorBody::new(
                        ErrorCode::InvalidState,
                        "no turn is in progress",
                    ))
                };
            }
        };
        match adapter.interrupt().await {
            Ok(()) => Ok(()),
            Err(AgentError::NoTurn) => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "no turn is in progress",
            )),
            Err(error) => Err(ErrorBody::new(ErrorCode::ProviderError, error.to_string())),
        }
    }

    // Approvals --------------------------------------------------------------------------------

    /// Permission requests waiting for an answer, oldest first.
    pub fn approval_list(&self, session: Option<SessionId>) -> ApprovalList {
        let mut approvals: Vec<ApprovalInfo> = lock(&self.approvals)
            .values()
            .map(|p| p.info.clone())
            .filter(|info| session.is_none_or(|s| s == info.session_id))
            .collect();
        approvals.sort_by(|a, b| {
            a.requested_at_ms
                .cmp(&b.requested_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        ApprovalList { approvals }
    }

    /// Answers a pending request with one of the options the agent offered.
    pub async fn respond(
        &self,
        approval: ApprovalId,
        option_id: String,
        client: Option<ClientKind>,
    ) -> Result<(), ErrorBody> {
        let session = {
            let mut approvals = lock(&self.approvals);
            let Some(pending) = approvals.get_mut(&approval) else {
                return Err(ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("approval {approval} is not pending (answered, or its turn ended)"),
                ));
            };
            if !pending.info.options.iter().any(|o| o.id == option_id) {
                let offered: Vec<&str> =
                    pending.info.options.iter().map(|o| o.id.as_str()).collect();
                let mut error = ErrorBody::new(
                    ErrorCode::OptionNotOffered,
                    format!(
                        "option `{option_id}` was not offered; offered: {}",
                        offered.join(", ")
                    ),
                );
                error.data = Some(serde_json::json!({ "offered": offered }));
                return Err(error);
            }
            pending.decided_by = Some(client_name(client));
            pending.info.session_id
        };
        let live = self.live(session)?;
        let Backend::Agent(adapter) = &live.backend else {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("approval {approval} is not pending"),
            ));
        };
        match adapter.respond(approval, option_id).await {
            Ok(()) => Ok(()),
            Err(AgentError::UnknownApproval) => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("approval {approval} was already answered"),
            )),
            Err(AgentError::OptionNotOffered { option }) => Err(ErrorBody::new(
                ErrorCode::OptionNotOffered,
                format!("option `{option}` was not offered"),
            )),
            Err(error) => Err(ErrorBody::new(ErrorCode::ProviderError, error.to_string())),
        }
    }

    // History and models -----------------------------------------------------------------------

    pub async fn history(&self, req: ChatHistoryRequest) -> Result<ChatHistory, ErrorBody> {
        let limit = req.limit.clamp(1, HISTORY_MAX_LIMIT);
        let live_last = lock(&self.live)
            .get(&req.session_id)
            .map(|live| live.next_seq.load(Ordering::Acquire).saturating_sub(1));
        let storage = self.storage.clone();
        let (mut rows, journaled) = tokio::task::spawn_blocking(move || {
            let rows =
                storage
                    .chat()
                    .items(req.session_id, req.before_seq, limit.saturating_add(1))?;
            let last = storage.chat().last_seq(req.session_id)?;
            Ok::<_, heeler_storage::StorageError>((rows, last))
        })
        .await
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?;
        let has_more = rows.len() > limit as usize;
        if has_more {
            rows.remove(0);
        }
        let events = rows
            .iter()
            .filter_map(|row| match serde_json::from_str::<AgentEvent>(&row.event_json) {
                Ok(event) => Some(event.replayed()),
                Err(error) => {
                    warn!(session = %row.session_id, seq = row.seq, %error, "unreadable journal row skipped");
                    None
                }
            })
            .collect();
        Ok(ChatHistory {
            events,
            has_more,
            last_seq: live_last.unwrap_or(journaled).max(journaled),
        })
    }

    pub async fn models(&self, agent: &str) -> Result<ChatModels, ErrorBody> {
        if is_gateway_agent(agent) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "a direct chat's models come from its provider: use `provider.models`",
            ));
        }
        if agent != AGENT_CODEX {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("unsupported agent `{agent}` (supported: {AGENT_CODEX})"),
            ));
        }
        if let Some((at, models)) = lock(&self.models).as_ref()
            && at.elapsed() < MODEL_CACHE
        {
            return Ok(ChatModels {
                agent: agent.to_owned(),
                models: models.clone(),
            });
        }
        let models = heeler_agents::codex::list_models(&self.codex)
            .await
            .map_err(|e| provider_error(&e, &self.codex.program))?;
        *lock(&self.models) = Some((Instant::now(), models.clone()));
        Ok(ChatModels {
            agent: agent.to_owned(),
            models,
        })
    }

    // Events -----------------------------------------------------------------------------------

    /// Numbers, journals and publishes the events of one session until its agent is gone.
    async fn pump(self: Arc<Self>, live: Arc<Live>, mut events: mpsc::Receiver<AdapterMessage>) {
        let mut exit = ExitInfo::default();
        while let Some(message) = events.recv().await {
            match message {
                AdapterMessage::Event(draft) => self.handle(&live, *draft).await,
                AdapterMessage::Node(update) => self.notifier.node(live.id, *update).await,
                AdapterMessage::Exited(info) => {
                    exit = info;
                    break;
                }
            }
        }
        self.finish(&live, exit).await;
    }

    /// The agent is gone: nothing pending can be answered, an open turn is over.
    async fn finish(&self, live: &Live, exit: ExitInfo) {
        let leftover: Vec<ApprovalId> = lock(&self.approvals)
            .values()
            .filter(|p| p.info.session_id == live.id)
            .map(|p| p.info.id)
            .collect();
        for approval in leftover {
            let draft = AgentEventDraft::new(AgentEventKind::PermissionResolved {
                approval_id: approval,
                outcome: PermissionOutcome::Expired,
            });
            self.handle(live, draft).await;
        }
        let open_turn = lock(&live.open_turn).take();
        if let Some(turn) = open_turn {
            let kind = if live.stopping.load(Ordering::Acquire) {
                AgentEventKind::TurnCancelled
            } else {
                AgentEventKind::TurnFailed {
                    message: "the agent exited during the turn".to_owned(),
                    details: (!exit.stderr_tail.is_empty()).then(|| exit.stderr_tail.clone()),
                }
            };
            let mut draft = AgentEventDraft::new(kind);
            draft.turn_id = Some(turn);
            // `handle` clears `busy` and tells the registry the turn ended.
            self.handle(live, draft).await;
        }
        lock(&self.live).remove(&live.id);
        info!(session = %live.id, code = ?exit.code, signal = ?exit.signal, "chat agent exited");
        self.notifier.exited(live.id, exit).await;
    }

    async fn handle(&self, live: &Live, draft: AgentEventDraft) {
        let seq = live.next_seq.fetch_add(1, Ordering::AcqRel);
        let ts_ms = registry::now_ms();
        let event = draft.into_event(live.id, seq, ts_ms);

        let mut turn_flag = None;
        let mut required: Option<ApprovalInfo> = None;
        let mut resolved: Option<(ApprovalId, PermissionOutcome)> = None;
        match &event.kind {
            AgentEventKind::TurnStarted => {
                *lock(&live.open_turn) = event.turn_id;
                self.write_turn(&event, ChatTurnStatus::Running, None);
                turn_flag = Some(true);
            }
            kind if kind.ends_turn() => {
                *lock(&live.open_turn) = None;
                live.busy.store(false, Ordering::Release);
                let (status, error) = match kind {
                    AgentEventKind::TurnCompleted { .. } => (ChatTurnStatus::Completed, None),
                    AgentEventKind::TurnFailed { message, .. } => {
                        (ChatTurnStatus::Failed, Some(message.clone()))
                    }
                    _ => (ChatTurnStatus::Cancelled, None),
                };
                self.write_turn(&event, status, error);
                turn_flag = Some(false);
            }
            AgentEventKind::PermissionRequested {
                approval_id,
                kind,
                title,
                detail,
                options,
            } => {
                let info = ApprovalInfo {
                    id: *approval_id,
                    session_id: live.id,
                    turn_id: event.turn_id,
                    kind: *kind,
                    title: title.clone(),
                    detail: detail.clone(),
                    options: options.clone(),
                    requested_at_ms: ts_ms,
                };
                self.write_approval_request(&info);
                lock(&self.approvals).insert(
                    *approval_id,
                    Pending {
                        info: info.clone(),
                        decided_by: None,
                    },
                );
                required = Some(info);
            }
            AgentEventKind::PermissionResolved {
                approval_id,
                outcome,
            } => {
                let pending = lock(&self.approvals).remove(approval_id);
                if let Some(pending) = pending {
                    self.write_approval_outcome(&pending, outcome, ts_ms);
                    resolved = Some((*approval_id, outcome.clone()));
                }
            }
            _ => {}
        }

        let approvals_changed = required.is_some() || resolved.is_some();
        if !event.kind.is_streaming() {
            self.journal(&event);
        }
        self.emit(&Event::ChatEvent {
            session_id: live.id,
            event: Box::new(event),
        });
        if let Some(info) = required {
            self.emit(&Event::ApprovalRequired {
                approval: Box::new(info),
            });
        }
        if let Some((approval_id, outcome)) = resolved {
            self.emit(&Event::ApprovalResolved {
                approval_id,
                session_id: live.id,
                outcome,
            });
        }
        // The session's state follows what happened: the registry is told after the events
        // are out, so a client that sees the state change has the events already.
        let pending = u32::try_from(
            lock(&self.approvals)
                .values()
                .filter(|p| p.info.session_id == live.id)
                .count(),
        )
        .unwrap_or(u32::MAX);
        if turn_flag.is_some() || approvals_changed {
            self.notifier.state(live.id, turn_flag, pending).await;
        }
    }

    fn emit(&self, event: &Event) {
        if let Err(error) = self.bus.publish(event) {
            error!(%error, "failed to publish a chat event");
        }
    }

    fn journal(&self, event: &AgentEvent) {
        let event_json = match serde_json::to_string(event) {
            Ok(json) => json,
            Err(error) => {
                error!(session = %event.session_id, %error, "could not encode a chat event");
                return;
            }
        };
        let row = ChatItemRow {
            session_id: event.session_id,
            seq: event.seq,
            ts_ms: event.ts_ms,
            turn_id: event.turn_id.map(|t| t.to_string()),
            item_id: event.item_id.map(|i| i.to_string()),
            kind: event.kind.name().to_owned(),
            event_json,
        };
        registry::log_write(self.storage.chat().append(row), "a chat event");
    }

    fn write_turn(&self, event: &AgentEvent, status: ChatTurnStatus, error: Option<String>) {
        let Some(turn) = event.turn_id else { return };
        let running = status == ChatTurnStatus::Running;
        let row = ChatTurnRow {
            session_id: event.session_id,
            turn_id: turn.to_string(),
            provider_turn_id: event.provider.turn_id.clone(),
            status,
            started_at_ms: event.ts_ms,
            ended_at_ms: (!running).then_some(event.ts_ms),
            error,
        };
        registry::log_write(self.storage.chat().upsert_turn(row), "a chat turn");
    }

    fn write_approval_request(&self, info: &ApprovalInfo) {
        let options_json = serde_json::to_string(&info.options).unwrap_or_else(|error| {
            error!(approval = %info.id, %error, "could not encode approval options");
            "[]".to_owned()
        });
        let row = ApprovalRow {
            id: info.id,
            session_id: info.session_id,
            turn_id: info.turn_id.map(|t| t.to_string()),
            kind: enum_name(&info.kind),
            title: info.title.clone(),
            options_json,
            requested_at_ms: info.requested_at_ms,
            resolved_at_ms: None,
            outcome: None,
            option_id: None,
            decided_by: None,
        };
        registry::log_write(self.storage.approvals().upsert(row), "an approval request");
    }

    fn write_approval_outcome(&self, pending: &Pending, outcome: &PermissionOutcome, at_ms: u64) {
        let info = &pending.info;
        let options_json = serde_json::to_string(&info.options).unwrap_or_else(|_| "[]".to_owned());
        let (outcome, option_id, decided_by) = match outcome {
            PermissionOutcome::Selected { option_id } => (
                ApprovalOutcome::Selected,
                Some(option_id.clone()),
                Some(pending.decided_by.unwrap_or("other").to_owned()),
            ),
            PermissionOutcome::Expired => (ApprovalOutcome::Expired, None, None),
        };
        let row = ApprovalRow {
            id: info.id,
            session_id: info.session_id,
            turn_id: info.turn_id.map(|t| t.to_string()),
            kind: enum_name(&info.kind),
            title: info.title.clone(),
            options_json,
            requested_at_ms: info.requested_at_ms,
            resolved_at_ms: Some(at_ms),
            outcome: Some(outcome),
            option_id,
            decided_by,
        };
        registry::log_write(self.storage.approvals().upsert(row), "an approval decision");
    }
}

/// Marks approvals a crashed daemon left open as expired (their agents are gone).
pub fn expire_dangling_approvals(storage: &Storage) -> Result<(), heeler_storage::StorageError> {
    let now = registry::now_ms();
    for mut row in storage.approvals().unresolved()? {
        row.resolved_at_ms = Some(now);
        row.outcome = Some(ApprovalOutcome::Expired);
        storage.approvals().upsert(row)?;
    }
    debug!("dangling approvals expired");
    Ok(())
}
