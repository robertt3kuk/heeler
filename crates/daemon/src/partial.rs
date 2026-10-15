//! Mid-stream snapshots (protocol 0.15, capability `chat.provenance`): what a client that joins
//! while the agent is still writing needs to show the text so far.
//!
//! Streaming deltas are not journaled. Per running session [`Partials`] keeps the text of every
//! open assistant message and reasoning block (bounded: one entry per open item, at most
//! [`MAX_PARTIAL_BYTES`] of text each), and the chat manager journals a `message_partial` /
//! `reasoning_partial` snapshot of the dirty ones at most every [`PARTIAL_INTERVAL`]. A
//! snapshot is *replaced in place* in the journal (the old row goes away when the new one is
//! written), so the journal never grows per delta; the item's own `*_end` event supersedes it.
//! `chat.history` answers an open item with a fresh snapshot taken at that moment, so a joiner
//! sees every delta that went out before its history reply and none that comes after.
//!
//! Everything here is plain data behind the session's lock: the chat manager takes the lock
//! around "number the event, fold it in", which makes a snapshot's `seq` mean "includes every
//! delta with a smaller `seq`".

use std::time::Duration;

use hutan_core::agent::{MessageRole, ReasoningPart};
use hutan_core::{AgentEvent, AgentEventDraft, AgentEventKind, ItemId, SessionId, TurnId};

/// Journaled snapshots of an open item are at least this far apart.
pub const PARTIAL_INTERVAL: Duration = Duration::from_millis(500);

/// Text kept per open item; deltas past it are not added (the `*_end` event has the whole text).
pub const MAX_PARTIAL_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
enum Body {
    Message { phase: Option<String>, text: String },
    Reasoning { summary: String, content: String },
}

impl Body {
    fn bytes(&self) -> usize {
        match self {
            Self::Message { text, .. } => text.len(),
            Self::Reasoning { summary, content } => summary.len() + content.len(),
        }
    }
}

#[derive(Debug)]
struct Open {
    item: ItemId,
    turn: Option<TurnId>,
    body: Body,
    /// Text arrived since the last journaled snapshot (or there is no snapshot yet).
    dirty: bool,
    /// The `seq` of this item's row in the journal.
    journaled: Option<u64>,
}

impl Open {
    fn snapshot_kind(&self) -> AgentEventKind {
        match &self.body {
            Body::Message { phase, text } => AgentEventKind::MessagePartial {
                message_id: self.item,
                phase: phase.clone(),
                text: text.clone(),
            },
            Body::Reasoning { summary, content } => AgentEventKind::ReasoningPartial {
                reasoning_id: self.item,
                summary: summary.clone(),
                content: content.clone(),
            },
        }
    }

    fn event(&self, session: SessionId, seq: u64, ts_ms: u64) -> AgentEvent {
        let mut draft = AgentEventDraft::new(self.snapshot_kind());
        draft.turn_id = self.turn;
        draft.item_id = Some(self.item);
        draft.into_event(session, seq, ts_ms)
    }
}

/// A snapshot that was just taken for the journal.
#[derive(Debug)]
pub struct Journaled {
    pub event: AgentEvent,
    /// The `seq` of the snapshot of the same item it replaces.
    pub replaces: Option<u64>,
}

/// Which open items [`Partials::flush`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flush {
    /// Those that changed since their last snapshot.
    Dirty,
    /// Those that changed, and then forget every open item: the turn is over.
    Final,
}

#[derive(Debug, Default)]
pub struct Partials {
    open: Vec<Open>,
}

impl Partials {
    /// Some open item has text its journal row does not have.
    pub fn has_dirty(&self) -> bool {
        self.open.iter().any(|open| open.dirty)
    }

    /// The items that are still streaming.
    pub fn open_items(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.open.iter().map(|open| open.item)
    }

    fn position(&self, item: ItemId) -> Option<usize> {
        self.open.iter().position(|open| open.item == item)
    }

    fn entry(&mut self, item: ItemId, turn: Option<TurnId>, make: impl FnOnce() -> Body) -> usize {
        if let Some(at) = self.position(item) {
            return at;
        }
        self.open.push(Open {
            item,
            turn,
            body: make(),
            dirty: false,
            journaled: None,
        });
        self.open.len() - 1
    }

    /// Folds in a stamped event. Returns the `seq` of the item's journaled snapshot when the
    /// event ends the item (that row is now superseded and must go).
    pub fn observe(&mut self, event: &AgentEvent) -> Option<u64> {
        let item = event.item_id?;
        let turn = event.turn_id;
        match &event.kind {
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase,
            } => {
                let at = self.entry(item, turn, || Body::Message {
                    phase: phase.clone(),
                    text: String::new(),
                });
                if let Body::Message { phase: kept, .. } = &mut self.open[at].body {
                    kept.clone_from(phase);
                }
                None
            }
            AgentEventKind::MessageDelta { text } => {
                let at = self.entry(item, turn, || Body::Message {
                    phase: None,
                    text: String::new(),
                });
                self.append(
                    at,
                    |body| match body {
                        Body::Message { text: kept, .. } => Some(kept),
                        Body::Reasoning { .. } => None,
                    },
                    text,
                );
                None
            }
            AgentEventKind::ReasoningStart => {
                self.entry(item, turn, || Body::Reasoning {
                    summary: String::new(),
                    content: String::new(),
                });
                None
            }
            AgentEventKind::ReasoningDelta { part, text } => {
                let at = self.entry(item, turn, || Body::Reasoning {
                    summary: String::new(),
                    content: String::new(),
                });
                let part = *part;
                self.append(
                    at,
                    move |body| match body {
                        Body::Reasoning { summary, content } => Some(match part {
                            ReasoningPart::Summary => summary,
                            ReasoningPart::Content => content,
                        }),
                        Body::Message { .. } => None,
                    },
                    text,
                );
                None
            }
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                ..
            }
            | AgentEventKind::ReasoningEnd { .. } => {
                let at = self.position(item)?;
                self.open.remove(at).journaled
            }
            _ => None,
        }
    }

    fn append(
        &mut self,
        at: usize,
        target: impl FnOnce(&mut Body) -> Option<&mut String>,
        text: &str,
    ) {
        let open = &mut self.open[at];
        let used = open.body.bytes();
        let Some(kept) = target(&mut open.body) else {
            return;
        };
        if used + text.len() > MAX_PARTIAL_BYTES {
            return;
        }
        kept.push_str(text);
        open.dirty = true;
    }

    /// Takes the journal snapshots due now: `next_seq` numbers each one. With
    /// [`Flush::Final`] the open items are forgotten afterwards.
    pub fn flush(
        &mut self,
        flush: Flush,
        session: SessionId,
        ts_ms: u64,
        mut next_seq: impl FnMut() -> u64,
    ) -> Vec<Journaled> {
        let mut written = Vec::new();
        for open in &mut self.open {
            if !open.dirty {
                continue;
            }
            let seq = next_seq();
            written.push(Journaled {
                event: open.event(session, seq, ts_ms),
                replaces: open.journaled.replace(seq),
            });
            open.dirty = false;
        }
        if flush == Flush::Final {
            self.open.clear();
        }
        written
    }

    /// Snapshots of every open item as they are now, for `chat.history`; nothing is journaled
    /// and the items stay as they were.
    pub fn view(
        &self,
        session: SessionId,
        ts_ms: u64,
        mut next_seq: impl FnMut() -> u64,
    ) -> Vec<AgentEvent> {
        self.open
            .iter()
            .map(|open| open.event(session, next_seq(), ts_ms))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use hutan_core::agent::MessageRole;

    use super::*;

    fn event(item: ItemId, seq: u64, kind: AgentEventKind) -> AgentEvent {
        let mut draft = AgentEventDraft::new(kind);
        draft.item_id = Some(item);
        draft.into_event(SessionId::new(), seq, 0)
    }

    fn delta(item: ItemId, seq: u64, text: &str) -> AgentEvent {
        event(
            item,
            seq,
            AgentEventKind::MessageDelta { text: text.into() },
        )
    }

    #[test]
    fn deltas_accumulate_and_flush_replaces_the_previous_snapshot() {
        let mut partials = Partials::default();
        let item = ItemId::new();
        let session = SessionId::new();
        partials.observe(&event(
            item,
            1,
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: Some("final_answer".into()),
            },
        ));
        assert!(!partials.has_dirty());
        partials.observe(&delta(item, 2, "Hel"));
        partials.observe(&delta(item, 3, "lo"));
        assert!(partials.has_dirty());

        let mut seq = 10;
        let mut next = || {
            seq += 1;
            seq
        };
        let first = partials.flush(Flush::Dirty, session, 0, &mut next);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].replaces, None);
        assert_eq!(first[0].event.seq, 11);
        assert_eq!(
            first[0].event.kind,
            AgentEventKind::MessagePartial {
                message_id: item,
                phase: Some("final_answer".into()),
                text: "Hello".into()
            }
        );
        assert!(!partials.has_dirty());
        assert!(
            partials
                .flush(Flush::Dirty, session, 0, &mut next)
                .is_empty()
        );

        partials.observe(&delta(item, 4, "!"));
        let second = partials.flush(Flush::Dirty, session, 0, &mut next);
        assert_eq!(second[0].replaces, Some(11), "replaced, not appended");

        let end = event(
            item,
            5,
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                phase: None,
                text: "Hello!".into(),
            },
        );
        assert_eq!(partials.observe(&end), Some(12), "the snapshot goes away");
        assert_eq!(partials.open_items().count(), 0);
    }

    #[test]
    fn a_final_flush_keeps_what_was_never_journaled_and_forgets_the_items() {
        let mut partials = Partials::default();
        let item = ItemId::new();
        partials.observe(&delta(item, 1, "cut off"));
        let written = partials.flush(Flush::Final, SessionId::new(), 0, || 9);
        assert_eq!(written.len(), 1);
        assert_eq!(partials.open_items().count(), 0);
    }

    #[test]
    fn reasoning_keeps_summary_and_content_apart_and_user_messages_are_ignored() {
        let mut partials = Partials::default();
        let item = ItemId::new();
        partials.observe(&event(item, 1, AgentEventKind::ReasoningStart));
        partials.observe(&event(
            item,
            2,
            AgentEventKind::ReasoningDelta {
                part: ReasoningPart::Summary,
                text: "plan".into(),
            },
        ));
        partials.observe(&event(
            item,
            3,
            AgentEventKind::ReasoningDelta {
                part: ReasoningPart::Content,
                text: "details".into(),
            },
        ));
        let user = ItemId::new();
        partials.observe(&event(
            user,
            4,
            AgentEventKind::MessageStart {
                role: MessageRole::User,
                phase: None,
            },
        ));
        let view = partials.view(SessionId::new(), 0, || 7);
        assert_eq!(view.len(), 1);
        assert_eq!(
            view[0].kind,
            AgentEventKind::ReasoningPartial {
                reasoning_id: item,
                summary: "plan".into(),
                content: "details".into()
            }
        );
        assert!(partials.has_dirty(), "viewing does not flush");
    }

    #[test]
    fn text_past_the_bound_is_not_kept() {
        let mut partials = Partials::default();
        let item = ItemId::new();
        let chunk = "x".repeat(MAX_PARTIAL_BYTES);
        partials.observe(&delta(item, 1, &chunk));
        partials.observe(&delta(item, 2, "more"));
        let view = partials.view(SessionId::new(), 0, || 3);
        let AgentEventKind::MessagePartial { text, .. } = &view[0].kind else {
            unreachable!()
        };
        assert_eq!(text.len(), MAX_PARTIAL_BYTES);
    }
}
