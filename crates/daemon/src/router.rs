//! Dispatches typed [`Request`]s to the services that own the state. Connection tasks decode;
//! the router decides.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use bytes::Bytes;
use heeler_core::TerminalId;
use heeler_protocol::control::{ErrorBody, ErrorCode};
use heeler_protocol::messages::{DaemonStatusInfo, Empty, Request, SessionCreated, Subscribed};
use heeler_protocol::{Frame, PROTOCOL_VERSION, ProtocolError};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::bus::EventBus;
use crate::conn::{ConnHandle, ConnId};
use crate::registry::RegistryHandle;
use crate::terminals::TerminalHub;

#[derive(Debug)]
pub struct Router {
    registry: RegistryHandle,
    hub: Arc<TerminalHub>,
    bus: Arc<EventBus>,
    started: Instant,
    shutdown: CancellationToken,
    clients: Arc<AtomicU32>,
}

impl Router {
    pub fn new(
        registry: RegistryHandle,
        hub: Arc<TerminalHub>,
        bus: Arc<EventBus>,
        shutdown: CancellationToken,
        clients: Arc<AtomicU32>,
    ) -> Self {
        Self {
            registry,
            hub,
            bus,
            started: Instant::now(),
            shutdown,
            clients,
        }
    }

    /// Runs one request from an already handshaken connection and returns the response result.
    pub async fn dispatch(&self, conn: &ConnHandle, request: Request) -> Result<Value, ErrorBody> {
        match request {
            Request::Hello(_) => Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "hello is only valid as the first request",
            )),
            Request::DaemonStatus(_) => {
                let counts = self.registry.session_counts().await?;
                ok(&DaemonStatusInfo {
                    daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                    protocol: PROTOCOL_VERSION,
                    epoch: self.bus.epoch(),
                    pid: std::process::id(),
                    uptime_ms: u64::try_from(self.started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    sessions: counts.total,
                    clients: self.clients.load(Ordering::Relaxed),
                })
            }
            Request::DaemonShutdown(params) => {
                let counts = self.registry.session_counts().await?;
                if counts.live > 0 && !params.stop_sessions {
                    return Err(ErrorBody::new(
                        ErrorCode::InvalidState,
                        format!(
                            "{} live session(s); pass stop_sessions to shut down anyway",
                            counts.live
                        ),
                    ));
                }
                info!(conn = %conn.id(), stop_sessions = params.stop_sessions, "shutdown requested by client");
                // The connection task queues the response before it next looks at the token,
                // and closing connections flush their queues first.
                self.shutdown.cancel();
                ok(&Empty {})
            }
            Request::EventsSubscribe(_) => ok(&Subscribed {
                seq: self.bus.subscribe(conn),
            }),
            Request::WorkspaceCreate(req) => ok(&self.registry.create_workspace(req).await?),
            Request::WorkspaceList(_) => ok(&self.registry.list_workspaces().await?),
            Request::WorkspaceRename(req) => ok(&self.registry.rename_workspace(req).await?),
            Request::WorkspaceClose(req) => {
                self.registry.close_workspace(req.workspace_id).await?;
                ok(&Empty {})
            }
            Request::SessionSnapshot(_) => ok(&self.registry.snapshot().await?),
            Request::SessionCreate(req) => ok(&SessionCreated {
                session: self.registry.create_session(req).await?,
            }),
            Request::SessionList(filter) => ok(&self.registry.list_sessions(filter).await?),
            // Stop, restart and rename answer with the session as it is afterwards.
            Request::SessionStop(req) => ok(&SessionCreated {
                session: self.registry.stop_session(req.session_id).await?,
            }),
            Request::SessionRestart(req) => ok(&SessionCreated {
                session: self.registry.restart_session(req.session_id).await?,
            }),
            Request::SessionRename(req) => ok(&SessionCreated {
                session: self.registry.rename_session(req).await?,
            }),
            Request::TerminalAttach(req) => ok(&self.hub.attach(conn, req)?),
            Request::TerminalDetach(req) => {
                self.hub.detach(conn.id(), req.terminal_id)?;
                ok(&Empty {})
            }
            Request::TerminalResize(req) => {
                self.hub.resize(req)?;
                ok(&Empty {})
            }
            Request::TerminalAck(req) => {
                self.hub.ack(conn.id(), req.terminal_id, req.frame)?;
                ok(&Empty {})
            }
            Request::TerminalHistory(req) => ok(&self.hub.history(req)?),
        }
    }

    /// Routes a kind=2 frame to its terminal. Only a malformed frame is an error (the caller
    /// closes the connection); unknown or ended terminals just drop the bytes. Waits while the
    /// terminal's input queue is full.
    pub async fn input(&self, frame: &Frame) -> Result<(), ProtocolError> {
        let (terminal, bytes): (TerminalId, Bytes) = frame.split_terminal()?;
        self.hub.input(terminal, bytes).await;
        Ok(())
    }

    /// Drops everything the closed connection had attached.
    pub fn connection_closed(&self, conn: ConnId) {
        self.hub.connection_closed(conn);
    }

    /// Terminates every live session (SIGHUP, then SIGKILL after the grace) and waits for
    /// their processes to be gone.
    pub async fn stop_sessions(&self) -> Result<(), ErrorBody> {
        self.registry.stop_all().await
    }
}

fn ok<T: Serialize>(value: &T) -> Result<Value, ErrorBody> {
    serde_json::to_value(value)
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("encoding result: {e}")))
}
