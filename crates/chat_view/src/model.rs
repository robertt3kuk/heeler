//! The transcript: [`AgentEvent`]s folded into the entries the chat view draws.
//!
//! Events are applied in any mix of journal replay and live delivery. Every event is applied at
//! most once (by its `seq`), streaming text only grows an item that is still open, and an item is
//! found by its id, so a reconnect that replays what was already seen, or a history reload that
//! overlaps live events, changes nothing twice. Deltas are not journaled: text that streamed
//! while nobody listened is complete again when the item's `*_end` event arrives.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use staffy_core::agent::{
    FileChange, NoticeLevel, PermissionKind, PermissionOption, PermissionOutcome, PlanStep,
    TokenCounts, ToolKind, ToolStatus,
};
use staffy_core::{AgentEvent, AgentEventKind, ApprovalId, ItemId, TurnId};

/// Bytes of live command output kept per tool; the final output replaces it at the end.
pub const MAX_LIVE_OUTPUT: usize = 256 * 1024;
/// `tool_update` messages kept per tool.
const MAX_UPDATES: usize = 20;

#[derive(Debug, Clone)]
pub struct Message {
    pub item: Option<ItemId>,
    pub text: String,
    pub phase: Option<String>,
    /// The `message_end` event arrived (or the turn ended).
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct Reasoning {
    pub item: Option<ItemId>,
    pub summary: String,
    pub content: String,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub item: Option<ItemId>,
    pub kind: ToolKind,
    pub name: String,
    /// One line: the command, the files, the query.
    pub title: String,
    pub input: Option<Value>,
    /// `None` while the tool runs.
    pub status: Option<ToolStatus>,
    /// The turn ended before the tool reported an end.
    pub interrupted: bool,
    /// Live output while it runs, the complete output after.
    pub output: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub updates: Vec<String>,
    pub changes: Vec<FileChange>,
    /// Permission requests made for this tool, oldest first.
    pub approvals: Vec<ApprovalId>,
}

impl Tool {
    fn new(item: Option<ItemId>, kind: ToolKind) -> Self {
        Self {
            item,
            kind,
            name: String::new(),
            title: String::new(),
            input: None,
            status: None,
            interrupted: false,
            output: String::new(),
            truncated: false,
            exit_code: None,
            duration_ms: None,
            updates: Vec::new(),
            changes: Vec::new(),
            approvals: Vec::new(),
        }
    }

    pub fn running(&self) -> bool {
        self.status.is_none() && !self.interrupted
    }

    /// The working directory of a command, from its input.
    pub fn cwd(&self) -> Option<&str> {
        self.input.as_ref()?.get("cwd")?.as_str()
    }

    /// The command line of a command, from its input (the title otherwise).
    pub fn command(&self) -> &str {
        self.input
            .as_ref()
            .and_then(|input| input.get("command"))
            .and_then(Value::as_str)
            .unwrap_or(&self.title)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Selected {
        option_id: String,
    },
    /// The turn ended, the agent withdrew the request, or it went away.
    Expired,
}

#[derive(Debug, Clone)]
pub struct Approval {
    pub id: ApprovalId,
    pub turn: Option<TurnId>,
    pub kind: PermissionKind,
    pub title: String,
    pub detail: Option<String>,
    pub options: Vec<PermissionOption>,
    pub state: ApprovalState,
}

impl Approval {
    /// The option that was chosen, when one was.
    pub fn chosen(&self) -> Option<&PermissionOption> {
        match &self.state {
            ApprovalState::Selected { option_id } => {
                self.options.iter().find(|o| o.id == *option_id)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub explanation: Option<String>,
    pub steps: Vec<PlanStep>,
    pub text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TurnDiff {
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Failed {
        message: String,
        details: Option<String>,
    },
    Cancelled,
}

/// What one turn cost; a field the provider did not report stays `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnUsage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// Tokens the whole conversation has used so far.
    pub conversation_tokens: Option<u64>,
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Footer {
    pub outcome: TurnOutcome,
    pub duration_ms: Option<u64>,
    pub usage: Option<TurnUsage>,
}

#[derive(Debug, Clone)]
pub enum Body {
    User(Message),
    Assistant(Message),
    Reasoning(Reasoning),
    Tool(Tool),
    /// An approval whose tool is not in the transcript.
    Approval(ApprovalId),
    Plan(Plan),
    TurnDiff(TurnDiff),
    Footer(Footer),
    Notice {
        level: NoticeLevel,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Stable for the life of the transcript (view state such as "expanded" hangs on it).
    pub id: u64,
    /// The transcript's revision when this entry last changed.
    pub rev: u64,
    pub ts_ms: u64,
    pub turn: Option<TurnId>,
    pub body: Body,
}

#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    by_item: HashMap<ItemId, usize>,
    approvals: HashMap<ApprovalId, Approval>,
    approval_entry: HashMap<ApprovalId, usize>,
    turn_plan: HashMap<TurnId, usize>,
    turn_diff: HashMap<TurnId, usize>,
    seen: HashSet<u64>,
    max_seq: u64,
    rev: u64,
    epoch: u64,
    next_id: u64,
    active_turn: bool,
    /// Index of the first entry of the turn in progress.
    turn_start: usize,
    usage: Option<(TokenCounts, Option<u64>)>,
    usage_at_turn_start: TokenCounts,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Changes with every change of an entry; an entry whose `rev` is newer than the one a view
    /// last saw has to be drawn again.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Changes when the entries were replaced wholesale ([`reset`](Self::reset)), which a view
    /// must treat as an entirely new list.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The highest `seq` applied.
    pub fn max_seq(&self) -> u64 {
        self.max_seq
    }

    /// A turn is in progress, as far as the events say.
    pub fn turn_active(&self) -> bool {
        self.active_turn
    }

    pub fn approval(&self, id: ApprovalId) -> Option<&Approval> {
        self.approvals.get(&id)
    }

    /// Permission requests still waiting for an answer, oldest first.
    pub fn pending_approvals(&self) -> Vec<&Approval> {
        let mut pending: Vec<(usize, &Approval)> = self
            .approvals
            .values()
            .filter(|a| a.state == ApprovalState::Pending)
            .map(|a| {
                (
                    self.approval_entry
                        .get(&a.id)
                        .copied()
                        .unwrap_or(usize::MAX),
                    a,
                )
            })
            .collect();
        pending.sort_by_key(|(at, a)| (*at, a.id));
        pending.into_iter().map(|(_, a)| a).collect()
    }

    /// Requests the journal shows as open that the daemon no longer holds are over (a daemon
    /// that restarted expires them without a journal entry of their own).
    pub fn expire_missing(&mut self, pending: &HashSet<ApprovalId>) {
        let stale: Vec<ApprovalId> = self
            .approvals
            .values()
            .filter(|a| a.state == ApprovalState::Pending && !pending.contains(&a.id))
            .map(|a| a.id)
            .collect();
        for id in stale {
            self.set_approval_state(id, ApprovalState::Expired);
        }
    }

    /// Forgets every entry (the daemon restarted: its sequence numbers start over).
    pub fn reset(&mut self) {
        let epoch = self.epoch + 1;
        let rev = self.rev + 1;
        *self = Self::default();
        self.epoch = epoch;
        self.rev = rev;
    }

    // ---- applying events -------------------------------------------------------------------

    /// Folds `event` in. `false` when it was applied before.
    pub fn apply(&mut self, event: &AgentEvent) -> bool {
        if !self.seen.insert(event.seq) {
            return false;
        }
        self.max_seq = self.max_seq.max(event.seq);
        let item = event.item_id;
        let turn = event.turn_id;
        match &event.kind {
            AgentEventKind::TurnStarted => {
                self.active_turn = true;
                self.turn_start = self.entries.len();
                self.usage_at_turn_start = self.usage.map(|(total, _)| total).unwrap_or_default();
            }
            AgentEventKind::MessageStart { role, phase } => {
                let at = self.message_entry(*role, item, turn, event.ts_ms);
                if let Some(message) = self.message_mut(at) {
                    message.phase = phase.clone();
                }
                self.touch(at);
            }
            AgentEventKind::MessageDelta { text } => {
                let at = self.message_entry(
                    staffy_core::agent::MessageRole::Assistant,
                    item,
                    turn,
                    event.ts_ms,
                );
                if let Some(message) = self.message_mut(at)
                    && !message.done
                {
                    message.text.push_str(text);
                    self.touch(at);
                }
            }
            AgentEventKind::MessageEnd { role, phase, text } => {
                let at = self.message_entry(*role, item, turn, event.ts_ms);
                if let Some(message) = self.message_mut(at) {
                    message.text.clone_from(text);
                    message.phase = phase.clone();
                    message.done = true;
                }
                self.touch(at);
            }
            AgentEventKind::ReasoningStart => {
                let at = self.reasoning_entry(item, turn, event.ts_ms);
                self.touch(at);
            }
            AgentEventKind::ReasoningDelta { part, text } => {
                let at = self.reasoning_entry(item, turn, event.ts_ms);
                if let Body::Reasoning(reasoning) = &mut self.entries[at].body
                    && !reasoning.done
                {
                    match part {
                        staffy_core::agent::ReasoningPart::Summary => {
                            reasoning.summary.push_str(text);
                        }
                        staffy_core::agent::ReasoningPart::Content => {
                            reasoning.content.push_str(text);
                        }
                    }
                    self.touch(at);
                }
            }
            AgentEventKind::ReasoningEnd { summary, content } => {
                let at = self.reasoning_entry(item, turn, event.ts_ms);
                if let Body::Reasoning(reasoning) = &mut self.entries[at].body {
                    reasoning.summary = summary.join("\n\n");
                    reasoning.content = content.join("\n\n");
                    reasoning.done = true;
                }
                self.touch(at);
            }
            AgentEventKind::ToolStart {
                tool,
                name,
                title,
                input,
            } => {
                let at = self.tool_entry(item, turn, *tool, event.ts_ms);
                if let Body::Tool(card) = &mut self.entries[at].body {
                    card.kind = *tool;
                    card.name.clone_from(name);
                    card.title.clone_from(title);
                    card.input.clone_from(input);
                }
                self.touch(at);
            }
            AgentEventKind::ToolUpdate { message } => {
                if let Some(at) = self.known_tool(item)
                    && let Body::Tool(card) = &mut self.entries[at].body
                {
                    card.updates.push(message.clone());
                    if card.updates.len() > MAX_UPDATES {
                        card.updates.remove(0);
                    }
                    self.touch(at);
                }
            }
            AgentEventKind::CommandOutput { text } => {
                if let Some(at) = self.known_tool(item)
                    && let Body::Tool(card) = &mut self.entries[at].body
                    && card.running()
                {
                    card.output.push_str(text);
                    cut_front(&mut card.output, MAX_LIVE_OUTPUT);
                    self.touch(at);
                }
            }
            AgentEventKind::ToolEnd {
                tool,
                status,
                output,
                truncated,
                exit_code,
                duration_ms,
            } => {
                let at = self.tool_entry(item, turn, *tool, event.ts_ms);
                if let Body::Tool(card) = &mut self.entries[at].body {
                    card.status = Some(*status);
                    card.interrupted = false;
                    if let Some(output) = output {
                        card.output.clone_from(output);
                    }
                    card.truncated = *truncated;
                    card.exit_code = *exit_code;
                    card.duration_ms = *duration_ms;
                }
                self.touch(at);
            }
            AgentEventKind::FileDiff { changes, patch } => {
                self.file_diff(item, turn, changes, patch.as_deref(), event.ts_ms);
            }
            AgentEventKind::PermissionRequested {
                approval_id,
                kind,
                title,
                detail,
                options,
            } => {
                self.permission_requested(
                    item,
                    turn,
                    Approval {
                        id: *approval_id,
                        turn,
                        kind: *kind,
                        title: title.clone(),
                        detail: detail.clone(),
                        options: options.clone(),
                        state: ApprovalState::Pending,
                    },
                    event.ts_ms,
                );
            }
            AgentEventKind::PermissionResolved {
                approval_id,
                outcome,
            } => {
                let state = match outcome {
                    PermissionOutcome::Selected { option_id } => ApprovalState::Selected {
                        option_id: option_id.clone(),
                    },
                    PermissionOutcome::Expired => ApprovalState::Expired,
                };
                self.set_approval_state(*approval_id, state);
            }
            AgentEventKind::Plan {
                explanation,
                steps,
                text,
            } => {
                let plan = Plan {
                    explanation: explanation.clone(),
                    steps: steps.clone(),
                    text: text.clone(),
                };
                let existing = match (item, turn) {
                    (Some(item), _) => self.by_item.get(&item).copied(),
                    (None, Some(turn)) => self.turn_plan.get(&turn).copied(),
                    (None, None) => None,
                };
                let at = match existing {
                    Some(at) => {
                        self.entries[at].body = Body::Plan(plan);
                        at
                    }
                    None => {
                        let at = self.push(turn, event.ts_ms, Body::Plan(plan));
                        match (item, turn) {
                            (Some(item), _) => {
                                self.by_item.insert(item, at);
                            }
                            (None, Some(turn)) => {
                                self.turn_plan.insert(turn, at);
                            }
                            (None, None) => {}
                        }
                        at
                    }
                };
                self.touch(at);
            }
            AgentEventKind::Usage {
                total,
                context_window,
                ..
            } => match (total, &mut self.usage) {
                (Some(total), _) => self.usage = Some((*total, *context_window)),
                (None, Some((_, window))) => *window = context_window.or(*window),
                (None, None) => {}
            },
            AgentEventKind::TurnCompleted { duration_ms } => {
                self.turn_ended(turn, TurnOutcome::Completed, *duration_ms, event.ts_ms);
            }
            AgentEventKind::TurnFailed { message, details } => {
                self.turn_ended(
                    turn,
                    TurnOutcome::Failed {
                        message: message.clone(),
                        details: details.clone(),
                    },
                    None,
                    event.ts_ms,
                );
            }
            AgentEventKind::TurnCancelled => {
                self.turn_ended(turn, TurnOutcome::Cancelled, None, event.ts_ms);
            }
            AgentEventKind::Notice { level, message } => {
                let at = self.push(
                    turn,
                    event.ts_ms,
                    Body::Notice {
                        level: *level,
                        message: message.clone(),
                    },
                );
                self.touch(at);
            }
            AgentEventKind::ResumeLocator { .. } | AgentEventKind::Opaque { .. } => {}
        }
        true
    }

    fn push(&mut self, turn: Option<TurnId>, ts_ms: u64, body: Body) -> usize {
        self.next_id += 1;
        self.entries.push(Entry {
            id: self.next_id,
            rev: 0,
            ts_ms,
            turn,
            body,
        });
        self.entries.len() - 1
    }

    fn touch(&mut self, at: usize) {
        self.rev += 1;
        if let Some(entry) = self.entries.get_mut(at) {
            entry.rev = self.rev;
        }
    }

    fn message_mut(&mut self, at: usize) -> Option<&mut Message> {
        match &mut self.entries[at].body {
            Body::User(message) | Body::Assistant(message) => Some(message),
            _ => None,
        }
    }

    fn message_entry(
        &mut self,
        role: staffy_core::agent::MessageRole,
        item: Option<ItemId>,
        turn: Option<TurnId>,
        ts_ms: u64,
    ) -> usize {
        if let Some(at) = item.and_then(|i| self.by_item.get(&i).copied()) {
            return at;
        }
        let message = Message {
            item,
            text: String::new(),
            phase: None,
            done: false,
        };
        let body = match role {
            staffy_core::agent::MessageRole::User => Body::User(message),
            staffy_core::agent::MessageRole::Assistant => Body::Assistant(message),
        };
        let at = self.push(turn, ts_ms, body);
        if let Some(item) = item {
            self.by_item.insert(item, at);
        }
        at
    }

    fn reasoning_entry(&mut self, item: Option<ItemId>, turn: Option<TurnId>, ts_ms: u64) -> usize {
        if let Some(at) = item.and_then(|i| self.by_item.get(&i).copied()) {
            return at;
        }
        let at = self.push(
            turn,
            ts_ms,
            Body::Reasoning(Reasoning {
                item,
                summary: String::new(),
                content: String::new(),
                done: false,
            }),
        );
        if let Some(item) = item {
            self.by_item.insert(item, at);
        }
        at
    }

    fn tool_entry(
        &mut self,
        item: Option<ItemId>,
        turn: Option<TurnId>,
        kind: ToolKind,
        ts_ms: u64,
    ) -> usize {
        if let Some(at) = self.known_tool(item) {
            return at;
        }
        let at = self.push(turn, ts_ms, Body::Tool(Tool::new(item, kind)));
        if let Some(item) = item {
            self.by_item.insert(item, at);
        }
        at
    }

    fn known_tool(&self, item: Option<ItemId>) -> Option<usize> {
        let at = item.and_then(|i| self.by_item.get(&i).copied())?;
        matches!(self.entries[at].body, Body::Tool(_)).then_some(at)
    }

    fn file_diff(
        &mut self,
        item: Option<ItemId>,
        turn: Option<TurnId>,
        changes: &[FileChange],
        patch: Option<&str>,
        ts_ms: u64,
    ) {
        if !changes.is_empty() {
            let at = self.tool_entry(item, turn, ToolKind::FileChange, ts_ms);
            if let Body::Tool(card) = &mut self.entries[at].body {
                if card.name.is_empty() {
                    "apply_patch".clone_into(&mut card.name);
                    card.title = match changes {
                        [one] => format!("edit {}", one.path),
                        [first, rest @ ..] => {
                            format!("edit {} and {} more", first.path, rest.len())
                        }
                        [] => String::new(),
                    };
                }
                for change in changes {
                    match card.changes.iter_mut().find(|c| c.path == change.path) {
                        Some(existing) => *existing = change.clone(),
                        None => card.changes.push(change.clone()),
                    }
                }
            }
            self.touch(at);
        }
        if let (Some(patch), Some(turn)) = (patch, turn)
            && !patch.is_empty()
        {
            match self.turn_diff.get(&turn).copied() {
                Some(at) => {
                    self.entries[at].body = Body::TurnDiff(TurnDiff {
                        patch: patch.to_owned(),
                    });
                    self.touch(at);
                }
                None => {
                    let at = self.push(
                        Some(turn),
                        ts_ms,
                        Body::TurnDiff(TurnDiff {
                            patch: patch.to_owned(),
                        }),
                    );
                    self.turn_diff.insert(turn, at);
                    self.touch(at);
                }
            }
        }
    }

    fn permission_requested(
        &mut self,
        item: Option<ItemId>,
        turn: Option<TurnId>,
        approval: Approval,
        ts_ms: u64,
    ) {
        let id = approval.id;
        if self.approvals.contains_key(&id) {
            return;
        }
        self.approvals.insert(id, approval);
        let at = match self.known_tool(item) {
            Some(at) => {
                if let Body::Tool(card) = &mut self.entries[at].body {
                    card.approvals.push(id);
                }
                at
            }
            None => self.push(turn, ts_ms, Body::Approval(id)),
        };
        self.approval_entry.insert(id, at);
        self.touch(at);
    }

    fn set_approval_state(&mut self, id: ApprovalId, state: ApprovalState) {
        if let Some(approval) = self.approvals.get_mut(&id) {
            approval.state = state;
        }
        if let Some(at) = self.approval_entry.get(&id).copied() {
            self.touch(at);
        }
    }

    fn turn_ended(
        &mut self,
        turn: Option<TurnId>,
        outcome: TurnOutcome,
        duration_ms: Option<u64>,
        ts_ms: u64,
    ) {
        // Whatever was still open when the turn ended is over.
        let from = self.turn_start.min(self.entries.len());
        for at in from..self.entries.len() {
            let changed = match &mut self.entries[at].body {
                Body::Assistant(m) | Body::User(m) if !m.done => {
                    m.done = true;
                    true
                }
                Body::Reasoning(r) if !r.done => {
                    r.done = true;
                    true
                }
                Body::Tool(t) if t.status.is_none() && !t.interrupted => {
                    t.interrupted = true;
                    true
                }
                _ => false,
            };
            if changed {
                self.touch(at);
            }
        }
        self.active_turn = false;
        let usage = self.turn_usage();
        let at = self.push(
            turn,
            ts_ms,
            Body::Footer(Footer {
                outcome,
                duration_ms,
                usage,
            }),
        );
        self.touch(at);
    }

    fn turn_usage(&self) -> Option<TurnUsage> {
        let (now, window) = self.usage?;
        let before = self.usage_at_turn_start;
        let delta = |now: Option<u64>, before: Option<u64>| {
            now.map(|n| n.saturating_sub(before.unwrap_or(0)))
        };
        Some(TurnUsage {
            input_tokens: delta(now.input_tokens, before.input_tokens),
            cached_input_tokens: delta(now.cached_input_tokens, before.cached_input_tokens),
            output_tokens: delta(now.output_tokens, before.output_tokens),
            reasoning_output_tokens: delta(
                now.reasoning_output_tokens,
                before.reasoning_output_tokens,
            ),
            total_tokens: delta(now.total_tokens, before.total_tokens),
            conversation_tokens: now.total_tokens,
            context_window: window,
        })
    }
}

/// Cuts `text` to its last `max` bytes (at a character boundary).
fn cut_front(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut at = text.len() - max;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    text.drain(..at);
}

#[cfg(test)]
mod tests {
    use super::*;
    use staffy_core::SessionId;
    use staffy_core::agent::{
        Delivery, FileChangeKind, MessageRole, PermissionOptionKind, ProviderIds, ReasoningPart,
    };

    struct Script {
        session: SessionId,
        turn: TurnId,
        seq: u64,
    }

    impl Script {
        fn new() -> Self {
            Self {
                session: SessionId::new(),
                turn: TurnId::new(),
                seq: 0,
            }
        }

        fn event(&mut self, item: Option<ItemId>, kind: AgentEventKind) -> AgentEvent {
            self.seq += 1;
            AgentEvent {
                session_id: self.session,
                seq: self.seq,
                ts_ms: self.seq * 10,
                delivery: Delivery::Live,
                turn_id: Some(self.turn),
                item_id: item,
                provider: ProviderIds::default(),
                kind,
                payload: None,
            }
        }
    }

    fn assistant(t: &Transcript, at: usize) -> &Message {
        match &t.entries()[at].body {
            Body::Assistant(m) => m,
            other => panic!("{other:?}"),
        }
    }

    fn tool(t: &Transcript, at: usize) -> &Tool {
        match &t.entries()[at].body {
            Body::Tool(tool) => tool,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_streamed_message_grows_and_ends_with_the_complete_text() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        assert!(t.turn_active());
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: Some("final_answer".into()),
            },
        ));
        for piece in ["po", "ng"] {
            t.apply(&s.event(
                Some(item),
                AgentEventKind::MessageDelta { text: piece.into() },
            ));
        }
        assert_eq!(assistant(&t, 0).text, "pong");
        assert!(!assistant(&t, 0).done);
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                phase: None,
                text: "pong!".into(),
            },
        ));
        assert_eq!(assistant(&t, 0).text, "pong!");
        assert!(assistant(&t, 0).done);
        // A late delta (replayed after the end) does not grow it.
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageDelta { text: "x".into() },
        ));
        assert_eq!(assistant(&t, 0).text, "pong!");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn an_event_applies_once() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        let start = s.event(
            Some(item),
            AgentEventKind::MessageStart {
                role: MessageRole::User,
                phase: None,
            },
        );
        let delta = s.event(
            Some(item),
            AgentEventKind::MessageDelta { text: "a".into() },
        );
        assert!(t.apply(&start));
        assert!(t.apply(&delta));
        assert!(!t.apply(&start));
        assert!(!t.apply(&delta));
        assert_eq!(t.len(), 1);
        assert_eq!(t.max_seq(), 2);
    }

    #[test]
    fn a_command_card_collects_live_output_and_ends_with_the_whole() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "ls".into(),
                input: Some(serde_json::json!({"command": "ls", "cwd": "/w"})),
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::CommandOutput { text: "a\n".into() },
        ));
        assert!(tool(&t, 0).running());
        assert_eq!(tool(&t, 0).output, "a\n");
        assert_eq!(tool(&t, 0).cwd(), Some("/w"));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolEnd {
                tool: ToolKind::Command,
                status: ToolStatus::Completed,
                output: Some("a\nb\n".into()),
                truncated: false,
                exit_code: Some(0),
                duration_ms: Some(12),
            },
        ));
        let card = tool(&t, 0);
        assert!(!card.running());
        assert_eq!(card.output, "a\nb\n");
        assert_eq!(card.exit_code, Some(0));
        // Output after the end is ignored.
        t.apply(&s.event(
            Some(item),
            AgentEventKind::CommandOutput {
                text: "late".into(),
            },
        ));
        assert_eq!(tool(&t, 0).output, "a\nb\n");
    }

    #[test]
    fn a_tool_end_without_output_keeps_the_live_output() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "make".into(),
                input: None,
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::CommandOutput {
                text: "built".into(),
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolEnd {
                tool: ToolKind::Command,
                status: ToolStatus::Failed,
                output: None,
                truncated: false,
                exit_code: Some(2),
                duration_ms: None,
            },
        ));
        assert_eq!(tool(&t, 0).output, "built");
        assert_eq!(tool(&t, 0).status, Some(ToolStatus::Failed));
    }

    #[test]
    fn live_output_is_bounded_to_its_tail() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "yes".into(),
                input: None,
            },
        ));
        let chunk = "é".repeat(100_000);
        for _ in 0..4 {
            t.apply(&s.event(
                Some(item),
                AgentEventKind::CommandOutput {
                    text: chunk.clone(),
                },
            ));
        }
        let output = &tool(&t, 0).output;
        assert!(output.len() <= MAX_LIVE_OUTPUT);
        assert!(output.chars().all(|c| c == 'é'));
    }

    #[test]
    fn file_changes_merge_by_path_and_the_turn_diff_has_its_own_entry() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        let change = |patch: &str| FileChange {
            path: "/w/a.txt".into(),
            kind: FileChangeKind::Add,
            move_path: None,
            patch: Some(patch.into()),
            before: None,
            after: None,
        };
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::FileChange,
                name: "apply_patch".into(),
                title: "edit /w/a.txt".into(),
                input: None,
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::FileDiff {
                changes: vec![change("one\n")],
                patch: None,
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::FileDiff {
                changes: vec![change("one\ntwo\n")],
                patch: None,
            },
        ));
        assert_eq!(tool(&t, 0).changes.len(), 1);
        assert_eq!(tool(&t, 0).changes[0].patch.as_deref(), Some("one\ntwo\n"));
        t.apply(&s.event(
            None,
            AgentEventKind::FileDiff {
                changes: Vec::new(),
                patch: Some("diff --git a/a b/a\n".into()),
            },
        ));
        t.apply(&s.event(
            None,
            AgentEventKind::FileDiff {
                changes: Vec::new(),
                patch: Some("diff --git a/a b/a\n+x\n".into()),
            },
        ));
        assert_eq!(t.len(), 2, "one turn diff entry, updated in place");
        assert!(matches!(&t.entries()[1].body, Body::TurnDiff(d) if d.patch.ends_with("+x\n")));
    }

    fn request(s: &mut Script, item: Option<ItemId>, id: ApprovalId) -> AgentEvent {
        s.event(
            item,
            AgentEventKind::PermissionRequested {
                approval_id: id,
                kind: PermissionKind::Command,
                title: "touch hi.txt".into(),
                detail: None,
                options: vec![
                    PermissionOption {
                        id: "accept".into(),
                        label: "Allow once".into(),
                        kind: PermissionOptionKind::AllowOnce,
                        provider: None,
                    },
                    PermissionOption {
                        id: "cancel".into(),
                        label: "Cancel".into(),
                        kind: PermissionOptionKind::Cancel,
                        provider: None,
                    },
                ],
            },
        )
    }

    #[test]
    fn an_approval_belongs_to_its_tool_and_resolves_in_place() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        let id = ApprovalId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "touch hi.txt".into(),
                input: None,
            },
        ));
        t.apply(&request(&mut s, Some(item), id));
        assert_eq!(t.len(), 1, "the card is part of the tool entry");
        assert_eq!(tool(&t, 0).approvals, [id]);
        assert_eq!(t.pending_approvals().len(), 1);
        let before = t.entries()[0].rev;
        t.apply(&s.event(
            None,
            AgentEventKind::PermissionResolved {
                approval_id: id,
                outcome: PermissionOutcome::Selected {
                    option_id: "accept".into(),
                },
            },
        ));
        assert!(t.pending_approvals().is_empty());
        let approval = t.approval(id).unwrap();
        assert_eq!(approval.chosen().unwrap().label, "Allow once");
        assert!(
            t.entries()[0].rev > before,
            "the tool's card is drawn again"
        );
    }

    #[test]
    fn an_approval_without_a_tool_is_its_own_entry() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let id = ApprovalId::new();
        t.apply(&request(&mut s, Some(ItemId::new()), id));
        assert!(matches!(t.entries()[0].body, Body::Approval(a) if a == id));
        // A repeated request changes nothing.
        let again = request(&mut s, Some(ItemId::new()), id);
        t.apply(&again);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_end_of_a_turn_expires_nothing_by_itself_but_closes_open_items() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let (msg, cmd) = (ItemId::new(), ItemId::new());
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        t.apply(&s.event(
            Some(msg),
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: None,
            },
        ));
        t.apply(&s.event(
            Some(cmd),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "sleep 9".into(),
                input: None,
            },
        ));
        t.apply(&s.event(None, AgentEventKind::TurnCancelled));
        assert!(!t.turn_active());
        assert!(assistant(&t, 0).done);
        assert!(tool(&t, 1).interrupted);
        assert!(!tool(&t, 1).running());
        assert!(matches!(
            &t.entries()[2].body,
            Body::Footer(f) if f.outcome == TurnOutcome::Cancelled
        ));
    }

    #[test]
    fn the_footer_says_what_the_turn_used() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let usage = |total: u64, input: u64| AgentEventKind::Usage {
            total: Some(TokenCounts {
                total_tokens: Some(total),
                input_tokens: Some(input),
                ..TokenCounts::default()
            }),
            last: None,
            context_window: Some(128_000),
        };
        for (total, input) in [(100, 90), (250, 220)] {
            t.apply(&s.event(None, AgentEventKind::TurnStarted));
            t.apply(&s.event(None, usage(total, input)));
            t.apply(&s.event(
                None,
                AgentEventKind::TurnCompleted {
                    duration_ms: Some(1500),
                },
            ));
        }
        let footers: Vec<&Footer> = t
            .entries()
            .iter()
            .filter_map(|e| match &e.body {
                Body::Footer(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(footers.len(), 2);
        let second = footers[1].usage.unwrap();
        assert_eq!(second.total_tokens, Some(150));
        assert_eq!(second.input_tokens, Some(130));
        assert_eq!(second.output_tokens, None, "never zero-filled");
        assert_eq!(second.conversation_tokens, Some(250));
        assert_eq!(second.context_window, Some(128_000));
        assert_eq!(footers[1].duration_ms, Some(1500));
    }

    #[test]
    fn a_turn_without_usage_has_no_usage_footer() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        t.apply(&s.event(None, AgentEventKind::TurnCompleted { duration_ms: None }));
        assert!(matches!(&t.entries()[0].body, Body::Footer(f) if f.usage.is_none()));
    }

    #[test]
    fn plans_update_in_place_per_turn() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let plan = |status| AgentEventKind::Plan {
            explanation: None,
            steps: vec![PlanStep {
                text: "do it".into(),
                status,
            }],
            text: None,
        };
        t.apply(&s.event(None, plan(staffy_core::agent::PlanStepStatus::Pending)));
        t.apply(&s.event(None, plan(staffy_core::agent::PlanStepStatus::Completed)));
        assert_eq!(t.len(), 1);
        assert!(matches!(
            &t.entries()[0].body,
            Body::Plan(p) if p.steps[0].status == staffy_core::agent::PlanStepStatus::Completed
        ));
    }

    #[test]
    fn reasoning_streams_then_takes_the_final_parts() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(Some(item), AgentEventKind::ReasoningStart));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ReasoningDelta {
                part: ReasoningPart::Summary,
                text: "Check".into(),
            },
        ));
        assert!(
            matches!(&t.entries()[0].body, Body::Reasoning(r) if r.summary == "Check" && !r.done)
        );
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ReasoningEnd {
                summary: vec!["Check the files".into()],
                content: Vec::new(),
            },
        ));
        assert!(matches!(
            &t.entries()[0].body,
            Body::Reasoning(r) if r.summary == "Check the files" && r.done
        ));
    }

    #[test]
    fn a_reload_replays_what_the_view_has_without_duplicating() {
        let mut s = Script::new();
        let item = ItemId::new();
        let events = vec![
            s.event(None, AgentEventKind::TurnStarted),
            s.event(
                Some(item),
                AgentEventKind::MessageStart {
                    role: MessageRole::Assistant,
                    phase: None,
                },
            ),
            s.event(
                Some(item),
                AgentEventKind::MessageDelta { text: "hel".into() },
            ),
            s.event(
                Some(item),
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    phase: None,
                    text: "hello".into(),
                },
            ),
            s.event(None, AgentEventKind::TurnCompleted { duration_ms: None }),
        ];
        let mut t = Transcript::new();
        // Live: everything up to the delta.
        for e in &events[..3] {
            t.apply(e);
        }
        // The journal arrives (no deltas) and then the replayed live events.
        for e in events.iter().filter(|e| !e.kind.is_streaming()) {
            t.apply(e);
        }
        for e in &events {
            t.apply(e);
        }
        assert_eq!(t.len(), 2);
        assert_eq!(assistant(&t, 0).text, "hello");
        assert!(!t.turn_active());
    }

    #[test]
    fn expire_missing_ends_requests_the_daemon_no_longer_holds() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let (a, b) = (ApprovalId::new(), ApprovalId::new());
        t.apply(&request(&mut s, None, a));
        t.apply(&request(&mut s, None, b));
        t.expire_missing(&HashSet::from([b]));
        assert_eq!(t.approval(a).unwrap().state, ApprovalState::Expired);
        assert_eq!(t.approval(b).unwrap().state, ApprovalState::Pending);
    }

    #[test]
    fn reset_starts_a_new_list() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let id = ApprovalId::new();
        t.apply(&request(&mut s, None, id));
        let epoch = t.epoch();
        t.reset();
        assert!(t.is_empty());
        assert_ne!(t.epoch(), epoch);
        assert!(t.pending_approvals().is_empty());
        // Sequence numbers start over after a daemon restart: the same seq applies again.
        let fresh = s.event(None, AgentEventKind::TurnStarted);
        let mut again = fresh.clone();
        again.seq = 1;
        assert!(t.apply(&again));
    }

    #[test]
    fn every_change_bumps_the_entry_revision() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: None,
            },
        ));
        let first = t.entries()[0].rev;
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageDelta { text: "x".into() },
        ));
        assert!(t.entries()[0].rev > first);
        assert!(t.rev() >= t.entries()[0].rev);
    }
}
