//! [`FakePoster`]: an in-memory [`NativePoster`] that records every call and lets a test click.
//! Behind the `test-support` feature (this crate's integration tests) and `cfg(test)`.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use super::{NativeNotification, NativePoster, NativeResponse, RESPONSE_QUEUE, ResponseAction};

/// How the fake answers the permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorization {
    Grant,
    Deny,
    /// No answer until [`FakePoster::answer`] (the prompt is up).
    Hold,
}

/// One call the fake received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Authorize,
    Post(NativeNotification),
    Remove(String),
}

#[derive(Debug)]
pub struct FakePoster {
    mode: Authorization,
    calls: Mutex<Vec<Call>>,
    held: Mutex<Option<oneshot::Sender<bool>>>,
    clicks: mpsc::Sender<NativeResponse>,
    responses: Mutex<Option<mpsc::Receiver<NativeResponse>>>,
}

impl FakePoster {
    pub fn new(mode: Authorization) -> Arc<Self> {
        let (clicks, responses) = mpsc::channel(RESPONSE_QUEUE);
        Arc::new(Self {
            mode,
            calls: Mutex::new(Vec::new()),
            held: Mutex::new(None),
            clicks,
            responses: Mutex::new(Some(responses)),
        })
    }

    /// Everything the fake was asked, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().clone()
    }

    pub fn posted(&self) -> Vec<NativeNotification> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Post(notification) => Some(notification),
                _ => None,
            })
            .collect()
    }

    pub fn removed(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Remove(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    pub fn authorize_calls(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| **call == Call::Authorize)
            .count()
    }

    /// Answers a held permission request.
    pub fn answer(&self, granted: bool) {
        if let Some(sender) = self.held.lock().take() {
            let _ = sender.send(granted);
        }
    }

    /// Ends a held permission request without an answer (the poster died).
    pub fn drop_authorization(&self) {
        self.held.lock().take();
    }

    /// The user clicked `action` of the notification for `intent_id`.
    pub fn click(&self, intent_id: &str, action: ResponseAction) {
        let _ = self.clicks.try_send(NativeResponse {
            intent_id: intent_id.to_owned(),
            action,
        });
    }

    /// Polls until `done` holds for the calls so far; panics after ten seconds.
    pub async fn wait_for(&self, what: &str, done: impl Fn(&[Call]) -> bool) -> Vec<Call> {
        for _ in 0..1000 {
            let calls = self.calls();
            if done(&calls) {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}; calls: {:?}", self.calls());
    }
}

impl NativePoster for FakePoster {
    fn authorize(&self) -> oneshot::Receiver<bool> {
        self.calls.lock().push(Call::Authorize);
        let (sender, receiver) = oneshot::channel();
        match self.mode {
            Authorization::Grant => {
                let _ = sender.send(true);
            }
            Authorization::Deny => {
                let _ = sender.send(false);
            }
            Authorization::Hold => *self.held.lock() = Some(sender),
        }
        receiver
    }

    fn post(&self, notification: &NativeNotification) {
        self.calls.lock().push(Call::Post(notification.clone()));
    }

    fn remove(&self, identifier: &str) {
        self.calls.lock().push(Call::Remove(identifier.to_owned()));
    }

    fn responses(&self) -> Option<mpsc::Receiver<NativeResponse>> {
        self.responses.lock().take()
    }
}
