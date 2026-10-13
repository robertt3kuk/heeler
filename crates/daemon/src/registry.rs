//! `SessionRegistry` actor: owns workspaces (and, from M1.5, sessions) and publishes their
//! events. Everything reaches it through one bounded mailbox; replies travel over `oneshot`s.

use std::sync::Arc;

use heeler_core::WorkspaceId;
use heeler_protocol::control::{ErrorBody, ErrorCode};
use heeler_protocol::messages::{
    Event, SessionInfo, SessionSnapshot, WorkspaceCreate, WorkspaceInfo, WorkspaceList,
    WorkspaceRename,
};
use tokio::sync::{mpsc, oneshot};
use tracing::error;

use crate::bus::{EventBus, PublishError};

/// Commands waiting in the mailbox before senders wait their turn.
const MAILBOX_CAPACITY: usize = 256;

pub const DEFAULT_WORKSPACE_NAME: &str = "default";

type Reply<T> = oneshot::Sender<Result<T, ErrorBody>>;

#[derive(Debug)]
enum Command {
    CreateWorkspace(WorkspaceCreate, Reply<WorkspaceInfo>),
    ListWorkspaces(Reply<WorkspaceList>),
    RenameWorkspace(WorkspaceRename, Reply<WorkspaceInfo>),
    CloseWorkspace(WorkspaceId, Reply<()>),
    Snapshot(Reply<SessionSnapshot>),
    SessionCounts(Reply<SessionCounts>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCounts {
    pub total: u32,
    pub live: u32,
}

/// Handle to the registry actor; cheap to clone.
#[derive(Debug, Clone)]
pub struct RegistryHandle {
    tx: mpsc::Sender<Command>,
}

impl RegistryHandle {
    async fn ask<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, ErrorBody> {
        let (reply, response) = oneshot::channel();
        let unavailable = || ErrorBody::new(ErrorCode::Internal, "session registry unavailable");
        self.tx
            .send(command(reply))
            .await
            .map_err(|_| unavailable())?;
        response.await.map_err(|_| unavailable())?
    }

    pub async fn create_workspace(&self, req: WorkspaceCreate) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::CreateWorkspace(req, r)).await
    }

    pub async fn list_workspaces(&self) -> Result<WorkspaceList, ErrorBody> {
        self.ask(Command::ListWorkspaces).await
    }

    pub async fn rename_workspace(&self, req: WorkspaceRename) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::RenameWorkspace(req, r)).await
    }

    pub async fn close_workspace(&self, id: WorkspaceId) -> Result<(), ErrorBody> {
        self.ask(|r| Command::CloseWorkspace(id, r)).await
    }

    pub async fn snapshot(&self) -> Result<SessionSnapshot, ErrorBody> {
        self.ask(Command::Snapshot).await
    }

    pub async fn session_counts(&self) -> Result<SessionCounts, ErrorBody> {
        self.ask(Command::SessionCounts).await
    }
}

#[derive(Debug)]
struct SessionRegistry {
    bus: Arc<EventBus>,
    workspaces: Vec<WorkspaceInfo>,
    sessions: Vec<SessionInfo>,
}

/// Starts the actor. It stops when every [`RegistryHandle`] is dropped.
pub fn spawn(bus: Arc<EventBus>) -> RegistryHandle {
    let (tx, rx) = mpsc::channel(MAILBOX_CAPACITY);
    let registry = SessionRegistry {
        bus,
        workspaces: vec![WorkspaceInfo {
            id: WorkspaceId::new(),
            name: DEFAULT_WORKSPACE_NAME.to_owned(),
            roots: Vec::new(),
        }],
        sessions: Vec::new(),
    };
    tokio::spawn(registry.run(rx));
    RegistryHandle { tx }
}

impl SessionRegistry {
    async fn run(mut self, mut mailbox: mpsc::Receiver<Command>) {
        while let Some(command) = mailbox.recv().await {
            // A dropped reply means the asking connection went away; nothing to do.
            match command {
                Command::CreateWorkspace(req, reply) => {
                    let _ = reply.send(self.create_workspace(req));
                }
                Command::ListWorkspaces(reply) => {
                    let _ = reply.send(Ok(WorkspaceList {
                        workspaces: self.workspaces.clone(),
                    }));
                }
                Command::RenameWorkspace(req, reply) => {
                    let _ = reply.send(self.rename_workspace(req));
                }
                Command::CloseWorkspace(id, reply) => {
                    let _ = reply.send(self.close_workspace(id));
                }
                Command::Snapshot(reply) => {
                    // The actor is the only publisher of the state it snapshots, so the bus's
                    // last seq is exactly the watermark of this state.
                    let _ = reply.send(Ok(SessionSnapshot {
                        workspaces: self.workspaces.clone(),
                        sessions: self.sessions.clone(),
                        seq: self.bus.last_seq(),
                    }));
                }
                Command::SessionCounts(reply) => {
                    let _ = reply.send(Ok(SessionCounts {
                        total: self.sessions.len() as u32,
                        live: self.sessions.iter().filter(|s| s.state.is_live()).count() as u32,
                    }));
                }
            }
        }
    }

    fn create_workspace(&mut self, req: WorkspaceCreate) -> Result<WorkspaceInfo, ErrorBody> {
        let name = validated_name(req.name)?;
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            name,
            roots: req.roots,
        };
        self.publish(&Event::WorkspaceUpdated {
            workspace: workspace.clone(),
        })?;
        self.workspaces.push(workspace.clone());
        Ok(workspace)
    }

    fn rename_workspace(&mut self, req: WorkspaceRename) -> Result<WorkspaceInfo, ErrorBody> {
        let name = validated_name(req.name)?;
        let index = self.index_of(req.workspace_id)?;
        let mut workspace = self.workspaces[index].clone();
        workspace.name = name;
        self.publish(&Event::WorkspaceUpdated {
            workspace: workspace.clone(),
        })?;
        self.workspaces[index] = workspace.clone();
        Ok(workspace)
    }

    fn close_workspace(&mut self, id: WorkspaceId) -> Result<(), ErrorBody> {
        let index = self.index_of(id)?;
        self.publish(&Event::WorkspaceClosed { workspace_id: id })?;
        self.workspaces.remove(index);
        Ok(())
    }

    fn index_of(&self, id: WorkspaceId) -> Result<usize, ErrorBody> {
        self.workspaces
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("workspace {id} not found")))
    }

    /// Publishes before the caller commits, so a rejected event leaves state untouched.
    fn publish(&self, event: &Event) -> Result<(), ErrorBody> {
        match self.bus.publish(event) {
            Ok(_) => Ok(()),
            Err(PublishError::TooLarge { len }) => Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("workspace description too large ({len} bytes)"),
            )),
            Err(e) => {
                error!(error = %e, "failed to publish event");
                Err(ErrorBody::new(
                    ErrorCode::Internal,
                    "failed to publish event",
                ))
            }
        }
    }
}

fn validated_name(name: String) -> Result<String, ErrorBody> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            "workspace name must not be empty",
        ));
    }
    Ok(trimmed.to_owned())
}
