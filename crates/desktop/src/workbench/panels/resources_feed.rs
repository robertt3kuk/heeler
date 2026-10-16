//! The resources panel's own connection to the daemon (M13-D): `resources.subscribe` on a
//! connection of its own, because the `resources.updated` stream belongs to the connection that
//! asked for it and the shared [`hutan_client::Connection`] does not surface it.
//!
//! A [`Feed`] owns one thread with a current-thread Tokio runtime (the GPUI executor has no
//! reactor, as in `signals.rs`). The thread connects, subscribes, and publishes the latest
//! sample through a `watch` channel, which any executor can await; it reconnects with backoff
//! when the link drops. Dropping the feed unsubscribes and ends the thread: nothing is sampled
//! by the daemon while no feed lives.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hutan_client::{Client, ClientError, ConnectOptions};
use hutan_protocol::messages::{ClientInfo, ClientKind, Event};
use hutan_protocol::resources::ResourceSample;
use tokio::sync::{broadcast, oneshot, watch};

/// First delay before reconnecting; doubles up to [`BACKOFF_MAX`].
const BACKOFF_START: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// How long the polite `resources.unsubscribe` may take when the feed is dropped.
const GOODBYE: Duration = Duration::from_secs(1);

/// What the feed last learned.
#[derive(Debug, Clone)]
pub enum FeedState {
    /// Dialling or subscribing.
    Connecting,
    /// The latest sample (the first one comes with the subscription's answer).
    Sample(Arc<ResourceSample>),
    /// The daemon does not advertise capability `resources` (older than protocol 0.18).
    Unsupported,
    /// The link is down; the feed keeps retrying.
    Down(String),
}

/// A live subscription. Dropping it unsubscribes.
#[derive(Debug)]
pub struct Feed {
    interval_ms: u32,
    state: watch::Receiver<FeedState>,
    /// Dropped with the feed: the thread sees the sender go and winds down.
    _stop: oneshot::Sender<()>,
}

impl Feed {
    /// Starts a feed on the daemon at `socket`, sampled every `interval_ms` (the daemon refuses
    /// less than 1000).
    pub fn start(socket: PathBuf, interval_ms: u32) -> Self {
        let (state_tx, state) = watch::channel(FeedState::Connecting);
        let (stop, stop_rx) = oneshot::channel();
        let options = ConnectOptions {
            socket: Some(socket),
            // The panel never starts the daemon; the shared connection owns that.
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Desktop,
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        };
        let failed = state_tx.clone();
        let thread = std::thread::Builder::new()
            .name("hutan-resources".to_owned())
            .spawn(move || run_thread(options, interval_ms, state_tx, stop_rx));
        if let Err(err) = thread {
            tracing::warn!("cannot start the resources feed: {err}");
            failed.send_replace(FeedState::Down(format!("cannot start the feed: {err}")));
        }
        Self {
            interval_ms,
            state,
            _stop: stop,
        }
    }

    /// The interval this feed asked for.
    pub fn interval_ms(&self) -> u32 {
        self.interval_ms
    }

    /// A receiver of the feed's states; `changed()` errors once the feed ended.
    pub fn watch(&self) -> watch::Receiver<FeedState> {
        self.state.clone()
    }
}

fn run_thread(
    options: ConnectOptions,
    interval_ms: u32,
    state: watch::Sender<FeedState>,
    stop: oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!("cannot start the resources runtime: {err}");
            state.send_replace(FeedState::Down(format!("cannot start the feed: {err}")));
            return;
        }
    };
    runtime.block_on(run(options, interval_ms, state, stop));
}

/// How one connection ended.
enum Ended {
    /// The feed was dropped.
    Stopped,
    /// The daemon cannot do resources; nothing to retry.
    Unsupported,
    /// Lost or failed; try again.
    Lost(String),
}

async fn run(
    options: ConnectOptions,
    interval_ms: u32,
    state: watch::Sender<FeedState>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut backoff = BACKOFF_START;
    loop {
        match connected(&options, interval_ms, &state, &mut backoff, &mut stop).await {
            Ended::Stopped => return,
            Ended::Unsupported => {
                state.send_replace(FeedState::Unsupported);
                // Nothing to retry; wait to be dropped.
                let _ = (&mut stop).await;
                return;
            }
            Ended::Lost(reason) => {
                // No receiver left: the panel dropped the feed.
                if state.send(FeedState::Down(reason)).is_err() {
                    return;
                }
                tokio::select! {
                    _ = &mut stop => return,
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// One connection: connect, subscribe, publish samples until the link ends or `stop` fires (then
/// the subscription is ended politely before the connection closes).
async fn connected(
    options: &ConnectOptions,
    interval_ms: u32,
    state: &watch::Sender<FeedState>,
    backoff: &mut Duration,
    stop: &mut oneshot::Receiver<()>,
) -> Ended {
    let client = tokio::select! {
        _ = &mut *stop => return Ended::Stopped,
        client = Client::connect(options.clone()) => match client {
            Ok(client) => client,
            Err(err) => return Ended::Lost(err.to_string()),
        },
    };
    if !client.supports_resources() {
        return Ended::Unsupported;
    }
    // Listen before subscribing so no update slips between the answer and the first read.
    let mut events = client.events();
    let subscribed = tokio::select! {
        _ = &mut *stop => return Ended::Stopped,
        subscribed = client.resources_subscribe(Some(interval_ms)) => subscribed,
    };
    match subscribed {
        Ok(subscribed) => {
            *backoff = BACKOFF_START;
            if state
                .send(FeedState::Sample(Arc::new(subscribed.sample)))
                .is_err()
            {
                return Ended::Stopped;
            }
        }
        Err(ClientError::Remote(error)) => {
            return Ended::Lost(format!("{:?}: {}", error.code, error.message));
        }
        Err(err) => return Ended::Lost(err.to_string()),
    }
    loop {
        tokio::select! {
            _ = &mut *stop => {
                // Best effort and bounded; closing the connection ends the subscription anyway.
                let _ = tokio::time::timeout(GOODBYE, client.resources_unsubscribe()).await;
                return Ended::Stopped;
            }
            event = events.recv() => match event {
                Ok((_, Event::ResourcesUpdated { sample })) => {
                    if state.send(FeedState::Sample(Arc::new(*sample))).is_err() {
                        return Ended::Stopped;
                    }
                }
                Ok(_) => {}
                // Updates are replaced by the next one anyway.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Ended::Lost("the daemon closed the connection".to_owned());
                }
            },
        }
    }
}
