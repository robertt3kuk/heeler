//! Dispatches typed [`Request`]s to the services that own the state. Connection tasks decode;
//! the router decides.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use heeler_protocol::PROTOCOL_VERSION;
use heeler_protocol::control::{ErrorBody, ErrorCode};
use heeler_protocol::messages::{DaemonStatusInfo, Empty, Request, Subscribed};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::bus::EventBus;
use crate::conn::ConnHandle;
use crate::registry::RegistryHandle;

#[derive(Debug)]
pub struct Router {
    registry: RegistryHandle,
    bus: Arc<EventBus>,
    started: Instant,
    shutdown: CancellationToken,
    clients: Arc<AtomicU32>,
}

impl Router {
    pub fn new(
        registry: RegistryHandle,
        bus: Arc<EventBus>,
        shutdown: CancellationToken,
        clients: Arc<AtomicU32>,
    ) -> Self {
        Self {
            registry,
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
            Request::SessionCreate(_)
            | Request::SessionList(_)
            | Request::SessionStop(_)
            | Request::SessionRestart(_)
            | Request::SessionRename(_)
            | Request::TerminalAttach(_)
            | Request::TerminalDetach(_)
            | Request::TerminalResize(_)
            | Request::TerminalAck(_)
            | Request::TerminalHistory(_) => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "not available in this build",
            )),
        }
    }
}

fn ok<T: Serialize>(value: &T) -> Result<Value, ErrorBody> {
    serde_json::to_value(value)
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("encoding result: {e}")))
}
