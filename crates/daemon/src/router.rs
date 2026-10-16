//! Dispatches typed [`Request`]s to the services that own the state. Connection tasks decode;
//! the router decides.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use bytes::Bytes;
use hutan_core::TerminalId;
use hutan_protocol::chat::ChatSent;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{DaemonStatusInfo, Empty, Request, SessionResult, Subscribed};
use hutan_protocol::notify::NotifyList;
use hutan_protocol::task::TaskResult;
use hutan_protocol::{Frame, PROTOCOL_VERSION, ProtocolError};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::accounts::AccountService;
use crate::assistant::AssistantService;
use crate::bus::EventBus;
use crate::conn::{ConnHandle, ConnId};
use crate::history::HistoryService;
use crate::notify::NotifyService;
use crate::providers::ProviderService;
use crate::registry::RegistryHandle;
use crate::resources::{Actor, ResourceService, audit};
use crate::tasks::TaskService;
use crate::terminals::TerminalHub;
use crate::worktrees::WorktreeService;

/// The services a router hands requests to besides the registry and the terminal hub.
#[derive(Debug)]
pub struct Services {
    pub history: Arc<HistoryService>,
    pub assistant: Arc<AssistantService>,
    pub notify: Arc<NotifyService>,
    pub worktrees: Arc<WorktreeService>,
    pub tasks: Arc<TaskService>,
    pub accounts: Arc<AccountService>,
    pub resources: Arc<ResourceService>,
}

#[derive(Debug)]
pub struct Router {
    registry: RegistryHandle,
    hub: Arc<TerminalHub>,
    history: Arc<HistoryService>,
    assistant: Arc<AssistantService>,
    notify: Arc<NotifyService>,
    worktrees: Arc<WorktreeService>,
    tasks: Arc<TaskService>,
    accounts: Arc<AccountService>,
    resources: Arc<ResourceService>,
    bus: Arc<EventBus>,
    started: Instant,
    shutdown: CancellationToken,
    clients: Arc<AtomicU32>,
    /// The shutdown in progress asked to drop the stopped sessions from the restore set.
    forget: AtomicBool,
}

impl Router {
    pub fn new(
        registry: RegistryHandle,
        hub: Arc<TerminalHub>,
        services: Services,
        bus: Arc<EventBus>,
        shutdown: CancellationToken,
        clients: Arc<AtomicU32>,
    ) -> Self {
        Self {
            registry,
            hub,
            history: services.history,
            assistant: services.assistant,
            notify: services.notify,
            worktrees: services.worktrees,
            tasks: services.tasks,
            accounts: services.accounts,
            resources: services.resources,
            bus,
            started: Instant::now(),
            shutdown,
            clients,
            forget: AtomicBool::new(false),
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
                info!(
                    conn = %conn.id(),
                    stop_sessions = params.stop_sessions,
                    forget = params.forget,
                    "shutdown requested by client"
                );
                self.forget.store(params.forget, Ordering::Release);
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
            Request::WorkspaceOpen(req) => ok(&self.registry.open_workspace(req).await?),
            Request::WorkspaceAddRoot(req) => {
                ok(&self.registry.add_root(req.workspace_id, req.path).await?)
            }
            Request::WorkspaceRemoveRoot(req) => ok(&self
                .registry
                .remove_root(req.workspace_id, req.path)
                .await?),
            Request::WorkspaceRecent(req) => ok(&self.registry.recent_projects(req.limit).await?),
            Request::WorkspaceMove(req) => ok(&self.registry.move_workspace(req).await?),
            Request::WorkspacePin(req) => ok(&self.registry.pin_workspace(req).await?),
            Request::WorkspaceAssign(req) => ok(&self.registry.assign_workspace(req).await?),
            Request::WorkspaceDelete(req) => {
                self.registry.delete_workspace(req).await?;
                ok(&Empty {})
            }
            Request::SessionSnapshot(_) => ok(&self.registry.snapshot().await?),
            Request::SessionCreate(req) => ok(&SessionResult {
                session: self.registry.create_session(req).await?,
            }),
            Request::SessionList(filter) => ok(&self.registry.list_sessions(filter).await?),
            // Stop, restart and rename answer with the session as it is afterwards.
            Request::SessionStop(req) => {
                let stopped = self.registry.stop_session(req.session_id).await;
                audit(
                    Actor::of(conn),
                    "session.stop",
                    &req.session_id.to_string(),
                    &stopped,
                );
                ok(&SessionResult { session: stopped? })
            }
            Request::SessionKill(req) => ok(&SessionResult {
                session: self
                    .resources
                    .kill_session(Actor::of(conn), req.session_id)
                    .await?,
            }),
            Request::ProcessSignal(req) => ok(&self.resources.signal(Actor::of(conn), req).await?),
            Request::AgentInterrupt(req) => {
                ok(&self.resources.interrupt(Actor::of(conn), req).await?)
            }
            Request::ResourcesList(req) => ok(&self.resources.list(req.session_id).await?),
            Request::ResourcesSubscribe(req) => {
                ok(&self.resources.subscribe(conn, req.interval_ms).await?)
            }
            Request::ResourcesUnsubscribe(_) => {
                self.resources.unsubscribe(conn.id());
                ok(&Empty {})
            }
            Request::SessionRestart(req) => ok(&SessionResult {
                session: self.registry.restart_session(req.session_id).await?,
            }),
            Request::SessionRename(req) => ok(&SessionResult {
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
            Request::TerminalBlocks(req) => ok(&self.hub.blocks(req)?),
            Request::TerminalBlockText(req) => ok(&self.hub.block_text(req)?),
            Request::TerminalSetColors(req) => {
                self.hub.set_colors(req)?;
                ok(&Empty {})
            }
            Request::AgentReport(req) => ok(&self.registry.report_agent(req).await?),
            Request::AgentRelease(req) => ok(&self.registry.release_agent(req).await?),
            Request::AgentTree(req) => ok(&self.registry.agent_tree(req.session_id).await?),
            Request::HistorySearch(req) => ok(&self.history.search(req).await?),
            Request::HistoryStatus(_) => ok(&self.history.status().await?),
            Request::HistorySetEnabled(req) => ok(&self.history.set_enabled(req.enabled).await?),
            Request::HistoryClear(_) => {
                self.history.clear().await?;
                ok(&Empty {})
            }
            Request::HistoryRebuild(_) => {
                self.history.rebuild().await?;
                ok(&Empty {})
            }
            Request::ChatCreate(req) => ok(&SessionResult {
                session: self.registry.create_chat(req).await?,
            }),
            Request::ChatSend(req) => {
                self.live_chat(req.session_id).await?;
                // Resolved before anything is journaled or sent: a refused attachment fails
                // the whole message.
                let attached = self
                    .assistant
                    .prepare(req.session_id, &req.attachments)
                    .await?;
                let turn_id = self
                    .registry
                    .chats()
                    .send(req.session_id, req.text, attached)
                    .await?;
                ok(&ChatSent { turn_id })
            }
            Request::ChatInterrupt(req) => {
                self.live_chat(req.session_id).await?;
                self.registry.chats().interrupt(req.session_id).await?;
                ok(&Empty {})
            }
            Request::ChatHistory(req) => {
                self.registry.chat_target(req.session_id).await?;
                ok(&self.registry.chats().history(req).await?)
            }
            Request::ChatModels(req) => ok(&self.registry.chats().models(&req.agent).await?),
            Request::ApprovalList(req) => ok(&self.registry.chats().approval_list(req.session_id)),
            Request::ApprovalRespond(req) => {
                self.registry
                    .chats()
                    .respond(req.approval_id, req.option_id, conn.client_kind())
                    .await?;
                ok(&Empty {})
            }
            Request::ChatSetModel(req) => ok(&SessionResult {
                session: self.registry.set_chat_model(req).await?,
            }),
            Request::AssistantOpen(req) => ok(&SessionResult {
                session: self.assistant.open(req).await?,
            }),
            Request::AssistantList(req) => ok(&self.assistant.list(req).await?),
            Request::AssistantConfigure(req) => ok(&self.assistant.configure(req).await?),
            Request::NotifyList(_) => ok(&NotifyList {
                intents: self.notify.list(),
            }),
            Request::NotifyAck(req) => {
                self.notify.ack(&req.id)?;
                ok(&Empty {})
            }
            Request::NotifyAct(req) => ok(&self.notify.act(conn.client_kind(), req).await?),
            Request::ClientFocus(req) => {
                self.notify.set_focus(conn.id(), req.session_id);
                ok(&Empty {})
            }
            Request::WorktreeCreate(req) => ok(&self.worktrees.create(req).await?),
            Request::WorktreeList(req) => ok(&self.worktrees.list(req).await?),
            Request::WorktreeRemove(req) => ok(&self.worktrees.remove(req).await?),
            Request::TaskCreate(req) => ok(&TaskResult {
                task: self.tasks.create(req).await?,
            }),
            Request::TaskList(req) => ok(&self.tasks.list(req)),
            Request::TaskUpdate(req) => ok(&TaskResult {
                task: self.tasks.update(req).await?,
            }),
            Request::TaskDelete(req) => ok(&self.tasks.delete(req)?),
            Request::TaskStart(req) => ok(&self.tasks.start(req.id).await?),
            Request::AccountList(_) => ok(&self.accounts.list().await),
            Request::AccountLogin(req) => ok(&self.accounts.login(req).await?),
            Request::AccountLogout(req) => ok(&self.accounts.logout(req).await?),
            Request::ProviderList(_) => ok(&self.providers().list().await?),
            Request::ProviderAdd(req) => ok(&self.providers().add(req).await?),
            Request::ProviderUpdate(req) => ok(&self.providers().update(req).await?),
            Request::ProviderRemove(req) => {
                self.providers().remove(&req.provider_id).await?;
                ok(&Empty {})
            }
            Request::ProviderSetCredential(req) => ok(&self
                .providers()
                .set_credential(&req.provider_id, req.secret)
                .await?),
            Request::ProviderClearCredential(req) => {
                ok(&self.providers().clear_credential(&req.provider_id).await?)
            }
            Request::ProviderModels(req) => ok(&self.providers().models(&req.provider_id).await?),
            Request::ProviderDiscover(_) => ok(&self.providers().discover().await?),
        }
    }

    fn providers(&self) -> &Arc<ProviderService> {
        self.registry.chats().providers()
    }

    /// The session must be a chat session with a running agent.
    async fn live_chat(&self, id: hutan_core::SessionId) -> Result<(), ErrorBody> {
        if self.registry.chat_target(id).await? {
            Ok(())
        } else {
            Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("chat session {id} is not running"),
            ))
        }
    }

    /// Routes a kind=2 frame to its terminal. Only a malformed frame is an error (the caller
    /// closes the connection); unknown or ended terminals just drop the bytes. Waits while the
    /// terminal's input queue is full.
    pub async fn input(&self, frame: &Frame) -> Result<(), ProtocolError> {
        let (terminal, bytes): (TerminalId, Bytes) = frame.split_terminal()?;
        if self.hub.input(terminal, bytes).await {
            self.registry.note_activity(terminal);
        }
        Ok(())
    }

    /// Drops everything the closed connection had attached.
    pub fn connection_closed(&self, conn: ConnId) {
        self.hub.connection_closed(conn);
        self.notify.connection_closed(conn);
        self.resources.unsubscribe(conn);
    }

    /// Stops the history indexer, applying what it has queued; the index stays on disk.
    pub async fn stop_history(&self) {
        self.history.shutdown().await;
    }

    /// Terminates every live session (SIGHUP, then SIGKILL after the grace) and waits for
    /// their processes to be gone. They stay in the restore set unless the shutdown request
    /// asked to forget them.
    pub async fn stop_sessions(&self) -> Result<(), ErrorBody> {
        self.registry
            .stop_all(self.forget.load(Ordering::Acquire))
            .await
    }
}

fn ok<T: Serialize>(value: &T) -> Result<Value, ErrorBody> {
    serde_json::to_value(value)
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("encoding result: {e}")))
}
