//! Event bus: one bounded ring per epoch with strictly increasing sequence numbers, and fan-out
//! to subscribed connections (ADR 0002).
//!
//! Publishing never blocks: a subscriber whose outbound queue is full is dropped (and its
//! connection closed) instead of slowing the bus.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;

use heeler_core::{Epoch, Seq};
use heeler_protocol::messages::{Event, ResumeOutcome, ResumePoint};
use heeler_protocol::{ControlMessage, Frame, MAX_PAYLOAD, ProtocolError};

use crate::conn::{ConnHandle, ConnId, Outbound};

/// Bounds of the replay ring. Both limits apply; the oldest events are evicted first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingLimits {
    pub max_events: usize,
    pub max_bytes: usize,
}

impl Default for RingLimits {
    fn default() -> Self {
        Self {
            max_events: 10_000,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("event of {len} bytes exceeds the {MAX_PAYLOAD}-byte frame limit")]
    TooLarge { len: usize },
    #[error(transparent)]
    Encode(#[from] ProtocolError),
}

#[derive(Debug)]
struct Entry {
    seq: Seq,
    frame: Frame,
}

#[derive(Debug)]
struct Inner {
    last: Seq,
    ring: VecDeque<Entry>,
    ring_bytes: usize,
    subscribers: HashMap<ConnId, ConnHandle>,
    /// Internal consumer of the events the history index cares about.
    tap: Option<mpsc::Sender<Event>>,
}

#[derive(Debug)]
pub struct EventBus {
    epoch: Epoch,
    limits: RingLimits,
    inner: Mutex<Inner>,
}

impl EventBus {
    pub fn new(epoch: Epoch, limits: RingLimits) -> Self {
        Self {
            epoch,
            limits,
            inner: Mutex::new(Inner {
                last: Seq::default(),
                ring: VecDeque::new(),
                ring_bytes: 0,
                subscribers: HashMap::new(),
                tap: None,
            }),
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // The critical sections never leave `Inner` half-updated across a panic point.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends a copy of every session and terminal block event, and every chat event that is
    /// journaled, to `tap` from now on (`None`
    /// stops). Never blocks: when the tap's queue is full the event is dropped for it.
    pub fn set_tap(&self, tap: Option<mpsc::Sender<Event>>) {
        self.lock().tap = tap;
    }

    /// Seq of the most recent event (`0` before the first).
    pub fn last_seq(&self) -> Seq {
        self.lock().last
    }

    pub fn subscriber_count(&self) -> usize {
        self.lock().subscribers.len()
    }

    /// Assigns the next seq, retains the event in the ring, and queues it to every subscriber.
    /// An event that cannot be framed is rejected without consuming a seq.
    pub fn publish(&self, event: &Event) -> Result<Seq, PublishError> {
        let mut inner = self.lock();
        let seq = inner.last.next();
        let frame = ControlMessage::event(self.epoch, seq, event)?.to_frame()?;
        let len = frame.payload.len();
        if len > MAX_PAYLOAD {
            return Err(PublishError::TooLarge { len });
        }
        inner.last = seq;
        inner.ring_bytes += len;
        inner.ring.push_back(Entry {
            seq,
            frame: frame.clone(),
        });
        while inner.ring.len() > self.limits.max_events || inner.ring_bytes > self.limits.max_bytes
        {
            let Some(evicted) = inner.ring.pop_front() else {
                break;
            };
            inner.ring_bytes -= evicted.frame.payload.len();
        }
        inner
            .subscribers
            .retain(|_, sub| sub.enqueue(Outbound::Frame(frame.clone())));
        // Session and block events, and the journaled (not streamed) events of chats.
        let tapped = match event {
            Event::SessionUpdated { .. } | Event::TerminalBlock { .. } => true,
            Event::ChatEvent { event, .. } => !event.kind.is_streaming(),
            _ => false,
        };
        if tapped && let Some(tap) = &inner.tap {
            match tap.try_send(event.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::debug!("history tap is full; dropped an event");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => inner.tap = None,
            }
        }
        Ok(seq)
    }

    /// Completes a handshake atomically with respect to publishing: queues the welcome built by
    /// `welcome`, then (if the client can resume) every missed event, and subscribes the
    /// connection so no event is lost or duplicated in between.
    pub fn open(
        &self,
        conn: &ConnHandle,
        resume: Option<ResumePoint>,
        welcome: impl FnOnce(ResumeOutcome) -> Result<Frame, ProtocolError>,
    ) -> Result<ResumeOutcome, ProtocolError> {
        let mut inner = self.lock();
        let replay = resume
            .filter(|r| r.epoch == self.epoch)
            .and_then(|r| inner.replay_after(r.seq));
        let outcome = match replay {
            Some(_) => ResumeOutcome::Replayed,
            None => ResumeOutcome::SnapshotRequired,
        };
        let welcome = welcome(outcome)?;
        if !conn.enqueue(Outbound::Frame(welcome)) {
            return Ok(outcome);
        }
        if let Some(missed) = replay {
            if !missed.is_empty() && !conn.enqueue(Outbound::Batch(missed)) {
                return Ok(outcome);
            }
            inner.subscribers.insert(conn.id(), conn.clone());
        }
        Ok(outcome)
    }

    /// Registers `conn` for every subsequent event and returns the last seq already emitted.
    pub fn subscribe(&self, conn: &ConnHandle) -> Seq {
        let mut inner = self.lock();
        inner.subscribers.insert(conn.id(), conn.clone());
        inner.last
    }

    pub fn unsubscribe(&self, id: ConnId) {
        self.lock().subscribers.remove(&id);
    }
}

impl Inner {
    /// Frames of every event after `seq`, or `None` if some were evicted (or `seq` is ahead of
    /// anything emitted, which can only come from a confused client).
    fn replay_after(&self, seq: Seq) -> Option<Vec<Frame>> {
        if seq > self.last {
            return None;
        }
        let oldest = self.ring.front().map_or(self.last.next(), |e| e.seq);
        if seq.next() < oldest {
            return None;
        }
        Some(
            self.ring
                .iter()
                .filter(|e| e.seq > seq)
                .map(|e| e.frame.clone())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use heeler_core::WorkspaceId;

    use super::*;

    fn event() -> Event {
        Event::WorkspaceClosed {
            workspace_id: WorkspaceId::new(),
        }
    }

    fn bus(max_events: usize, max_bytes: usize) -> EventBus {
        EventBus::new(
            Epoch::new(),
            RingLimits {
                max_events,
                max_bytes,
            },
        )
    }

    #[test]
    fn seq_is_strictly_increasing_from_one() {
        let bus = bus(10, usize::MAX);
        assert_eq!(bus.last_seq(), Seq(0));
        assert_eq!(bus.publish(&event()).unwrap(), Seq(1));
        assert_eq!(bus.publish(&event()).unwrap(), Seq(2));
        assert_eq!(bus.last_seq(), Seq(2));
    }

    #[test]
    fn ring_is_bounded_by_count() {
        let bus = bus(3, usize::MAX);
        for _ in 0..5 {
            bus.publish(&event()).unwrap();
        }
        let inner = bus.lock();
        assert_eq!(inner.ring.len(), 3);
        assert_eq!(inner.ring.front().unwrap().seq, Seq(3));
    }

    #[test]
    fn ring_is_bounded_by_bytes() {
        let bus = bus(100, 1);
        bus.publish(&event()).unwrap();
        bus.publish(&event()).unwrap();
        let inner = bus.lock();
        assert!(inner.ring.is_empty());
        assert_eq!(inner.ring_bytes, 0);
    }

    #[test]
    fn replay_window() {
        let bus = bus(3, usize::MAX);
        for _ in 0..5 {
            bus.publish(&event()).unwrap();
        }
        let inner = bus.lock();
        // ring holds 3..=5
        assert_eq!(inner.replay_after(Seq(5)).unwrap().len(), 0);
        assert_eq!(inner.replay_after(Seq(2)).unwrap().len(), 3);
        assert_eq!(inner.replay_after(Seq(3)).unwrap().len(), 2);
        assert!(inner.replay_after(Seq(1)).is_none());
        assert!(inner.replay_after(Seq(6)).is_none());
    }

    #[test]
    fn replay_from_zero_on_fresh_bus() {
        let bus = bus(3, usize::MAX);
        assert_eq!(bus.lock().replay_after(Seq(0)).unwrap().len(), 0);
    }
}
