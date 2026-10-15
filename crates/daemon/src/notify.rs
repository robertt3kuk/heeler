//! Notification intents (protocol 0.13, capability `notify.intents`): the daemon decides *that*
//! something deserves the user's attention, clients decide how to show it.
//!
//! Intents are derived from the event stream, never from polling:
//!
//! | kind | raised when |
//! |---|---|
//! | `approval_required` | `approval.required`; answered by `notify.act` through `approval.respond` |
//! | `agent_blocked` | a node of a session's agent tree becomes `blocked` |
//! | `agent_finished` | the main agent goes `working` → `idle` with outcome `completed` after `min_turn_secs` |
//! | `session_exited_with_error` | a session's process exits with a non-zero code |
//! | `chat_turn_finished` | a chat turn completes while no client has the session focused |
//!
//! Every candidate passes the same gate: the `[notify]` settings (`enabled`, `kinds`), dedup by
//! `(kind, session, node)` against the pending intents, and a rate limit of one intent per
//! session per [`RATE_LIMIT_MS`] (approvals are exempt from it and never counted by it). Pending
//! intents are kept in memory, at most [`MAX_PENDING`]; they are lost with the daemon, which is
//! right for something that says "look now". An intent leaves the list when it is acknowledged,
//! acted on, expires, or (approvals) is resolved.
//!
//! [`Core`] is the whole decision logic, synchronous and driven by explicit timestamps, so its
//! rules are unit-tested without a clock; [`NotifyService`] feeds it from the event bus and
//! answers the requests.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use heeler_core::agent::{Delivery, MessageRole};
use heeler_core::{
    AgentEventKind, AgentNode, ApprovalId, Liveness, NodeKind, NodeOutcome, NodeState, SessionId,
    WorkspaceId,
};
use heeler_protocol::chat::ApprovalInfo;
use heeler_protocol::control::{ErrorBody, ErrorCode};
use heeler_protocol::messages::{ClientKind, Event, SessionListFilter};
use heeler_protocol::notify::{
    ACTION_OPEN, ActRoute, BODY_MAX_CHARS, IntentAction, IntentKind, NotificationIntent, NotifyAct,
    NotifyActed, TITLE_MAX_CHARS,
};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use ulid::Ulid;

use crate::bus::EventBus;
use crate::config::NotifySettings;
use crate::conn::ConnId;
use crate::registry::{RegistryHandle, now_ms};

/// Shortest time between two intents of one session (approvals excepted).
pub const RATE_LIMIT_MS: u64 = 10_000;
/// Most pending intents; the oldest is dropped to make room.
pub const MAX_PENDING: usize = 100;
/// Queue between the event bus and the service task.
pub const OBSERVER_QUEUE: usize = 1024;
/// Per-node memory of the agent trees is dropped when it grows past this many entries (a safety
/// net: trees are bounded and report their removals).
const MAX_MEMOS: usize = 4096;

/// What the session is called in an intent, and where it lives.
#[derive(Debug, Clone, Default)]
struct SessionMeta {
    title: Option<String>,
    workspace_id: Option<WorkspaceId>,
}

/// Something that may become an intent: everything but the session's title, which is looked up
/// asynchronously between planning and offering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    kind: IntentKind,
    session: SessionId,
    /// The third part of the dedup key: an approval, node or turn id.
    node: String,
    /// Said after the session's title: `"{title}: {headline}"`.
    headline: String,
    body: String,
    actions: Vec<IntentAction>,
    approval: Option<ApprovalId>,
}

#[derive(Debug, Clone)]
struct Entry {
    intent: NotificationIntent,
    /// The dedup key's node part.
    node: String,
    approval: Option<ApprovalId>,
}

/// What is remembered of an agent tree node between two `agent.tree_updated` events.
#[derive(Debug, Clone, Copy)]
struct NodeMemo {
    state: NodeState,
    /// When the node was first seen working in the turn that is going on.
    working_since: Option<u64>,
}

/// The decision logic: settings, pending intents, per-session rate limits, node memory, focus.
#[derive(Debug)]
struct Core {
    settings: NotifySettings,
    pending: VecDeque<Entry>,
    last_raised: HashMap<SessionId, u64>,
    nodes: HashMap<(SessionId, String), NodeMemo>,
    /// The latest finished assistant message of each chat, already redacted and bounded.
    last_message: HashMap<SessionId, String>,
    focus: HashMap<ConnId, SessionId>,
}

impl Core {
    fn new(settings: NotifySettings) -> Self {
        Self {
            settings,
            pending: VecDeque::new(),
            last_raised: HashMap::new(),
            nodes: HashMap::new(),
            last_message: HashMap::new(),
            focus: HashMap::new(),
        }
    }

    // Planning --------------------------------------------------------------------------------

    /// Folds one event in and returns what it may raise.
    fn plan(&mut self, event: &Event, now_ms: u64) -> Vec<Candidate> {
        if !self.settings.enabled {
            return Vec::new();
        }
        match event {
            Event::ApprovalRequired { approval } => vec![approval_candidate(approval)],
            Event::ApprovalResolved { approval_id, .. } => {
                self.pending.retain(|e| e.approval != Some(*approval_id));
                Vec::new()
            }
            Event::AgentTreeUpdated {
                session_id,
                nodes_changed,
                nodes_removed,
            } => self.plan_tree(*session_id, nodes_changed, nodes_removed, now_ms),
            Event::SessionExited {
                session_id, code, ..
            } => {
                self.forget_session(*session_id);
                match code {
                    Some(code) if *code != 0 => vec![Candidate {
                        kind: IntentKind::SessionExitedWithError,
                        session: *session_id,
                        node: "exit".to_owned(),
                        headline: "exited with an error".to_owned(),
                        body: format!("The process exited with code {code}."),
                        actions: vec![open_action()],
                        approval: None,
                    }],
                    _ => Vec::new(),
                }
            }
            Event::ChatEvent { session_id, event } if event.delivery == Delivery::Live => {
                match &event.kind {
                    AgentEventKind::TurnStarted => {
                        self.last_message.remove(session_id);
                        Vec::new()
                    }
                    AgentEventKind::MessageEnd {
                        role: MessageRole::Assistant,
                        text,
                        ..
                    } => {
                        self.last_message
                            .insert(*session_id, clean(text, BODY_MAX_CHARS));
                        Vec::new()
                    }
                    AgentEventKind::TurnCompleted { .. } => {
                        let body = self
                            .last_message
                            .remove(session_id)
                            .filter(|text| !text.is_empty())
                            .unwrap_or_else(|| "The turn finished.".to_owned());
                        let node = event
                            .turn_id
                            .map_or_else(|| format!("seq-{}", event.seq), |turn| turn.to_string());
                        vec![Candidate {
                            kind: IntentKind::ChatTurnFinished,
                            session: *session_id,
                            node,
                            headline: "reply ready".to_owned(),
                            body,
                            actions: vec![open_action()],
                            approval: None,
                        }]
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn plan_tree(
        &mut self,
        session: SessionId,
        changed: &[AgentNode],
        removed: &[String],
        now_ms: u64,
    ) -> Vec<Candidate> {
        let min_turn_ms = self.settings.min_turn_secs.saturating_mul(1000);
        let mut raised = Vec::new();
        for node in changed {
            let key = (session, node.node_id.clone());
            let before = self.nodes.get(&key).copied();
            let is_root = node.kind == NodeKind::Main && node.parent.is_none();
            let who = if is_root {
                node.agent.as_str()
            } else {
                node.label()
            };
            // A turn runs from the first `working` to the next `idle`, blocked stretches
            // included.
            let working_since = match node.state {
                NodeState::Working => Some(before.and_then(|b| b.working_since).unwrap_or(now_ms)),
                NodeState::Blocked => before.and_then(|b| b.working_since),
                NodeState::Idle | NodeState::Unknown => None,
            };
            if node.state == NodeState::Blocked
                && node.liveness != Liveness::Gone
                && before.map(|b| b.state) != Some(NodeState::Blocked)
            {
                raised.push(Candidate {
                    kind: IntentKind::AgentBlocked,
                    session,
                    node: node.node_id.clone(),
                    headline: format!("{who} is waiting for you"),
                    body: node
                        .activity
                        .as_deref()
                        .map(|activity| clean(activity, BODY_MAX_CHARS))
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| "It needs a decision from you.".to_owned()),
                    actions: vec![open_action()],
                    approval: None,
                });
            }
            // A normal stop is a finish: a plain idle (the Stop hooks, OMP `agent_end`) or an
            // explicit `completed`. Failures, interruptions and lost contact are not. Heeler's own
            // Codex chats announce their turns as `chat_turn_finished` instead.
            if is_root
                && node.state == NodeState::Idle
                && matches!(node.outcome, None | Some(NodeOutcome::Completed))
                && node.source != heeler_agents::codex::TREE_SOURCE
                && let Some(since) = before.and_then(|b| b.working_since)
                && now_ms.saturating_sub(since) >= min_turn_ms
            {
                raised.push(Candidate {
                    kind: IntentKind::AgentFinished,
                    session,
                    node: node.node_id.clone(),
                    headline: format!("{who} finished"),
                    body: format!(
                        "Finished after {}.",
                        human_duration(now_ms.saturating_sub(since))
                    ),
                    actions: vec![open_action()],
                    approval: None,
                });
            }
            self.nodes.insert(
                key,
                NodeMemo {
                    state: node.state,
                    working_since,
                },
            );
        }
        for id in removed {
            self.nodes.remove(&(session, id.clone()));
        }
        if self.nodes.len() > MAX_MEMOS {
            self.nodes.clear();
        }
        raised
    }

    /// A session ended: what is remembered about its agents and chat is of no use any more.
    fn forget_session(&mut self, session: SessionId) {
        self.nodes.retain(|(s, _), _| *s != session);
        self.last_message.remove(&session);
    }

    // Offering --------------------------------------------------------------------------------

    /// Applies the gate to a candidate; the intent when it passes (it is then pending).
    fn offer(
        &mut self,
        candidate: Candidate,
        meta: &SessionMeta,
        now_ms: u64,
    ) -> Option<NotificationIntent> {
        let settings = &self.settings;
        if !settings.enabled || !settings.kinds.contains(&candidate.kind) {
            return None;
        }
        self.expire(now_ms);
        let approval = candidate.kind == IntentKind::ApprovalRequired;
        if candidate.kind == IntentKind::ChatTurnFinished
            && self.focus.values().any(|s| *s == candidate.session)
        {
            debug!(session = %candidate.session, "chat turn finished in a focused session; no intent");
            return None;
        }
        if self.pending.iter().any(|e| {
            e.intent.kind == candidate.kind
                && e.intent.session_id == candidate.session
                && e.node == candidate.node
        }) {
            debug!(kind = candidate.kind.as_str(), session = %candidate.session, "duplicate intent dropped");
            return None;
        }
        if !approval
            && self
                .last_raised
                .get(&candidate.session)
                .is_some_and(|last| now_ms.saturating_sub(*last) < RATE_LIMIT_MS)
        {
            debug!(kind = candidate.kind.as_str(), session = %candidate.session, "intent rate-limited");
            return None;
        }
        let title = clean(
            &format!(
                "{}: {}",
                meta.title.as_deref().unwrap_or("Session"),
                candidate.headline
            ),
            TITLE_MAX_CHARS,
        );
        let expires_at_ms = (!approval)
            .then(|| now_ms.saturating_add(self.settings.expire_secs.saturating_mul(1000)));
        let intent = NotificationIntent {
            id: Ulid::generate().to_string(),
            kind: candidate.kind,
            session_id: candidate.session,
            workspace_id: meta.workspace_id,
            title,
            body: clean(&candidate.body, BODY_MAX_CHARS),
            actions: candidate.actions,
            created_at_ms: now_ms,
            expires_at_ms,
        };
        if !approval {
            self.last_raised.insert(candidate.session, now_ms);
            if self.last_raised.len() > MAX_MEMOS {
                self.last_raised
                    .retain(|_, last| now_ms.saturating_sub(*last) < RATE_LIMIT_MS);
            }
        }
        self.pending.push_back(Entry {
            intent: intent.clone(),
            node: candidate.node,
            approval: candidate.approval,
        });
        while self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
        }
        Some(intent)
    }

    // Pending intents -------------------------------------------------------------------------

    fn expire(&mut self, now_ms: u64) {
        self.pending
            .retain(|e| e.intent.expires_at_ms.is_none_or(|at| at > now_ms));
    }

    fn list(&mut self, now_ms: u64) -> Vec<NotificationIntent> {
        self.expire(now_ms);
        self.pending.iter().map(|e| e.intent.clone()).collect()
    }

    fn find(&mut self, id: &str, now_ms: u64) -> Option<Entry> {
        self.expire(now_ms);
        self.pending.iter().find(|e| e.intent.id == id).cloned()
    }

    /// Removes the intent; whether it was pending.
    fn remove(&mut self, id: &str) -> bool {
        let before = self.pending.len();
        self.pending.retain(|e| e.intent.id != id);
        self.pending.len() != before
    }
}

fn open_action() -> IntentAction {
    IntentAction {
        id: ACTION_OPEN.to_owned(),
        label: "Open".to_owned(),
    }
}

fn approval_candidate(approval: &ApprovalInfo) -> Candidate {
    let body = match approval.detail.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(detail) => format!("{}: {detail}", approval.title),
        None => approval.title.clone(),
    };
    Candidate {
        kind: IntentKind::ApprovalRequired,
        session: approval.session_id,
        node: approval.id.to_string(),
        headline: "approval needed".to_owned(),
        body,
        actions: approval
            .options
            .iter()
            .map(|option| IntentAction {
                id: option.id.clone(),
                label: option.label.clone(),
            })
            .collect(),
        approval: Some(approval.id),
    }
}

/// `text` as one redacted line of at most `max_chars` characters; a cut ends in `…`. Redaction
/// comes first and sees the whole text: cutting first could clip a quoted secret before its
/// closing quote, or a URL password before its `@`, and the prefix would then pass the masks.
fn clean(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let redacted = heeler_index::redact::redact(text);
    let mut words = redacted.split_whitespace();
    let Some(first) = words.next() else {
        return String::new();
    };
    let mut chars = first
        .chars()
        .chain(words.flat_map(|word| std::iter::once(' ').chain(word.chars())));
    // One bounded UTF-8 buffer, including room for an ellipsis; do not normalize the whole reply.
    let capacity = max_chars
        .saturating_mul(4)
        .min(redacted.len().saturating_add(2));
    let mut line = String::with_capacity(capacity);
    line.extend(chars.by_ref().take(max_chars));
    if chars.next().is_some() {
        line.pop();
        line.push('…');
    }
    line
}

fn human_duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

/// Feeds [`Core`] from the event bus and answers the `notify.*` requests.
#[derive(Debug)]
pub struct NotifyService {
    core: Mutex<Core>,
    bus: Arc<EventBus>,
    registry: RegistryHandle,
}

impl NotifyService {
    pub fn new(
        settings: NotifySettings,
        bus: Arc<EventBus>,
        registry: RegistryHandle,
    ) -> Arc<Self> {
        Arc::new(Self {
            core: Mutex::new(Core::new(settings)),
            bus,
            registry,
        })
    }

    /// Starts watching the event bus until `shutdown`.
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) -> JoinHandle<()> {
        let (tx, mut rx) = mpsc::channel(OBSERVER_QUEUE);
        self.bus.set_observer(Some(tx));
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    event = rx.recv() => match event {
                        Some(event) => this.handle(event).await,
                        None => break,
                    },
                }
            }
            this.bus.set_observer(None);
        })
    }

    async fn handle(&self, event: Event) {
        let candidates = self.core.lock().plan(&event, now_ms());
        for candidate in candidates {
            let meta = self.meta(candidate.session).await;
            let raised = self.core.lock().offer(candidate, &meta, now_ms());
            if let Some(intent) = raised
                && let Err(error) = self.bus.publish(&Event::NotifyIntent { intent })
            {
                warn!(%error, "could not publish a notification intent");
            }
        }
    }

    async fn meta(&self, session: SessionId) -> SessionMeta {
        match self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        {
            Ok(list) => list
                .sessions
                .into_iter()
                .find(|s| s.id == session)
                .map(|s| SessionMeta {
                    title: Some(s.title),
                    workspace_id: Some(s.workspace_id),
                })
                .unwrap_or_default(),
            Err(error) => {
                debug!(message = %error.message, "session lookup for an intent failed");
                SessionMeta::default()
            }
        }
    }

    /// `notify.list`.
    pub fn list(&self) -> Vec<NotificationIntent> {
        self.core.lock().list(now_ms())
    }

    /// `notify.ack`.
    pub fn ack(&self, id: &str) -> Result<(), ErrorBody> {
        let mut core = self.core.lock();
        core.expire(now_ms());
        if core.remove(id) {
            Ok(())
        } else {
            Err(not_found(id))
        }
    }

    /// `notify.act`.
    pub async fn act(
        &self,
        client: Option<ClientKind>,
        req: NotifyAct,
    ) -> Result<NotifyActed, ErrorBody> {
        let entry = self
            .core
            .lock()
            .find(&req.id, now_ms())
            .ok_or_else(|| not_found(&req.id))?;
        if !entry.intent.actions.iter().any(|a| a.id == req.action_id) {
            let offered: Vec<&str> = entry.intent.actions.iter().map(|a| a.id.as_str()).collect();
            let mut error = ErrorBody::new(
                ErrorCode::OptionNotOffered,
                format!(
                    "action `{}` is not offered; offered: {}",
                    req.action_id,
                    offered.join(", ")
                ),
            );
            error.data = Some(serde_json::json!({ "offered": offered }));
            return Err(error);
        }
        match entry.approval {
            Some(approval) => {
                let answered = self
                    .registry
                    .chats()
                    .respond(approval, req.action_id, client)
                    .await;
                match answered {
                    Ok(()) => {}
                    // The approval is gone (answered elsewhere, its turn ended): the intent is
                    // stale, whatever the caller does about the error.
                    Err(error) if error.code == ErrorCode::NotFound => {
                        self.core.lock().remove(&req.id);
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                }
                self.core.lock().remove(&req.id);
                Ok(NotifyActed {
                    route: ActRoute::Approval,
                    desktop_clients: 0,
                })
            }
            None => {
                self.core.lock().remove(&req.id);
                let reached = self.bus.subscriber_count_of(ClientKind::Desktop);
                self.bus
                    .publish(&Event::UiOpen {
                        session_id: entry.intent.session_id,
                        workspace_id: entry.intent.workspace_id,
                        at_ms: now_ms(),
                    })
                    .map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))?;
                Ok(NotifyActed {
                    route: ActRoute::UiOpen,
                    desktop_clients: u32::try_from(reached).unwrap_or(u32::MAX),
                })
            }
        }
    }

    /// `client.focus`.
    pub fn set_focus(&self, conn: ConnId, session: Option<SessionId>) {
        let mut core = self.core.lock();
        match session {
            Some(session) => {
                core.focus.insert(conn, session);
            }
            None => {
                core.focus.remove(&conn);
            }
        }
    }

    /// A connection closed: its focus goes with it.
    pub fn connection_closed(&self, conn: ConnId) {
        self.core.lock().focus.remove(&conn);
    }
}

fn not_found(id: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::NotFound,
        format!("notification {id} is not pending (acknowledged, expired, or never raised)"),
    )
}

#[cfg(test)]
mod tests {
    use heeler_core::agent::{PermissionKind, PermissionOption, PermissionOptionKind};
    use heeler_core::{AgentEvent, AgentEventDraft, TurnId};

    use super::*;

    fn core() -> Core {
        Core::new(NotifySettings {
            min_turn_secs: 20,
            ..NotifySettings::default()
        })
    }

    fn meta() -> SessionMeta {
        SessionMeta {
            title: Some("api".to_owned()),
            workspace_id: Some(WorkspaceId::new()),
        }
    }

    fn node(id: &str, kind: NodeKind, state: NodeState) -> AgentNode {
        AgentNode {
            node_id: id.to_owned(),
            parent: (kind == NodeKind::Sub).then(|| "root".to_owned()),
            kind,
            source: "claude-hooks".to_owned(),
            agent: "claude".to_owned(),
            name: None,
            agent_type: None,
            state,
            outcome: None,
            liveness: Liveness::Alive,
            model: None,
            started_at_ms: Some(1),
            ended_at_ms: None,
            last_activity_ms: 1,
            activity: None,
            usage: None,
            transcript_path: None,
        }
    }

    /// A node that finished its turn as the daemon reports it: idle, outcome `completed`.
    fn completed(id: &str, kind: NodeKind) -> AgentNode {
        AgentNode {
            outcome: Some(NodeOutcome::Completed),
            ..node(id, kind, NodeState::Idle)
        }
    }

    fn tree(session: SessionId, nodes: Vec<AgentNode>) -> Event {
        Event::AgentTreeUpdated {
            session_id: session,
            nodes_changed: nodes,
            nodes_removed: Vec::new(),
        }
    }

    fn approval(session: SessionId) -> ApprovalInfo {
        ApprovalInfo {
            id: ApprovalId::new(),
            session_id: session,
            turn_id: None,
            kind: PermissionKind::Command,
            title: "Run `rm -rf build`?".to_owned(),
            detail: Some("token=abcdef123456789 in the env".to_owned()),
            options: vec![
                PermissionOption {
                    id: "accept".to_owned(),
                    label: "Allow once".to_owned(),
                    kind: PermissionOptionKind::AllowOnce,
                    provider: None,
                },
                PermissionOption {
                    id: "cancel".to_owned(),
                    label: "Cancel".to_owned(),
                    kind: PermissionOptionKind::Cancel,
                    provider: None,
                },
            ],
            requested_at_ms: 1,
        }
    }

    fn chat_event(session: SessionId, kind: AgentEventKind, seq: u64) -> Event {
        let mut draft = AgentEventDraft::new(kind);
        draft.turn_id = Some(TurnId::new());
        Event::ChatEvent {
            session_id: session,
            event: Box::new(draft.into_event(session, seq, 1)),
        }
    }

    fn turn_completed(session: SessionId, seq: u64) -> Event {
        chat_event(
            session,
            AgentEventKind::TurnCompleted { duration_ms: None },
            seq,
        )
    }

    /// Plans `event` and offers every candidate; the intents raised.
    fn raise(core: &mut Core, event: &Event, now: u64) -> Vec<NotificationIntent> {
        let meta = meta();
        core.plan(event, now)
            .into_iter()
            .filter_map(|c| core.offer(c, &meta, now))
            .collect()
    }

    #[test]
    fn an_approval_becomes_an_intent_with_its_options_as_actions() {
        let mut core = core();
        let session = SessionId::new();
        let info = approval(session);
        let raised = raise(
            &mut core,
            &Event::ApprovalRequired {
                approval: Box::new(info.clone()),
            },
            1_000,
        );
        let [intent] = raised.as_slice() else {
            panic!("one intent expected, got {raised:?}");
        };
        assert_eq!(intent.kind, IntentKind::ApprovalRequired);
        assert_eq!(intent.session_id, session);
        assert_eq!(intent.title, "api: approval needed");
        let ids: Vec<_> = intent.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["accept", "cancel"]);
        assert_eq!(intent.expires_at_ms, None, "lives as long as the approval");
        assert!(
            !intent.body.contains("abcdef123456789"),
            "secrets are redacted: {}",
            intent.body
        );
        assert!(intent.body.starts_with("Run `rm -rf build`?"));
    }

    #[test]
    fn resolving_an_approval_withdraws_its_intent() {
        let mut core = core();
        let info = approval(SessionId::new());
        let session = info.session_id;
        raise(
            &mut core,
            &Event::ApprovalRequired {
                approval: Box::new(info.clone()),
            },
            1_000,
        );
        assert_eq!(core.list(1_001).len(), 1);
        raise(
            &mut core,
            &Event::ApprovalResolved {
                approval_id: info.id,
                session_id: session,
                outcome: heeler_core::agent::PermissionOutcome::Expired,
            },
            1_002,
        );
        assert!(core.list(1_003).is_empty());
    }

    #[test]
    fn a_node_becoming_blocked_raises_once_until_it_is_unblocked() {
        let mut core = core();
        let session = SessionId::new();
        let mut blocked = node("agent-1", NodeKind::Sub, NodeState::Blocked);
        blocked.name = Some("Explore".to_owned());
        blocked.activity = Some("wants to run npm install".to_owned());

        let first = raise(&mut core, &tree(session, vec![blocked.clone()]), 1_000);
        let [intent] = first.as_slice() else {
            panic!("expected one intent, got {first:?}");
        };
        assert_eq!(intent.kind, IntentKind::AgentBlocked);
        assert_eq!(intent.title, "api: Explore is waiting for you");
        assert_eq!(intent.body, "wants to run npm install");
        assert_eq!(intent.actions[0].id, ACTION_OPEN);

        // Still blocked (only the activity changed): not a new transition.
        blocked.activity = Some("still waiting".to_owned());
        assert!(raise(&mut core, &tree(session, vec![blocked.clone()]), 30_000).is_empty());

        // Unblocked, then blocked again after the rate limit and after the first was seen.
        let working = node("agent-1", NodeKind::Sub, NodeState::Working);
        assert!(raise(&mut core, &tree(session, vec![working]), 31_000).is_empty());
        core.remove(&intent.id);
        assert_eq!(
            raise(&mut core, &tree(session, vec![blocked]), 60_000).len(),
            1
        );
    }

    #[test]
    fn a_gone_node_that_is_blocked_raises_nothing() {
        let mut core = core();
        let mut gone = node("a", NodeKind::Sub, NodeState::Blocked);
        gone.liveness = Liveness::Gone;
        assert!(raise(&mut core, &tree(SessionId::new(), vec![gone]), 1_000).is_empty());
    }

    #[test]
    fn the_main_agent_finishing_a_long_enough_turn_raises_agent_finished() {
        let mut core = core();
        let session = SessionId::new();
        let working = node("root", NodeKind::Main, NodeState::Working);
        assert!(raise(&mut core, &tree(session, vec![working.clone()]), 1_000).is_empty());
        // Still working later: nothing, and the start of the turn is kept.
        assert!(raise(&mut core, &tree(session, vec![working]), 10_000).is_empty());
        let idle = completed("root", NodeKind::Main);
        let raised = raise(&mut core, &tree(session, vec![idle]), 61_000);
        let [intent] = raised.as_slice() else {
            panic!("expected one intent, got {raised:?}");
        };
        assert_eq!(intent.kind, IntentKind::AgentFinished);
        assert_eq!(intent.title, "api: claude finished");
        assert_eq!(intent.body, "Finished after 1m 0s.");
    }

    #[test]
    fn a_plain_idle_after_a_long_turn_is_a_finish_but_not_in_a_codex_chat() {
        // The Stop hooks and OMP report a normal stop as a plain idle: that is a finish.
        let mut core = core();
        let session = SessionId::new();
        let working = node("root", NodeKind::Main, NodeState::Working);
        raise(&mut core, &tree(session, vec![working.clone()]), 1_000);
        let idle = node("root", NodeKind::Main, NodeState::Idle);
        assert_eq!(idle.outcome, None);
        let raised = raise(&mut core, &tree(session, vec![idle.clone()]), 600_000);
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].kind, IntentKind::AgentFinished);

        // A Heeler Codex chat raises `chat_turn_finished` for the same turn, not this.
        let mut core = self::core();
        let chat = SessionId::new();
        let mut working = working;
        working.source = heeler_agents::codex::TREE_SOURCE.to_owned();
        let mut idle = idle;
        idle.source = working.source.clone();
        raise(&mut core, &tree(chat, vec![working]), 1_000);
        assert!(raise(&mut core, &tree(chat, vec![idle]), 600_000).is_empty());
    }

    #[test]
    fn a_short_turn_a_subagent_or_a_failure_is_not_a_finish() {
        let session = SessionId::new();
        // Under min_turn_secs.
        let mut core = core();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        let idle = completed("root", NodeKind::Main);
        assert!(raise(&mut core, &tree(session, vec![idle]), 20_999).is_empty());

        // Exactly min_turn_secs counts.
        let mut core = self::core();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        let idle = completed("root", NodeKind::Main);
        assert_eq!(
            raise(&mut core, &tree(session, vec![idle]), 21_000).len(),
            1
        );

        // A subagent finishing, however long it took.
        let mut core = self::core();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("sub", NodeKind::Sub, NodeState::Working)],
            ),
            1_000,
        );
        let idle = completed("sub", NodeKind::Sub);
        assert!(raise(&mut core, &tree(session, vec![idle]), 600_000).is_empty());

        // The main agent failing, being interrupted, or losing contact.
        for outcome in [
            NodeOutcome::Failed,
            NodeOutcome::Interrupted,
            NodeOutcome::Disconnected,
        ] {
            let mut core = self::core();
            raise(
                &mut core,
                &tree(
                    session,
                    vec![node("root", NodeKind::Main, NodeState::Working)],
                ),
                1_000,
            );
            let mut idle = node("root", NodeKind::Main, NodeState::Idle);
            idle.outcome = Some(outcome);
            assert!(
                raise(&mut core, &tree(session, vec![idle]), 600_000).is_empty(),
                "{outcome:?}"
            );
        }

        // An explicit `completed` outcome is a finish.
        let mut core = self::core();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        let mut done = node("root", NodeKind::Main, NodeState::Idle);
        done.outcome = Some(NodeOutcome::Completed);
        assert_eq!(
            raise(&mut core, &tree(session, vec![done]), 600_000).len(),
            1
        );
    }

    #[test]
    fn a_blocked_stretch_does_not_restart_the_turn() {
        let mut core = core();
        let session = SessionId::new();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        // Blocked raises its own intent; the rate limit and the pending intent are out of the way
        // by the time the turn ends.
        let blocked = node("root", NodeKind::Main, NodeState::Blocked);
        assert_eq!(
            raise(&mut core, &tree(session, vec![blocked]), 5_000).len(),
            1
        );
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            6_000,
        );
        let idle = completed("root", NodeKind::Main);
        let raised = raise(&mut core, &tree(session, vec![idle]), 300_000);
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert_eq!(raised[0].kind, IntentKind::AgentFinished);
        assert_eq!(raised[0].body, "Finished after 4m 59s.");
    }

    #[test]
    fn a_non_zero_exit_raises_and_a_clean_exit_or_signal_does_not() {
        let mut core = core();
        let session = SessionId::new();
        let exited = |code, signal| Event::SessionExited {
            session_id: session,
            code,
            signal,
        };
        assert!(raise(&mut core, &exited(Some(0), None), 1_000).is_empty());
        assert!(raise(&mut core, &exited(None, Some(1)), 1_000).is_empty());
        let raised = raise(&mut core, &exited(Some(2), None), 1_000);
        let [intent] = raised.as_slice() else {
            panic!("expected one intent, got {raised:?}");
        };
        assert_eq!(intent.kind, IntentKind::SessionExitedWithError);
        assert_eq!(intent.body, "The process exited with code 2.");
    }

    #[test]
    fn a_finished_chat_turn_raises_with_the_reply_unless_the_session_is_focused() {
        let mut core = core();
        let session = SessionId::new();
        raise(
            &mut core,
            &chat_event(session, AgentEventKind::TurnStarted, 1),
            1_000,
        );
        raise(
            &mut core,
            &chat_event(
                session,
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    phase: None,
                    text: "Use  a\nBTreeMap, password=hunter2hunter2".to_owned(),
                },
                2,
            ),
            1_100,
        );
        let raised = raise(&mut core, &turn_completed(session, 3), 1_200);
        let [intent] = raised.as_slice() else {
            panic!("expected one intent, got {raised:?}");
        };
        assert_eq!(intent.kind, IntentKind::ChatTurnFinished);
        assert!(intent.body.starts_with("Use a BTreeMap"), "{}", intent.body);
        assert!(!intent.body.contains("hunter2"), "{}", intent.body);

        // Focus suppresses it, and releasing the focus lets the next one through.
        let conn = ConnId(7);
        core.focus.insert(conn, session);
        assert!(raise(&mut core, &turn_completed(session, 4), 100_000).is_empty());
        core.focus.remove(&conn);
        core.remove(&intent.id);
        assert_eq!(
            raise(&mut core, &turn_completed(session, 5), 100_001).len(),
            1
        );

        // Another session's focus does not suppress.
        core.focus.insert(conn, SessionId::new());
        let other = SessionId::new();
        assert_eq!(
            raise(&mut core, &turn_completed(other, 1), 100_002).len(),
            1
        );
    }

    #[test]
    fn replayed_and_streaming_chat_events_raise_nothing() {
        let mut core = core();
        let session = SessionId::new();
        let replay = match turn_completed(session, 1) {
            Event::ChatEvent { session_id, event } => Event::ChatEvent {
                session_id,
                event: Box::new(AgentEvent::replayed(*event)),
            },
            other => other,
        };
        assert!(raise(&mut core, &replay, 1_000).is_empty());
        let delta = chat_event(
            session,
            AgentEventKind::MessageDelta {
                text: "hi".to_owned(),
            },
            2,
        );
        assert!(raise(&mut core, &delta, 1_000).is_empty());
    }

    #[test]
    fn identical_intents_are_deduplicated_while_pending_but_not_after_an_ack() {
        let mut core = core();
        let session = SessionId::new();
        let info = approval(session);
        let event = Event::ApprovalRequired {
            approval: Box::new(info),
        };
        let first = raise(&mut core, &event, 1_000);
        assert_eq!(first.len(), 1);
        assert!(raise(&mut core, &event, 1_001).is_empty(), "same approval");
        assert_eq!(core.list(1_002).len(), 1);
        // A different approval is a different intent.
        assert_eq!(
            raise(
                &mut core,
                &Event::ApprovalRequired {
                    approval: Box::new(approval(session)),
                },
                1_003,
            )
            .len(),
            1
        );
        // Acknowledged intents may be raised again for the same key.
        core.remove(&first[0].id);
        assert_eq!(raise(&mut core, &event, 1_004).len(), 1);
    }

    #[test]
    fn one_intent_per_session_per_ten_seconds_except_approvals() {
        let mut core = core();
        let session = SessionId::new();
        let exited = |code| Event::SessionExited {
            session_id: session,
            code: Some(code),
            signal: None,
        };
        assert_eq!(raise(&mut core, &exited(1), 1_000).len(), 1);
        // A different kind in the same session, inside the window.
        assert!(
            raise(
                &mut core,
                &turn_completed(session, 1),
                1_000 + RATE_LIMIT_MS - 1
            )
            .is_empty()
        );
        // Approvals are not limited, and do not use up the window either.
        for _ in 0..3 {
            assert_eq!(
                raise(
                    &mut core,
                    &Event::ApprovalRequired {
                        approval: Box::new(approval(session)),
                    },
                    1_001,
                )
                .len(),
                1
            );
        }
        // Another session is not limited by this one.
        let other = SessionId::new();
        assert_eq!(raise(&mut core, &turn_completed(other, 1), 1_001).len(), 1);
        // The window ends after exactly RATE_LIMIT_MS.
        assert_eq!(
            raise(
                &mut core,
                &turn_completed(session, 2),
                1_000 + RATE_LIMIT_MS
            )
            .len(),
            1
        );
    }

    #[test]
    fn intents_other_than_approvals_expire_and_approvals_do_not() {
        let mut core = Core::new(NotifySettings {
            expire_secs: 60,
            ..NotifySettings::default()
        });
        let session = SessionId::new();
        raise(
            &mut core,
            &Event::SessionExited {
                session_id: session,
                code: Some(1),
                signal: None,
            },
            1_000,
        );
        raise(
            &mut core,
            &Event::ApprovalRequired {
                approval: Box::new(approval(session)),
            },
            1_000,
        );
        let listed = core.list(60_999);
        assert_eq!(listed.len(), 2);
        let exit = listed
            .iter()
            .find(|i| i.kind == IntentKind::SessionExitedWithError)
            .unwrap();
        assert_eq!(exit.expires_at_ms, Some(61_000));
        let listed = core.list(61_000);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].kind, IntentKind::ApprovalRequired);
        assert!(core.find(&exit.id, 61_000).is_none());
        assert!(!core.remove(&exit.id));
    }

    #[test]
    fn disabled_kinds_and_a_disabled_service_raise_nothing() {
        let session = SessionId::new();
        let exited = Event::SessionExited {
            session_id: session,
            code: Some(1),
            signal: None,
        };
        let mut only_approvals = Core::new(NotifySettings {
            kinds: vec![IntentKind::ApprovalRequired],
            ..NotifySettings::default()
        });
        assert!(raise(&mut only_approvals, &exited, 1_000).is_empty());
        assert_eq!(
            raise(
                &mut only_approvals,
                &Event::ApprovalRequired {
                    approval: Box::new(approval(session)),
                },
                1_000,
            )
            .len(),
            1
        );
        let mut off = Core::new(NotifySettings {
            enabled: false,
            ..NotifySettings::default()
        });
        assert!(raise(&mut off, &exited, 1_000).is_empty());
        assert!(off.list(1_001).is_empty());
    }

    #[test]
    fn the_pending_list_is_bounded_and_drops_the_oldest() {
        let mut core = core();
        let mut ids = Vec::new();
        for n in 0..=MAX_PENDING {
            let raised = raise(
                &mut core,
                &Event::ApprovalRequired {
                    approval: Box::new(approval(SessionId::new())),
                },
                1_000 + n as u64,
            );
            ids.push(raised[0].id.clone());
        }
        let listed = core.list(2_000);
        assert_eq!(listed.len(), MAX_PENDING);
        assert_eq!(listed[0].id, ids[1], "the oldest was dropped");
    }

    #[test]
    fn titles_and_bodies_are_bounded_redacted_single_lines() {
        let long = format!(
            "{}\n{}",
            "word ".repeat(200),
            "sk-abcdefghijklmnopqrstuvwxyz0123"
        );
        let body = clean(&long, BODY_MAX_CHARS);
        assert_eq!(body.chars().count(), BODY_MAX_CHARS);
        assert!(body.ends_with('…'));
        assert!(!body.contains('\n'));
        let secret = clean("key sk-abcdefghijklmnopqrstuvwxyz0123 end", BODY_MAX_CHARS);
        assert_eq!(secret, "key [REDACTED] end");
        let multibyte = clean(&"é".repeat(500), 10);
        assert_eq!(multibyte.chars().count(), 10);
    }

    #[test]
    fn a_reply_that_exactly_fits_the_body_budget_keeps_its_last_character() {
        let fits = format!("{}x", "é".repeat(BODY_MAX_CHARS - 1));
        assert_eq!(clean(&fits, BODY_MAX_CHARS), fits);
        assert_eq!(
            clean(&format!("{fits}z"), BODY_MAX_CHARS),
            format!("{}…", "é".repeat(BODY_MAX_CHARS - 1))
        );
    }

    #[test]
    fn a_secret_that_ends_beyond_any_prefix_is_still_masked() {
        // A quoted assignment whose closing quote lies far beyond the notification body.
        let secret = "abcdefghij".repeat(1000);
        let quoted = format!("private_key=\"{secret}\" and then some text");
        let body = clean(&quoted, BODY_MAX_CHARS);
        assert!(!body.contains("abcdefghij"), "{body}");
        assert!(body.starts_with("private_key=[REDACTED] and"), "{body}");
        assert!(body.chars().count() <= BODY_MAX_CHARS);

        // A URL password whose `@` lies beyond the prefix.
        let password = "p4ssw0rdp4ss".repeat(800);
        let url = format!("fetch https://deploy:{password}@example.com/repo.git now");
        let body = clean(&url, BODY_MAX_CHARS);
        assert!(!body.contains("p4ssw0rdp4ss"), "{body}");
        assert!(
            body.contains("https://deploy:[REDACTED]@example.com"),
            "{body}"
        );

        // The same holds for the title, which carries the session's name.
        let title = clean(&quoted, TITLE_MAX_CHARS);
        assert!(!title.contains("abcdefghij"), "{title}");
    }

    #[test]
    fn ending_a_session_forgets_its_nodes_and_message() {
        let mut core = core();
        let session = SessionId::new();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        assert_eq!(core.nodes.len(), 1);
        raise(
            &mut core,
            &Event::SessionExited {
                session_id: session,
                code: Some(0),
                signal: None,
            },
            2_000,
        );
        assert!(core.nodes.is_empty());
    }
}
