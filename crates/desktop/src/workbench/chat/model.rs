//! [`ChatModel`]: one chat session's transcript and composer, shared by every view of it.
//!
//! It loads the journal (`chat.history`), then follows the connection's chat events. A
//! reconnect or a lagging receiver reads the journal again and applies only what is new; a
//! daemon restart (another epoch) starts the transcript over. See `heeler_chat_view::model` for
//! why applying an event twice is harmless.

use std::collections::{HashMap, HashSet};

use gpui::{Context, SharedString, Task};
use heeler_chat_view::Transcript;
use heeler_client::{ChatUpdate, ClientError, Connection};
use heeler_core::{AgentEvent, ApprovalId, Epoch, ItemId, SessionId};
use heeler_protocol::ErrorCode;
use heeler_protocol::assistant::{AttachmentKind, AttachmentMeta, attachments_meta};
use heeler_protocol::chat::HISTORY_MAX_LIMIT;
use tokio::sync::broadcast::error::RecvError;

use super::chip::{Chip, compose};
use super::context::{ContextChip, refused_by};
use super::input::TextInput;

/// Journal pages read per load; older events than this many pages are not shown.
const MAX_PAGES: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    Loading,
    Ready,
    /// The journal could not be read (offline, or the daemon refused).
    Failed(String),
}

#[derive(Debug)]
struct History {
    /// Oldest first.
    events: Vec<AgentEvent>,
    /// The journal has more before the first event.
    cut: bool,
}

pub struct ChatModel {
    session: SessionId,
    connection: Connection,
    transcript: Transcript,
    status: LoadStatus,
    /// Events older than the pages read exist.
    history_cut: bool,
    input: TextInput,
    chips: Vec<Chip>,
    /// Structured context for the next message (assistant chats): the daemon reads it.
    context: Vec<ContextChip>,
    /// What the daemon says about the attachments of each user message of the transcript.
    attachments: HashMap<ItemId, Vec<AttachmentMeta>>,
    sending: bool,
    /// The last thing that went wrong with a command (send, interrupt, answer).
    error: Option<SharedString>,
    /// What was taken out of the composer by a send that has not been answered yet.
    unsent: Option<(String, Vec<Chip>, Vec<ContextChip>)>,
    _task: Task<()>,
}

impl std::fmt::Debug for ChatModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatModel")
            .field("session", &self.session)
            .field("status", &self.status)
            .field("entries", &self.transcript.len())
            .finish_non_exhaustive()
    }
}

impl ChatModel {
    pub fn new(session: SessionId, connection: Connection, cx: &mut Context<Self>) -> Self {
        // Subscribed before the first read, so nothing between the two is lost.
        let mut updates = connection.chat_updates();
        let reader = connection.clone();
        let task = cx.spawn(async move |this, cx| {
            let mut known: Option<Epoch> = None;
            let mut load = true;
            loop {
                if load {
                    let stop_at = this
                        .update(cx, |this, _| {
                            let seen = this.transcript.max_seq();
                            (seen > 0).then_some(seen)
                        })
                        .ok()
                        .flatten();
                    let history = fetch_history(&reader, session, stop_at).await;
                    let pending = match &history {
                        Ok(_) => reader.approvals(Some(session)).await.ok(),
                        Err(_) => None,
                    };
                    let alive = this.update(cx, |this, cx| {
                        this.installed(history, pending, cx);
                    });
                    if alive.is_err() {
                        return;
                    }
                    load = false;
                }
                match updates.recv().await {
                    Ok(ChatUpdate::Event(event)) => {
                        if event.session_id != session {
                            continue;
                        }
                        let alive = this.update(cx, |this, cx| {
                            this.note_attachments(&event);
                            if this.transcript.apply(&event) {
                                cx.notify();
                            }
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                    Ok(ChatUpdate::Resync { epoch }) => {
                        if known.is_some_and(|before| before != epoch) {
                            // The daemon restarted: its sequence numbers start over.
                            let alive = this.update(cx, |this, cx| {
                                this.transcript.reset();
                                this.history_cut = false;
                                cx.notify();
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                        known = Some(epoch);
                        load = true;
                    }
                    Err(RecvError::Lagged(_)) => load = true,
                    Err(RecvError::Closed) => return,
                }
            }
        });
        Self {
            session,
            connection,
            transcript: Transcript::new(),
            status: LoadStatus::Loading,
            history_cut: false,
            input: TextInput::new(),
            chips: Vec::new(),
            context: Vec::new(),
            attachments: HashMap::new(),
            sending: false,
            error: None,
            unsent: None,
            _task: task,
        }
    }

    fn installed(
        &mut self,
        history: Result<History, ClientError>,
        pending: Option<Vec<heeler_protocol::chat::ApprovalInfo>>,
        cx: &mut Context<Self>,
    ) {
        match history {
            Ok(history) => {
                for event in &history.events {
                    self.note_attachments(event);
                    self.transcript.apply(event);
                }
                self.history_cut |= history.cut;
                if let Some(pending) = pending {
                    let ids: HashSet<ApprovalId> = pending.iter().map(|a| a.id).collect();
                    self.transcript.expire_missing(&ids);
                }
                self.status = LoadStatus::Ready;
            }
            Err(error) => {
                // Offline: what is shown stays; a reconnect reads again.
                if self.transcript.is_empty() {
                    self.status = LoadStatus::Failed(error.to_string());
                }
            }
        }
        cx.notify();
    }

    // ---- read access -----------------------------------------------------------------------

    pub fn session(&self) -> SessionId {
        self.session
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn status(&self) -> &LoadStatus {
        &self.status
    }

    /// The journal has events older than the ones shown.
    pub fn history_cut(&self) -> bool {
        self.history_cut
    }

    pub fn input(&self) -> &TextInput {
        &self.input
    }

    pub fn chips(&self) -> &[Chip] {
        &self.chips
    }

    /// The structured context attached to the next message.
    pub fn context(&self) -> &[ContextChip] {
        &self.context
    }

    /// What the daemon resolved for the attachments of the user message `item` (kind, label,
    /// characters kept, whether the budget cut it); empty for a message without any.
    pub fn attachments_of(&self, item: ItemId) -> &[AttachmentMeta] {
        self.attachments.get(&item).map_or(&[], Vec::as_slice)
    }

    fn note_attachments(&mut self, event: &AgentEvent) {
        let Some(item) = event.item_id else {
            return;
        };
        let meta = attachments_meta(event);
        if !meta.is_empty() {
            self.attachments.insert(item, meta);
        }
    }

    pub fn sending(&self) -> bool {
        self.sending
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    pub fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }

    // ---- composer --------------------------------------------------------------------------

    /// Edits the draft.
    pub fn edit(&mut self, change: impl FnOnce(&mut TextInput), cx: &mut Context<Self>) {
        change(&mut self.input);
        cx.notify();
    }

    /// Attaches a terminal block to the next message.
    pub fn attach(&mut self, chip: Chip, cx: &mut Context<Self>) {
        self.chips.push(chip);
        cx.notify();
    }

    /// Attaches structured context to the next message; the daemon reads, redacts and bounds it
    /// when the message is sent. At most [`heeler_protocol::assistant::ATTACHMENTS_MAX`].
    pub fn attach_context(&mut self, chip: ContextChip, cx: &mut Context<Self>) -> bool {
        if self.context.len() >= heeler_protocol::assistant::ATTACHMENTS_MAX {
            self.error = Some(
                format!(
                    "At most {} attachments go with one message",
                    heeler_protocol::assistant::ATTACHMENTS_MAX
                )
                .into(),
            );
            cx.notify();
            return false;
        }
        self.context.push(chip);
        cx.notify();
        true
    }

    pub fn remove_context(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.context.len() {
            self.context.remove(index);
            cx.notify();
        }
    }

    pub fn remove_chip(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.chips.len() {
            self.chips.remove(index);
            cx.notify();
        }
    }

    /// The message the composer would send now.
    pub fn draft_message(&self) -> String {
        compose(&self.chips, self.input.text())
    }

    /// Sends the draft with its chips and its structured context. Nothing happens when there is
    /// nothing to send or a send is in flight. The composer empties at once; a failure puts the
    /// draft back.
    pub fn send(&mut self, cx: &mut Context<Self>) -> bool {
        let message = self.draft_message();
        if message.is_empty() || self.sending {
            return false;
        }
        let draft = self.input.take();
        let chips = std::mem::take(&mut self.chips);
        let context = std::mem::take(&mut self.context);
        let attachments = context.iter().map(|c| c.attachment.clone()).collect();
        self.unsent = Some((draft, chips, context));
        self.sending = true;
        self.error = None;
        let connection = self.connection.clone();
        let session = self.session;
        cx.spawn(async move |this, cx| {
            let sent = connection
                .chat_send_with(session, message, attachments)
                .await;
            this.update(cx, |this, cx| {
                this.sending = false;
                let unsent = this.unsent.take();
                if let Err(error) = sent {
                    let refusal = refusal_of(&error);
                    this.error = Some(
                        match &refusal {
                            Some((_, message)) => format!("Not allowed: {message}"),
                            None => format!("Could not send: {error}"),
                        }
                        .into(),
                    );
                    // The user may have typed on meanwhile: the old draft goes in front.
                    if let Some((draft, chips, mut context)) = unsent {
                        let rest = this.input.take();
                        let mut restored = draft;
                        if !restored.is_empty() && !rest.is_empty() {
                            restored.push('\n');
                        }
                        restored.push_str(&rest);
                        this.input.set(&restored);
                        let mut all = chips;
                        all.append(&mut this.chips);
                        this.chips = all;
                        // A refusal says which kind of context the settings do not allow (a
                        // path outside the workspace names no setting: it is about files): the
                        // chips of that kind show it.
                        if let Some((setting, message)) = refusal {
                            for chip in &mut context {
                                let about = match setting.as_deref() {
                                    Some(setting) => refused_by(setting, chip.kind()),
                                    None => chip.kind() == AttachmentKind::File,
                                };
                                if about {
                                    chip.refused = Some(message.clone());
                                }
                            }
                        }
                        context.append(&mut this.context);
                        this.context = context;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
        true
    }

    // ---- commands --------------------------------------------------------------------------

    /// `chat.interrupt`.
    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        let session = self.session;
        cx.spawn(async move |this, cx| {
            if let Err(error) = connection.chat_interrupt(session).await {
                this.update(cx, |this, cx| {
                    this.error = Some(format!("Could not interrupt: {error}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Shows `message` as the last thing that went wrong.
    pub fn set_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }
}

/// The setting (when the daemon names one) and the message of a `forbidden` refusal: a context
/// kind the daemon's `[assistant.context]` does not allow, a file outside the workspace.
fn refusal_of(error: &ClientError) -> Option<(Option<String>, String)> {
    let ClientError::Remote(body) = error else {
        return None;
    };
    if body.code != ErrorCode::Forbidden {
        return None;
    }
    let setting = body
        .data
        .as_ref()
        .and_then(|data| data.get("setting"))
        .and_then(|setting| setting.as_str())
        .map(str::to_owned);
    Some((setting, body.message.clone()))
}

/// The journal's newest pages, oldest event first, down to (and including) the page that holds
/// `stop_at` when one is given, else down to the start or [`MAX_PAGES`] pages.
async fn fetch_history(
    connection: &Connection,
    session: SessionId,
    stop_at: Option<u64>,
) -> Result<History, ClientError> {
    let mut pages: Vec<Vec<AgentEvent>> = Vec::new();
    let mut before = None;
    let mut cut = false;
    for page_number in 0..MAX_PAGES {
        let page = connection
            .chat_history(session, before, HISTORY_MAX_LIMIT)
            .await?;
        let first = page.events.first().map(|event| event.seq);
        let more = page.has_more;
        pages.push(page.events);
        let reached = match (first, stop_at) {
            (Some(first), Some(stop)) => first <= stop,
            _ => false,
        };
        match first {
            Some(first) if more && !reached => {
                before = Some(first);
                cut = page_number + 1 == MAX_PAGES;
            }
            _ => {
                cut = false;
                break;
            }
        }
    }
    pages.reverse();
    Ok(History {
        events: pages.into_iter().flatten().collect(),
        // Merging into a transcript that already has the newer events leaves nothing missing.
        cut: cut && stop_at.is_none(),
    })
}
