//! SIGTERM, SIGINT and SIGHUP as a stream the GPUI main thread can await.
//!
//! The handlers are installed before [`watch`] returns, so no signal after that point kills the
//! process with the default action. A small thread with its own Tokio runtime waits for them
//! (GPUI has no reactor for signals) and forwards each one over a bounded channel; a signal
//! that arrives while the channel is full means a quit is already pending and is dropped.

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Terminate,
    Interrupt,
    Hangup,
}

/// Starts listening; the receiver yields every signal until the process exits.
pub fn watch() -> std::io::Result<mpsc::Receiver<Signal>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (mut terminate, mut interrupt, mut hangup) = {
        // Registering needs a runtime context; the streams then move to the thread.
        let _guard = runtime.enter();
        (
            signal(SignalKind::terminate())?,
            signal(SignalKind::interrupt())?,
            signal(SignalKind::hangup())?,
        )
    };
    let (tx, rx) = mpsc::channel(4);
    std::thread::Builder::new()
        .name("hutan-signals".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                loop {
                    let received = tokio::select! {
                        got = terminate.recv() => got.map(|()| Signal::Terminate),
                        got = interrupt.recv() => got.map(|()| Signal::Interrupt),
                        got = hangup.recv() => got.map(|()| Signal::Hangup),
                    };
                    let Some(received) = received else { break };
                    // Full: a quit is already queued behind an earlier signal.
                    let _ = tx.try_send(received);
                    if tx.is_closed() {
                        break;
                    }
                }
            });
        })?;
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    fn send(signal: &str) {
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(std::process::id().to_string())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn termination_signals_become_messages_instead_of_killing_the_process() {
        let mut signals = watch().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut next = |what: &str| {
            runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(10), signals.recv()).await
                })
                .unwrap_or_else(|_| panic!("no message for {what}"))
                .unwrap()
        };
        send("HUP");
        assert_eq!(next("SIGHUP"), Signal::Hangup);
        send("TERM");
        assert_eq!(next("SIGTERM"), Signal::Terminate);
        send("INT");
        assert_eq!(next("SIGINT"), Signal::Interrupt);
    }
}
