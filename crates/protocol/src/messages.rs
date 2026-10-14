//! Typed requests, results, events, and DTOs for protocol v0.1 (`docs/protocol.md`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use heeler_core::{Epoch, Seq, SessionId, SessionKind, SessionState, TerminalId, WorkspaceId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ProtocolVersion;
use crate::blocks::{Block, TerminalBlockText, TerminalBlocks};
use crate::control::{ErrorBody, ErrorCode};
use crate::error::ProtocolError;
use crate::render::Row;

/// Parameterless request body; encoded as `{}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Empty {}

// ---------------------------------------------------------------------------------------------
// Requests

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    #[serde(rename = "hello")]
    Hello(Hello),
    #[serde(rename = "daemon.status")]
    DaemonStatus(Empty),
    #[serde(rename = "daemon.shutdown")]
    DaemonShutdown(DaemonShutdown),
    #[serde(rename = "events.subscribe")]
    EventsSubscribe(Empty),
    #[serde(rename = "workspace.create")]
    WorkspaceCreate(WorkspaceCreate),
    #[serde(rename = "workspace.list")]
    WorkspaceList(Empty),
    #[serde(rename = "workspace.rename")]
    WorkspaceRename(WorkspaceRename),
    #[serde(rename = "workspace.close")]
    WorkspaceClose(WorkspaceRef),
    #[serde(rename = "session.create")]
    SessionCreate(SessionCreate),
    #[serde(rename = "session.list")]
    SessionList(SessionListFilter),
    #[serde(rename = "session.snapshot")]
    SessionSnapshot(Empty),
    #[serde(rename = "session.stop")]
    SessionStop(SessionRef),
    #[serde(rename = "session.restart")]
    SessionRestart(SessionRef),
    #[serde(rename = "session.rename")]
    SessionRename(SessionRename),
    #[serde(rename = "terminal.attach")]
    TerminalAttach(TerminalAttach),
    #[serde(rename = "terminal.detach")]
    TerminalDetach(TerminalRef),
    #[serde(rename = "terminal.resize")]
    TerminalResize(TerminalResize),
    #[serde(rename = "terminal.ack")]
    TerminalAck(TerminalAck),
    #[serde(rename = "terminal.history")]
    TerminalHistory(TerminalHistory),
    /// Capability `terminal.colors` (protocol 0.2).
    #[serde(rename = "terminal.set_colors")]
    TerminalSetColors(TerminalSetColors),
    /// Capability `agent.report` (protocol 0.4): a coding agent reports its state and how to
    /// resume it.
    #[serde(rename = "agent.report")]
    AgentReport(AgentReportRequest),
    /// Capability `agent.report` (protocol 0.4): the agent exited; forget its report.
    #[serde(rename = "agent.release")]
    AgentRelease(AgentReleaseRequest),
    /// Capability `terminal.blocks` (protocol 0.5).
    #[serde(rename = "terminal.blocks")]
    TerminalBlocks(TerminalBlocks),
    /// Capability `terminal.blocks` (protocol 0.5).
    #[serde(rename = "terminal.block_text")]
    TerminalBlockText(TerminalBlockText),
}

impl Request {
    /// Splits into `(method, params)` for the control envelope.
    pub fn encode(&self) -> Result<(String, Value), ProtocolError> {
        split_tagged(serde_json::to_value(self)?, "method", "params")
    }

    /// Typed decoding of an envelope's `method` + `params`. Unknown methods and invalid params
    /// map to `bad_request` so the daemon can answer with the request id.
    pub fn decode(method: &str, params: Value) -> Result<Self, ErrorBody> {
        let mut obj = Map::new();
        obj.insert("method".into(), Value::String(method.to_owned()));
        obj.insert("params".into(), params);
        serde_json::from_value(Value::Object(obj))
            .map_err(|e| ErrorBody::new(ErrorCode::BadRequest, format!("`{method}`: {e}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: ProtocolVersion,
    pub client: ClientInfo,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Last event the client applied, for replay after reconnect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumePoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub kind: ClientKind,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Desktop,
    Cli,
    Notifier,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumePoint {
    pub epoch: Epoch,
    pub seq: Seq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DaemonShutdown {
    /// Stop live sessions first; otherwise shutdown fails with `invalid_state` while any run.
    #[serde(default)]
    pub stop_sessions: bool,
    /// Also remove the stopped sessions from the restore set (only meaningful with
    /// `stop_sessions`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forget: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreate {
    pub name: String,
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRename {
    pub workspace_id: WorkspaceId,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub workspace_id: WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreate {
    /// Defaults to the daemon's default workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(flatten)]
    pub spec: SessionSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionSpec {
    Terminal(TerminalSpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TerminalSpec {
    /// argv; empty runs the user's login shell.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Extra environment for this process only; daemon-managed `HEELER_*` vars win.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TerminalSize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionListFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRename {
    pub session_id: SessionId,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub pixel_width: u16,
    #[serde(default)]
    pub pixel_height: u16,
}

/// The daemon sends a `TerminalRender::Snapshot` binary frame for this terminal before the
/// response, then frames paced by `terminal.ack`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttach {
    pub terminal_id: TerminalId,
    /// Requests a resize as part of attaching (the PTY has one size shared by all clients).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TerminalSize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRef {
    pub terminal_id: TerminalId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalResize {
    pub terminal_id: TerminalId,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAck {
    pub terminal_id: TerminalId,
    pub frame: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalHistory {
    pub terminal_id: TerminalId,
    /// 0 = the scrollback line directly above the viewport; larger = older.
    pub from_line: u32,
    pub count: u32,
}

/// The client's default terminal colors, used by the daemon to answer OSC 10/11/12 queries so
/// applications see the theme the user sees. Shared by all clients of the terminal, last writer
/// wins (like `terminal.resize`). Answered with `Empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSetColors {
    pub terminal_id: TerminalId,
    pub colors: TerminalColors,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalColors {
    pub foreground: (u8, u8, u8),
    pub background: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
}

/// A coding agent running inside a session reports what it is doing and how to start it again
/// (`docs/integrations.md`). Answered with [`AgentApplied`].
///
/// `seq` must increase per `(session_id, source)`; a request that does not is ignored
/// (`applied: false`). `resume_argv`, when present, replaces the session's resume reference: a
/// plain command name on `PATH` followed by arguments, at most 64 words and 8 KiB in total, with no
/// apostrophe or control character anywhere. The daemon runs it instead of the session's
/// command after a daemon restart. Absent leaves the stored reference alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReportRequest {
    pub session_id: SessionId,
    /// Who reports (one integration, e.g. `claude-hooks`); sequencing is per source.
    pub source: String,
    /// The agent's name (`omp`, `claude`, `codex`).
    pub agent: String,
    pub state: AgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub seq: u64,
    /// The agent's own id for the conversation (what its resume command takes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_argv: Option<Vec<String>>,
}

/// The agent behind `source` is gone: clears its report and its resume reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReleaseRequest {
    pub session_id: SessionId,
    pub source: String,
    pub seq: u64,
}

/// Result of `agent.report` and `agent.release`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentApplied {
    /// `false`: ignored because `seq` was not newer, the session has ended, or (release) the
    /// source held nothing.
    pub applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Waiting for the user.
    Idle,
    /// A turn is in progress.
    Working,
    /// Waiting for a decision (permission prompt, question).
    Blocked,
}

// ---------------------------------------------------------------------------------------------
// Results

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: ProtocolVersion,
    pub daemon_version: String,
    pub epoch: Epoch,
    pub capabilities: Vec<String>,
    pub resume: ResumeOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeOutcome {
    /// Events after the client's `seq` follow immediately.
    Replayed,
    /// Client must re-fetch snapshots.
    SnapshotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatusInfo {
    pub daemon_version: String,
    pub protocol: ProtocolVersion,
    pub epoch: Epoch,
    pub pid: u32,
    pub uptime_ms: u64,
    pub sessions: u32,
    pub clients: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribed {
    /// Last event already emitted; the stream continues with `seq + 1`.
    pub seq: Seq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    pub name: String,
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceList {
    pub workspaces: Vec<WorkspaceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub kind: SessionKind,
    pub title: String,
    pub state: SessionState,
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub terminal_id: Option<TerminalId>,
    pub created_at_ms: u64,
    /// What the coding agent in this session last reported (capability `agent.report`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentReport>,
    /// The session ended before a daemon restart and its final screen is gone (true only for
    /// ended sessions loaded from storage).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restored: bool,
}

/// The latest report of a coding agent running in a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReport {
    pub source: String,
    pub agent: String,
    pub state: AgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    pub updated_at_ms: u64,
}

/// Result of `session.create`, `.stop`, `.restart`, and `.rename`: the session after the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResult {
    pub session: SessionInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionInfo>,
    pub seq: Seq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub workspaces: Vec<WorkspaceInfo>,
    pub sessions: Vec<SessionInfo>,
    /// Apply only events with a greater `seq` on top of this snapshot.
    pub seq: Seq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttached {
    /// Frame number of the snapshot already sent.
    pub frame: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalHistoryRows {
    pub from_line: u32,
    /// Newest first; may be shorter than requested at the top of history.
    pub rows: Vec<Row>,
}

/// Decodes a response `result` into the type expected for the request that was sent.
pub fn decode_result<T: DeserializeOwned>(value: Value) -> Result<T, ProtocolError> {
    Ok(serde_json::from_value(value)?)
}

// ---------------------------------------------------------------------------------------------
// Events

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
// `SessionUpdated` carries a whole `SessionInfo`; events are built once, serialised, and dropped,
// so boxing it would only complicate every consumer's pattern.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    #[serde(rename = "workspace.updated")]
    WorkspaceUpdated { workspace: WorkspaceInfo },
    #[serde(rename = "workspace.closed")]
    WorkspaceClosed { workspace_id: WorkspaceId },
    #[serde(rename = "session.updated")]
    SessionUpdated { session: SessionInfo },
    #[serde(rename = "session.exited")]
    SessionExited {
        session_id: SessionId,
        code: Option<i32>,
        signal: Option<i32>,
    },
    #[serde(rename = "terminal.bell")]
    TerminalBell { terminal_id: TerminalId },
    #[serde(rename = "terminal.title")]
    TerminalTitle {
        terminal_id: TerminalId,
        title: Option<String>,
    },
    #[serde(rename = "terminal.notification")]
    TerminalNotification {
        terminal_id: TerminalId,
        title: Option<String>,
        body: String,
    },
    /// Capability `terminal.blocks` (protocol 0.5): a block started, changed state or was
    /// truncated; `block` is the whole block as it is now.
    #[serde(rename = "terminal.block")]
    TerminalBlock {
        terminal_id: TerminalId,
        block: Block,
    },
}

impl Event {
    pub fn encode(&self) -> Result<(String, Value), ProtocolError> {
        split_tagged(serde_json::to_value(self)?, "event", "data")
    }

    /// `Ok(None)` for events this build does not know; clients ignore them (compat rule).
    pub fn decode(event: &str, data: Value) -> Result<Option<Self>, ProtocolError> {
        if !Self::NAMES.contains(&event) {
            return Ok(None);
        }
        let mut obj = Map::new();
        obj.insert("event".into(), Value::String(event.to_owned()));
        obj.insert("data".into(), data);
        Ok(Some(serde_json::from_value(Value::Object(obj))?))
    }

    pub const NAMES: &'static [&'static str] = &[
        "workspace.updated",
        "workspace.closed",
        "session.updated",
        "session.exited",
        "terminal.bell",
        "terminal.title",
        "terminal.notification",
        "terminal.block",
    ];
}

fn split_tagged(value: Value, tag: &str, content: &str) -> Result<(String, Value), ProtocolError> {
    let Value::Object(mut obj) = value else {
        return Err(ProtocolError::MalformedControl(
            "tagged enum did not encode as object",
        ));
    };
    let Some(Value::String(name)) = obj.remove(tag) else {
        return Err(ProtocolError::MalformedControl("tagged enum missing tag"));
    };
    let body = obj
        .remove(content)
        .unwrap_or_else(|| Value::Object(Map::new()));
    Ok((name, body))
}
