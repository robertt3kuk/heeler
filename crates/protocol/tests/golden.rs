//! Wire-format guard: every message has a committed fixture under `tests/fixtures/v0/`.
//! A diff here means a protocol change: bump `PROTOCOL_VERSION`, then `just update-golden`.
#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hutan_core::agent::{
    DecidedBy, FileChange, FileChangeKind, MessageRole, PermissionKind, PermissionOption,
    PermissionOptionKind, PermissionOutcome, TokenCounts, ToolKind, ToolStatus,
};
use hutan_core::{
    AgentEvent, AgentEventDraft, AgentEventKind, AgentNode, AgentObservation, ApprovalId,
    ApprovalPolicy, ItemId, Liveness, ModelInfo, NodeKind, NodeOutcome, NodeState, SandboxMode,
    TurnId, Usage,
};
use hutan_core::{Epoch, Seq, SessionId, SessionKind, SessionState, TerminalId, WorkspaceId};
use hutan_protocol::assistant::*;
use hutan_protocol::blocks::{
    Block, BlockPart, BlockState, TerminalBlockText, TerminalBlockTextResult, TerminalBlocks,
    TerminalBlocksResult,
};
use hutan_protocol::chat::*;
use hutan_protocol::control::ControlMessage;
use hutan_protocol::history::*;
use hutan_protocol::messages::*;
use hutan_protocol::notify::{
    ActRoute, ClientFocus, IntentAction, IntentKind, NotificationIntent, NotifyAck, NotifyAct,
    NotifyActed, NotifyList,
};
use hutan_protocol::provider::*;
use hutan_protocol::render::*;
use hutan_protocol::tree::*;
use hutan_protocol::worktree::*;
use hutan_protocol::{ErrorBody, ErrorCode, Frame, PROTOCOL_VERSION};
fn ws() -> WorkspaceId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3JA".parse().unwrap()
}
fn sid() -> SessionId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3KM".parse().unwrap()
}
fn epoch() -> Epoch {
    "01K7F3B5N2Q9W8E4R6T1Y0V3PQ".parse().unwrap()
}
const TID: TerminalId = TerminalId(7);

fn size() -> TerminalSize {
    TerminalSize {
        cols: 120,
        rows: 40,
        pixel_width: 1440,
        pixel_height: 960,
    }
}

fn session_info() -> SessionInfo {
    SessionInfo {
        id: sid(),
        workspace_id: ws(),
        kind: SessionKind::Terminal,
        title: "omp".into(),
        state: SessionState::Running,
        command: vec!["omp".into()],
        cwd: Some("/repo".into()),
        terminal_id: Some(TID),
        created_at_ms: 1_791_894_164_332,
        agent: None,
        restored: false,
        chat: None,
        placement: None,
    }
}

fn finished_block() -> Block {
    Block {
        id: 12,
        prompt_line: 1500,
        command_line: Some(1500),
        output_start: Some(1501),
        end_line: Some(1504),
        command: Some("cargo test".into()),
        exit_code: Some(101),
        started_at_ms: 1_791_894_170_000,
        finished_at_ms: Some(1_791_894_171_250),
        cwd: Some("/repo".into()),
        state: BlockState::Finished,
    }
}

fn prompt_block() -> Block {
    Block {
        id: 13,
        prompt_line: 1504,
        command_line: None,
        output_start: None,
        end_line: None,
        command: None,
        exit_code: None,
        started_at_ms: 1_791_894_171_300,
        finished_at_ms: None,
        cwd: Some("/repo".into()),
        state: BlockState::Prompt,
    }
}

fn turn() -> TurnId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3TV".parse().unwrap()
}
fn item() -> ItemId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3JT".parse().unwrap()
}
fn approval() -> ApprovalId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3AP".parse().unwrap()
}

fn chat_info() -> ChatInfo {
    ChatInfo {
        agent: "codex".into(),
        model: "gpt-6.1-sol".into(),
        approval_policy: ApprovalPolicy::Untrusted,
        sandbox: SandboxMode::ReadOnly,
        turn_active: true,
        pending_approvals: 1,
        provider_id: None,
    }
}

fn direct_chat_session_info() -> SessionInfo {
    SessionInfo {
        title: "openrouter · llama-3.3-70b".into(),
        state: SessionState::Running,
        command: vec!["direct".into()],
        chat: Some(ChatInfo {
            agent: "direct".into(),
            model: "meta-llama/llama-3.3-70b-instruct".into(),
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox: SandboxMode::WorkspaceWrite,
            turn_active: true,
            pending_approvals: 0,
            provider_id: Some("openrouter".into()),
        }),
        ..chat_session_info()
    }
}

fn provider_info() -> ProviderInfo {
    ProviderInfo {
        id: "openrouter".into(),
        kind: "openai-compatible".into(),
        name: "OpenRouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        default_model: Some("meta-llama/llama-3.3-70b-instruct".into()),
        has_credential: true,
        created_at_ms: 1_791_894_100_000,
    }
}

fn local_provider_info() -> ProviderInfo {
    ProviderInfo {
        id: "ollama".into(),
        kind: "ollama".into(),
        name: "ollama".into(),
        base_url: "http://127.0.0.1:11434".into(),
        default_model: None,
        has_credential: false,
        created_at_ms: 1_791_894_110_000,
    }
}

fn chat_session_info() -> SessionInfo {
    SessionInfo {
        kind: SessionKind::Harness,
        title: "codex".into(),
        state: SessionState::WaitingApproval,
        command: vec!["codex".into()],
        terminal_id: None,
        chat: Some(chat_info()),
        ..session_info()
    }
}

/// A draft stamped as the daemon does.
fn agent_event(seq: u64, kind: AgentEventKind) -> AgentEvent {
    let mut draft = AgentEventDraft::new(kind);
    draft.turn_id = Some(turn());
    draft.item_id = Some(item());
    draft.provider.thread_id = Some("thread-1".into());
    draft.provider.turn_id = Some("turn-1".into());
    draft.provider.item_id = Some("item-1".into());
    draft.into_event(sid(), seq, 1_791_894_170_000 + seq)
}

fn attachments_meta_fixture() -> Vec<AttachmentMeta> {
    vec![
        AttachmentMeta {
            kind: AttachmentKind::ShellBlock,
            label: "cargo test".into(),
            chars: 1250,
            truncated: false,
        },
        AttachmentMeta {
            kind: AttachmentKind::File,
            label: "src/main.rs:10-40".into(),
            chars: 3000,
            truncated: true,
        },
    ]
}

fn assistant_session_info() -> SessionInfo {
    SessionInfo {
        title: "Assistant · hutan".into(),
        state: SessionState::Idle,
        command: vec!["assistant".into()],
        chat: Some(ChatInfo {
            agent: AGENT_ASSISTANT.into(),
            model: "llama3.3:70b".into(),
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox: SandboxMode::WorkspaceWrite,
            turn_active: false,
            pending_approvals: 0,
            provider_id: Some("ollama".into()),
        }),
        ..chat_session_info()
    }
}

fn permission_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption {
            id: "accept".into(),
            label: "Allow once".into(),
            kind: PermissionOptionKind::AllowOnce,
            provider: Some(serde_json::json!("accept")),
        },
        PermissionOption {
            id: "cancel".into(),
            label: "Deny and stop the turn".into(),
            kind: PermissionOptionKind::Cancel,
            provider: Some(serde_json::json!("cancel")),
        },
    ]
}

fn approval_info() -> ApprovalInfo {
    ApprovalInfo {
        id: approval(),
        session_id: sid(),
        turn_id: Some(turn()),
        kind: PermissionKind::Command,
        title: "/bin/zsh -lc 'touch hi.txt'".into(),
        detail: Some("in /repo".into()),
        options: permission_options(),
        requested_at_ms: 1_791_894_170_500,
    }
}

fn ws2() -> WorkspaceId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3JB".parse().unwrap()
}

fn workspace_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: ws(),
        name: "hutan".into(),
        roots: vec!["/repo".into(), "/other-repo".into()],
        kind: WorkspaceKind::Project,
        pinned: true,
        position: 2,
        created_by: CreatedBy::Cli,
        last_active_ms: 1_791_894_170_000,
        live_sessions: 2,
        total_sessions: 5,
    }
}

fn home_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: ws2(),
        name: "Home".into(),
        roots: vec!["/Users/me/Hutan".into()],
        kind: WorkspaceKind::Home,
        pinned: false,
        position: 0,
        created_by: CreatedBy::User,
        last_active_ms: 1_791_894_160_000,
        live_sessions: 0,
        total_sessions: 0,
    }
}

fn main_node() -> AgentNode {
    AgentNode {
        node_id: "root".into(),
        parent: None,
        kind: NodeKind::Main,
        source: "claude-hooks".into(),
        agent: "claude".into(),
        name: None,
        agent_type: None,
        state: NodeState::Blocked,
        outcome: None,
        liveness: Liveness::Alive,
        model: Some("claude-opus-5".into()),
        started_at_ms: Some(1_791_894_100_000),
        ended_at_ms: None,
        last_activity_ms: 1_791_894_170_000,
        activity: Some("permission: Bash".into()),
        transcript_path: None,
        usage: None,
    }
}

fn sub_node() -> AgentNode {
    AgentNode {
        node_id: "agent-abc123".into(),
        parent: Some("root".into()),
        kind: NodeKind::Sub,
        source: "claude-hooks".into(),
        agent: "claude".into(),
        name: Some("Explore".into()),
        agent_type: Some("Explore".into()),
        state: NodeState::Idle,
        outcome: Some(NodeOutcome::Completed),
        liveness: Liveness::Gone,
        model: Some("claude-sonnet-5".into()),
        started_at_ms: Some(1_791_894_120_000),
        ended_at_ms: Some(1_791_894_160_000),
        last_activity_ms: 1_791_894_160_000,
        activity: None,
        transcript_path: Some("/h/.claude/projects/p/subagents/agent-abc123.jsonl".into()),
        usage: Some(Usage {
            kind: "claude.context".into(),
            context_tokens: Some(48_210),
            context_window: Some(200_000),
            ..Usage::default()
        }),
    }
}

fn observation() -> AgentObservation {
    AgentObservation {
        session_id: sid(),
        root: "root".into(),
        nodes: [main_node(), sub_node()]
            .into_iter()
            .map(|n| (n.node_id.clone(), n))
            .collect(),
    }
}

fn sid2() -> SessionId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3KN".parse().unwrap()
}

fn worktree_info() -> WorktreeInfo {
    WorktreeInfo {
        path: "/work/repo-wt/feature-login".into(),
        repo_root: "/work/repo".into(),
        branch: Some("feature/login".into()),
        head: "3f786850e387550fdab836ed7e6dc881de23001b".into(),
        main: false,
        created_by_hutan: true,
        dirty: true,
        live_sessions: vec![sid()],
        locked: false,
        prunable: false,
    }
}

fn main_worktree_info() -> WorktreeInfo {
    WorktreeInfo {
        path: "/work/repo".into(),
        repo_root: "/work/repo".into(),
        branch: Some("master".into()),
        head: "89e6c98d92887913cadf06b2adb97f26cde4849b".into(),
        main: true,
        created_by_hutan: false,
        dirty: false,
        live_sessions: vec![],
        locked: false,
        prunable: false,
    }
}

fn detached_worktree_info() -> WorktreeInfo {
    WorktreeInfo {
        path: "/work/repo-wt/bisect".into(),
        repo_root: "/work/repo".into(),
        branch: None,
        head: "6b3a3b8b5a0b7c8c0f4e4c1f8e6d5c4b3a291807".into(),
        main: false,
        created_by_hutan: false,
        dirty: false,
        live_sessions: vec![],
        locked: true,
        prunable: true,
    }
}

fn requests() -> Vec<(&'static str, Request)> {
    vec![
        (
            "hello",
            Request::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                client: ClientInfo {
                    kind: ClientKind::Desktop,
                    version: "0.1.0".into(),
                },
                capabilities: vec!["terminal.render.v1".into()],
                resume: Some(ResumePoint {
                    epoch: epoch(),
                    seq: Seq(1042),
                }),
            }),
        ),
        ("daemon_status", Request::DaemonStatus(Empty {})),
        (
            "daemon_shutdown",
            Request::DaemonShutdown(DaemonShutdown {
                stop_sessions: true,
                forget: false,
            }),
        ),
        ("events_subscribe", Request::EventsSubscribe(Empty {})),
        (
            "workspace_create",
            Request::WorkspaceCreate(WorkspaceCreate {
                name: "hutan".into(),
                roots: vec!["/repo".into()],
                created_by: None,
            }),
        ),
        ("workspace_list", Request::WorkspaceList(Empty {})),
        (
            "workspace_rename",
            Request::WorkspaceRename(WorkspaceRename {
                workspace_id: ws(),
                name: "core".into(),
            }),
        ),
        (
            "workspace_close",
            Request::WorkspaceClose(WorkspaceRef { workspace_id: ws() }),
        ),
        (
            "session_create",
            Request::SessionCreate(SessionCreate {
                workspace_id: Some(ws()),
                spec: SessionSpec::Terminal(TerminalSpec {
                    command: vec!["omp".into()],
                    cwd: Some("/repo".into()),
                    env: BTreeMap::from([("RUST_LOG".into(), "debug".into())]),
                    title: None,
                    size: Some(size()),
                }),
            }),
        ),
        (
            "session_create_shell",
            Request::SessionCreate(SessionCreate {
                workspace_id: None,
                spec: SessionSpec::Terminal(TerminalSpec::default()),
            }),
        ),
        (
            "session_list",
            Request::SessionList(SessionListFilter {
                workspace_id: Some(ws()),
            }),
        ),
        ("session_snapshot", Request::SessionSnapshot(Empty {})),
        (
            "session_stop",
            Request::SessionStop(SessionRef { session_id: sid() }),
        ),
        (
            "session_restart",
            Request::SessionRestart(SessionRef { session_id: sid() }),
        ),
        (
            "session_rename",
            Request::SessionRename(SessionRename {
                session_id: sid(),
                title: "agent".into(),
            }),
        ),
        (
            "terminal_attach",
            Request::TerminalAttach(TerminalAttach {
                terminal_id: TID,
                size: Some(size()),
            }),
        ),
        (
            "terminal_detach",
            Request::TerminalDetach(TerminalRef { terminal_id: TID }),
        ),
        (
            "terminal_resize",
            Request::TerminalResize(TerminalResize {
                terminal_id: TID,
                size: size(),
            }),
        ),
        (
            "terminal_ack",
            Request::TerminalAck(TerminalAck {
                terminal_id: TID,
                frame: 42,
            }),
        ),
        (
            "terminal_history",
            Request::TerminalHistory(TerminalHistory {
                terminal_id: TID,
                from_line: 0,
                count: 100,
            }),
        ),
        (
            "terminal_set_colors",
            Request::TerminalSetColors(TerminalSetColors {
                terminal_id: TID,
                colors: TerminalColors {
                    foreground: (212, 212, 212),
                    background: (30, 30, 30),
                    cursor: (212, 212, 212),
                },
            }),
        ),
        (
            "daemon_shutdown_forget",
            Request::DaemonShutdown(DaemonShutdown {
                stop_sessions: true,
                forget: true,
            }),
        ),
        (
            "agent_report",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                agent: "claude".into(),
                state: Some(AgentState::Blocked),
                message: Some("Bash: rm -rf build".into()),
                seq: 1_791_894_164_332_000_000,
                agent_session_id: Some("5f3e1c1a-8d51-4a6e-9a0c-2b7a3c5d9e10".into()),
                resume_argv: Some(vec![
                    "claude".into(),
                    "--resume".into(),
                    "5f3e1c1a-8d51-4a6e-9a0c-2b7a3c5d9e10".into(),
                ]),
                node: None,
            }),
        ),
        (
            "agent_report_minimal",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "omp-extension".into(),
                agent: "omp".into(),
                state: Some(AgentState::Working),
                message: None,
                seq: 7,
                agent_session_id: None,
                resume_argv: None,
                node: None,
            }),
        ),
        (
            "agent_release",
            Request::AgentRelease(AgentReleaseRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                seq: 1_791_894_164_999_000_000,
                node: None,
                outcome: None,
            }),
        ),
        (
            "terminal_blocks",
            Request::TerminalBlocks(TerminalBlocks {
                terminal_id: TID,
                before_id: Some(13),
                limit: 50,
            }),
        ),
        (
            "terminal_blocks_newest",
            Request::TerminalBlocks(TerminalBlocks {
                terminal_id: TID,
                before_id: None,
                limit: 500,
            }),
        ),
        (
            "terminal_block_text",
            Request::TerminalBlockText(TerminalBlockText {
                terminal_id: TID,
                block_id: 12,
                part: BlockPart::Output,
                max_bytes: 65536,
            }),
        ),
        (
            "workspace_create_agent",
            Request::WorkspaceCreate(WorkspaceCreate {
                name: "scratch".into(),
                roots: vec![],
                created_by: Some(CreatedBy::Agent),
            }),
        ),
        (
            "workspace_open",
            Request::WorkspaceOpen(WorkspaceOpen {
                path: "/repo/crates/daemon".into(),
                created_by: None,
            }),
        ),
        (
            "workspace_open_cli",
            Request::WorkspaceOpen(WorkspaceOpen {
                path: "/repo".into(),
                created_by: Some(CreatedBy::Cli),
            }),
        ),
        (
            "workspace_add_root",
            Request::WorkspaceAddRoot(WorkspaceRoot {
                workspace_id: ws(),
                path: "/other-repo".into(),
            }),
        ),
        (
            "workspace_remove_root",
            Request::WorkspaceRemoveRoot(WorkspaceRoot {
                workspace_id: ws(),
                path: "/other-repo".into(),
            }),
        ),
        (
            "workspace_recent",
            Request::WorkspaceRecent(WorkspaceRecent { limit: None }),
        ),
        (
            "workspace_recent_limit",
            Request::WorkspaceRecent(WorkspaceRecent { limit: Some(50) }),
        ),
        (
            "workspace_move",
            Request::WorkspaceMove(WorkspaceMove {
                workspace_id: ws(),
                before: Some(ws2()),
            }),
        ),
        (
            "workspace_move_last",
            Request::WorkspaceMove(WorkspaceMove {
                workspace_id: ws(),
                before: None,
            }),
        ),
        (
            "workspace_pin",
            Request::WorkspacePin(WorkspacePin {
                workspace_id: ws(),
                pinned: true,
            }),
        ),
        (
            "history_search",
            Request::HistorySearch(HistorySearch {
                query: "cargo test -p daemon".into(),
                limit: 20,
                sources: vec![HistorySource::Claude, HistorySource::Hutan],
                roles: vec![HistoryRole::Command, HistoryRole::User],
                project: Some("/repo".into()),
                since_ms: Some(1_791_000_000_000),
                until_ms: Some(1_791_900_000_000),
            }),
        ),
        (
            "history_search_minimal",
            Request::HistorySearch(HistorySearch {
                query: "quokka".into(),
                limit: SEARCH_DEFAULT_LIMIT,
                sources: vec![],
                roles: vec![],
                project: None,
                since_ms: None,
                until_ms: None,
            }),
        ),
        ("history_status", Request::HistoryStatus(Empty {})),
        (
            "history_set_enabled",
            Request::HistorySetEnabled(HistorySetEnabled { enabled: true }),
        ),
        ("history_clear", Request::HistoryClear(Empty {})),
        ("history_rebuild", Request::HistoryRebuild(Empty {})),
        (
            "chat_create",
            Request::ChatCreate(ChatCreate {
                workspace_id: Some(ws()),
                cwd: "/repo".into(),
                agent: "codex".into(),
                model: Some("gpt-6.1-sol".into()),
                approval_policy: Some(ApprovalPolicy::Untrusted),
                sandbox: Some(SandboxMode::ReadOnly),
                title: Some("fix the build".into()),
                provider_id: None,
                system_prompt: None,
            }),
        ),
        (
            "chat_create_minimal",
            Request::ChatCreate(ChatCreate {
                workspace_id: None,
                cwd: "/repo".into(),
                agent: "codex".into(),
                model: None,
                approval_policy: None,
                sandbox: None,
                title: None,
                provider_id: None,
                system_prompt: None,
            }),
        ),
        (
            "chat_send",
            Request::ChatSend(ChatSend {
                session_id: sid(),
                text: "run the tests".into(),
                attachments: vec![],
            }),
        ),
        (
            "chat_interrupt",
            Request::ChatInterrupt(SessionRef { session_id: sid() }),
        ),
        (
            "chat_history",
            Request::ChatHistory(ChatHistoryRequest {
                session_id: sid(),
                before_seq: Some(120),
                limit: 50,
            }),
        ),
        (
            "chat_history_newest",
            Request::ChatHistory(ChatHistoryRequest {
                session_id: sid(),
                before_seq: None,
                limit: HISTORY_DEFAULT_LIMIT,
            }),
        ),
        (
            "chat_models",
            Request::ChatModels(ChatModelsRequest {
                agent: "codex".into(),
            }),
        ),
        (
            "approval_list",
            Request::ApprovalList(ApprovalListRequest { session_id: None }),
        ),
        (
            "approval_list_session",
            Request::ApprovalList(ApprovalListRequest {
                session_id: Some(sid()),
            }),
        ),
        (
            "approval_respond",
            Request::ApprovalRespond(ApprovalRespond {
                approval_id: approval(),
                option_id: "accept".into(),
            }),
        ),
        (
            "agent_report_node",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                agent: "claude".into(),
                state: Some(AgentState::Working),
                message: None,
                seq: 1_791_894_165_000_000_000,
                agent_session_id: None,
                resume_argv: None,
                node: Some(NodeReport {
                    id: "agent-abc123".into(),
                    parent: Some("root".into()),
                    kind: NodeKind::Sub,
                    name: Some("Explore".into()),
                    agent_type: Some("Explore".into()),
                    outcome: None,
                    model: Some("claude-sonnet-5".into()),
                    activity: Some("Bash: cargo test".into()),
                    transcript_path: None,
                    usage: Some(Usage {
                        kind: "claude.context".into(),
                        context_tokens: Some(48_210),
                        context_window: Some(200_000),
                        cost_usd: Some(0.0412),
                        ..Usage::default()
                    }),
                }),
            }),
        ),
        (
            "agent_report_node_minimal",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "omp-extension".into(),
                agent: "omp".into(),
                state: Some(AgentState::Idle),
                message: None,
                seq: 8,
                agent_session_id: None,
                resume_argv: None,
                node: Some(NodeReport {
                    id: "Main".into(),
                    parent: None,
                    kind: NodeKind::Main,
                    name: None,
                    agent_type: None,
                    outcome: None,
                    model: None,
                    activity: None,
                    transcript_path: None,
                    usage: None,
                }),
            }),
        ),
        (
            "agent_release_node",
            Request::AgentRelease(AgentReleaseRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                seq: 1_791_894_166_000_000_000,
                node: Some("agent-abc123".into()),
                outcome: Some(NodeOutcome::Completed),
            }),
        ),
        (
            "agent_tree",
            Request::AgentTree(AgentTreeRequest { session_id: None }),
        ),
        (
            "agent_tree_session",
            Request::AgentTree(AgentTreeRequest {
                session_id: Some(sid()),
            }),
        ),
        (
            "chat_create_direct",
            Request::ChatCreate(ChatCreate {
                workspace_id: None,
                cwd: "/repo".into(),
                agent: AGENT_DIRECT.into(),
                model: Some("meta-llama/llama-3.3-70b-instruct".into()),
                approval_policy: None,
                sandbox: None,
                title: None,
                provider_id: Some("openrouter".into()),
                system_prompt: Some("Answer in one sentence.".into()),
            }),
        ),
        (
            "chat_set_model",
            Request::ChatSetModel(ChatSetModel {
                session_id: sid(),
                provider_id: None,
                model: "llama3.3:70b".into(),
            }),
        ),
        (
            "chat_set_model_provider",
            Request::ChatSetModel(ChatSetModel {
                session_id: sid(),
                provider_id: Some("ollama".into()),
                model: "llama3.3:70b".into(),
            }),
        ),
        ("provider_list", Request::ProviderList(Empty {})),
        (
            "provider_add",
            Request::ProviderAdd(ProviderAdd {
                kind: "openai-compatible".into(),
                name: "OpenRouter".into(),
                base_url: Some("https://openrouter.ai/api/v1".into()),
                default_model: Some("meta-llama/llama-3.3-70b-instruct".into()),
            }),
        ),
        (
            "provider_add_minimal",
            Request::ProviderAdd(ProviderAdd {
                kind: "ollama".into(),
                name: "ollama".into(),
                base_url: None,
                default_model: None,
            }),
        ),
        (
            "provider_update",
            Request::ProviderUpdate(ProviderUpdate {
                provider_id: "openrouter".into(),
                name: Some("OpenRouter (work)".into()),
                base_url: None,
                default_model: Some(String::new()),
            }),
        ),
        (
            "provider_remove",
            Request::ProviderRemove(ProviderRef {
                provider_id: "openrouter".into(),
            }),
        ),
        (
            // A placeholder, not a key of any account.
            "provider_set_credential",
            Request::ProviderSetCredential(ProviderSetCredential {
                provider_id: "openrouter".into(),
                secret: "not-a-real-key".into(),
            }),
        ),
        (
            "provider_clear_credential",
            Request::ProviderClearCredential(ProviderRef {
                provider_id: "openrouter".into(),
            }),
        ),
        (
            "provider_models",
            Request::ProviderModels(ProviderRef {
                provider_id: "openrouter".into(),
            }),
        ),
        ("provider_discover", Request::ProviderDiscover(Empty {})),
        (
            "chat_send_attachments",
            Request::ChatSend(ChatSend {
                session_id: sid(),
                text: "why does this fail?".into(),
                attachments: vec![
                    Attachment::ShellBlock {
                        terminal_id: TID,
                        block_id: 12,
                    },
                    Attachment::ShellRecent {
                        terminal_id: TID,
                        lines: 80,
                    },
                    Attachment::AgentTranscript {
                        session_id: "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60".into(),
                        last_turns: 3,
                    },
                    Attachment::File {
                        path: "src/main.rs".into(),
                        range: Some(LineRange { start: 10, end: 40 }),
                    },
                    Attachment::File {
                        path: "/repo/README.md".into(),
                        range: None,
                    },
                    Attachment::Search {
                        query: "scheduler stall".into(),
                        limit: 5,
                    },
                ],
            }),
        ),
        (
            "assistant_open_global",
            Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            }),
        ),
        (
            "assistant_open_workspace_new",
            Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Workspace,
                workspace_id: Some(ws()),
                new: true,
            }),
        ),
        (
            "assistant_list",
            Request::AssistantList(AssistantListRequest {
                scope: Some(AssistantScope::Workspace),
                workspace_id: Some(ws()),
            }),
        ),
        (
            "assistant_list_all",
            Request::AssistantList(AssistantListRequest::default()),
        ),
        ("notify_list", Request::NotifyList(Empty {})),
        (
            "notify_ack",
            Request::NotifyAck(NotifyAck {
                id: "01K7F3B5N2Q9W8E4R6T1Y0V3NT".into(),
            }),
        ),
        (
            "notify_act_approval",
            Request::NotifyAct(NotifyAct {
                id: "01K7F3B5N2Q9W8E4R6T1Y0V3NT".into(),
                action_id: "accept".into(),
            }),
        ),
        (
            "notify_act_open",
            Request::NotifyAct(NotifyAct {
                id: "01K7F3B5N2Q9W8E4R6T1Y0V3NU".into(),
                action_id: hutan_protocol::notify::ACTION_OPEN.into(),
            }),
        ),
        (
            "client_focus",
            Request::ClientFocus(ClientFocus {
                session_id: Some(sid()),
            }),
        ),
        (
            "client_focus_none",
            Request::ClientFocus(ClientFocus::default()),
        ),
        (
            "worktree_create",
            Request::WorktreeCreate(WorktreeCreate {
                workspace_id: ws(),
                repo: Some("/work/repo".into()),
                branch: "feature/login".into(),
                base: Some("origin/main".into()),
                path: Some("/work/repo-wt/feature-login".into()),
            }),
        ),
        (
            "worktree_create_minimal",
            Request::WorktreeCreate(WorktreeCreate {
                workspace_id: ws(),
                repo: None,
                branch: "fix-typo".into(),
                base: None,
                path: None,
            }),
        ),
        (
            "worktree_list",
            Request::WorktreeList(WorktreeListRequest {
                workspace_id: Some(ws()),
            }),
        ),
        (
            "worktree_list_all",
            Request::WorktreeList(WorktreeListRequest::default()),
        ),
        (
            "worktree_remove",
            Request::WorktreeRemove(WorktreeRemove {
                path: "/work/repo-wt/feature-login".into(),
                force: false,
                delete_branch: false,
            }),
        ),
        (
            "worktree_remove_force_delete_branch",
            Request::WorktreeRemove(WorktreeRemove {
                path: "/work/repo-wt/feature-login".into(),
                force: true,
                delete_branch: true,
            }),
        ),
    ]
}

fn events() -> Vec<(&'static str, Event)> {
    vec![
        (
            "workspace_updated",
            Event::WorkspaceUpdated {
                workspace: workspace_info(),
            },
        ),
        (
            "workspace_closed",
            Event::WorkspaceClosed { workspace_id: ws() },
        ),
        (
            "session_updated",
            Event::SessionUpdated {
                session: session_info(),
            },
        ),
        (
            "session_exited",
            Event::SessionExited {
                session_id: sid(),
                code: Some(0),
                signal: None,
            },
        ),
        ("terminal_bell", Event::TerminalBell { terminal_id: TID }),
        (
            "terminal_title",
            Event::TerminalTitle {
                terminal_id: TID,
                title: Some("vim".into()),
            },
        ),
        (
            "terminal_notification",
            Event::TerminalNotification {
                terminal_id: TID,
                title: Some("build".into()),
                body: "done".into(),
            },
        ),
        (
            "session_updated_restored",
            Event::SessionUpdated {
                session: SessionInfo {
                    state: SessionState::Stopped,
                    terminal_id: None,
                    restored: true,
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_agent",
            Event::SessionUpdated {
                session: SessionInfo {
                    agent: Some(AgentReport {
                        source: "omp-extension".into(),
                        agent: "omp".into(),
                        state: AgentState::Working,
                        message: None,
                        agent_session_id: Some("0f4c7d52".into()),
                        updated_at_ms: 1_791_894_170_000,
                        blocked_sub: None,
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "terminal_block",
            Event::TerminalBlock {
                terminal_id: TID,
                block: finished_block(),
            },
        ),
        (
            "terminal_block_prompt",
            Event::TerminalBlock {
                terminal_id: TID,
                block: prompt_block(),
            },
        ),
        (
            "workspace_updated_home",
            Event::WorkspaceUpdated {
                workspace: home_info(),
            },
        ),
        (
            "session_updated_chat",
            Event::SessionUpdated {
                session: chat_session_info(),
            },
        ),
        (
            "chat_event_message_delta",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    41,
                    AgentEventKind::MessageDelta { text: "Hel".into() },
                )),
            },
        ),
        (
            "chat_event_message_end",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(
                    agent_event(
                        44,
                        AgentEventKind::MessageEnd {
                            role: MessageRole::Assistant,
                            phase: Some("final_answer".into()),
                            text: "Hello".into(),
                        },
                    )
                    .replayed(),
                ),
            },
        ),
        (
            "chat_event_tool_start",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    45,
                    AgentEventKind::ToolStart {
                        tool: ToolKind::Command,
                        name: "shell".into(),
                        title: "cargo test".into(),
                        input: Some(serde_json::json!({"command": "cargo test", "cwd": "/repo"})),
                    },
                )),
            },
        ),
        (
            "chat_event_tool_end",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    47,
                    AgentEventKind::ToolEnd {
                        tool: ToolKind::Command,
                        status: ToolStatus::Completed,
                        output: Some("ok\n".into()),
                        truncated: false,
                        exit_code: Some(0),
                        duration_ms: Some(1250),
                    },
                )),
            },
        ),
        (
            "chat_event_file_diff",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    48,
                    AgentEventKind::FileDiff {
                        changes: vec![FileChange {
                            path: "/repo/hello.txt".into(),
                            kind: FileChangeKind::Add,
                            move_path: None,
                            patch: Some("hi\n".into()),
                            before: None,
                            after: None,
                        }],
                        patch: None,
                    },
                )),
            },
        ),
        (
            "chat_event_permission_requested",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    49,
                    AgentEventKind::PermissionRequested {
                        approval_id: approval(),
                        kind: PermissionKind::Command,
                        title: "touch hi.txt".into(),
                        detail: None,
                        options: permission_options(),
                    },
                )),
            },
        ),
        (
            "chat_event_permission_resolved",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    50,
                    AgentEventKind::PermissionResolved {
                        approval_id: approval(),
                        outcome: PermissionOutcome::Selected {
                            option_id: "accept".into(),
                        },
                        decided_by: Some(DecidedBy {
                            client_kind: "desktop".into(),
                            client_name: None,
                        }),
                    },
                )),
            },
        ),
        (
            "chat_event_usage",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    51,
                    AgentEventKind::Usage {
                        total: Some(TokenCounts {
                            total_tokens: Some(41_338),
                            input_tokens: Some(41_285),
                            output_tokens: Some(53),
                            ..TokenCounts::default()
                        }),
                        last: None,
                        context_window: Some(258_400),
                    },
                )),
            },
        ),
        (
            "chat_event_opaque",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new({
                    let mut event = agent_event(
                        52,
                        AgentEventKind::Opaque {
                            method: "future/thing".into(),
                        },
                    );
                    event.payload = Some(serde_json::json!({"x": 1}));
                    event
                }),
            },
        ),
        (
            "chat_event_turn_completed",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    53,
                    AgentEventKind::TurnCompleted {
                        duration_ms: Some(6784),
                    },
                )),
            },
        ),
        (
            "approval_required",
            Event::ApprovalRequired {
                approval: Box::new(approval_info()),
            },
        ),
        (
            "approval_resolved_selected",
            Event::ApprovalResolved {
                approval_id: approval(),
                session_id: sid(),
                outcome: PermissionOutcome::Selected {
                    option_id: "accept".into(),
                },
                decided_by: Some(DecidedBy {
                    client_kind: "cli".into(),
                    client_name: None,
                }),
            },
        ),
        (
            "approval_resolved_expired",
            Event::ApprovalResolved {
                approval_id: approval(),
                session_id: sid(),
                outcome: PermissionOutcome::Expired,
                decided_by: None,
            },
        ),
        (
            "agent_tree_updated",
            Event::AgentTreeUpdated {
                session_id: sid(),
                nodes_changed: vec![sub_node(), main_node()],
                nodes_removed: vec!["agent-old".into()],
            },
        ),
        (
            "session_updated_agent_blocked_sub",
            Event::SessionUpdated {
                session: SessionInfo {
                    agent: Some(AgentReport {
                        source: "claude-hooks".into(),
                        agent: "claude".into(),
                        state: AgentState::Blocked,
                        message: None,
                        agent_session_id: Some("5f3e1c1a".into()),
                        updated_at_ms: 1_791_894_170_000,
                        blocked_sub: Some("Explore".into()),
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_chat_direct",
            Event::SessionUpdated {
                session: direct_chat_session_info(),
            },
        ),
        (
            "chat_event_message_end_attachments",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new({
                    let mut draft = AgentEventDraft::new(AgentEventKind::MessageEnd {
                        role: MessageRole::User,
                        phase: None,
                        text: "why does this fail?\n\n[shell: cargo test · exit 101 · 3 lines]\nerror[E0308]".into(),
                    });
                    draft.turn_id = Some(turn());
                    draft.item_id = Some(item());
                    draft.payload = Some(serde_json::json!({
                        ATTACHMENTS_META_KEY: attachments_meta_fixture(),
                    }));
                    draft.into_event(sid(), 43, 1_791_894_170_043)
                }),
            },
        ),
        (
            "notify_intent_approval",
            Event::NotifyIntent {
                intent: approval_intent(),
            },
        ),
        (
            "notify_intent_agent_finished",
            Event::NotifyIntent {
                intent: open_intent(),
            },
        ),
        (
            "ui_open",
            Event::UiOpen {
                session_id: sid(),
                workspace_id: Some(ws()),
                at_ms: 1_791_894_180_000,
            },
        ),
        (
            "ui_open_without_workspace",
            Event::UiOpen {
                session_id: sid(),
                workspace_id: None,
                at_ms: 1_791_894_180_000,
            },
        ),
        (
            "worktree_changed",
            Event::WorktreeChanged { workspace_id: ws() },
        ),
        (
            "session_updated_placement_main",
            Event::SessionUpdated {
                session: SessionInfo {
                    placement: Some(Placement {
                        kind: PlacementKind::Main,
                        root: "/work/repo".into(),
                        branch: Some("master".into()),
                        shared_with: vec![],
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_placement_worktree_shared",
            Event::SessionUpdated {
                session: SessionInfo {
                    placement: Some(Placement {
                        kind: PlacementKind::Worktree,
                        root: "/work/repo-wt/feature-login".into(),
                        branch: Some("feature/login".into()),
                        shared_with: vec![sid2()],
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_placement_detached",
            Event::SessionUpdated {
                session: SessionInfo {
                    placement: Some(Placement {
                        kind: PlacementKind::Worktree,
                        root: "/work/repo-wt/bisect".into(),
                        branch: None,
                        shared_with: vec![],
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "approval_resolved_selected_named",
            Event::ApprovalResolved {
                approval_id: approval(),
                session_id: sid(),
                outcome: PermissionOutcome::Selected {
                    option_id: "accept".into(),
                },
                decided_by: Some(DecidedBy {
                    client_kind: "desktop".into(),
                    client_name: Some("window-2".into()),
                }),
            },
        ),
        (
            "chat_event_message_partial",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(
                    agent_event(
                        52,
                        AgentEventKind::MessagePartial {
                            message_id: item(),
                            phase: Some("final_answer".into()),
                            text: "Hel".into(),
                        },
                    )
                    .replayed(),
                ),
            },
        ),
        (
            "chat_event_reasoning_partial",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(
                    agent_event(
                        53,
                        AgentEventKind::ReasoningPartial {
                            reasoning_id: item(),
                            summary: "Planning the test run".into(),
                            content: String::new(),
                        },
                    )
                    .replayed(),
                ),
            },
        ),
    ]
}

fn approval_intent() -> NotificationIntent {
    NotificationIntent {
        id: "01K7F3B5N2Q9W8E4R6T1Y0V3NT".into(),
        kind: IntentKind::ApprovalRequired,
        session_id: sid(),
        workspace_id: Some(ws()),
        title: "Run `touch hi.txt`?".into(),
        body: "Codex wants to run a command in /repo".into(),
        actions: vec![
            IntentAction {
                id: "accept".into(),
                label: "Allow once".into(),
            },
            IntentAction {
                id: "cancel".into(),
                label: "Cancel".into(),
            },
        ],
        created_at_ms: 1_791_894_170_000,
        expires_at_ms: None,
    }
}

fn open_intent() -> NotificationIntent {
    NotificationIntent {
        id: "01K7F3B5N2Q9W8E4R6T1Y0V3NU".into(),
        kind: IntentKind::AgentFinished,
        session_id: sid(),
        workspace_id: None,
        title: "omp finished".into(),
        body: "Refactored the scheduler.".into(),
        actions: vec![IntentAction {
            id: hutan_protocol::notify::ACTION_OPEN.into(),
            label: "Open".into(),
        }],
        created_at_ms: 1_791_894_170_000,
        expires_at_ms: Some(1_791_897_770_000),
    }
}

fn styled_row() -> Row {
    Row {
        runs: vec![
            Run {
                text: "$ ls".into(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::BOLD,
                underline_color: None,
                link: None,
            },
            Run {
                text: "界".into(),
                fg: Color::Indexed(196),
                bg: Color::Rgb(4, 5, 6),
                flags: CellFlags::WIDE | CellFlags::UNDERCURL,
                underline_color: Some(Color::Rgb(255, 0, 0)),
                link: Some(1),
            },
        ],
        wrapped: false,
    }
}

fn cursor() -> Cursor {
    Cursor {
        col: 4,
        row: 0,
        shape: CursorShape::Bar,
        visible: true,
        blinking: false,
    }
}

fn renders() -> Vec<(&'static str, TerminalRender)> {
    vec![
        (
            "render_snapshot",
            TerminalRender::Snapshot(TerminalSnapshot {
                frame: 1,
                cols: 80,
                rows: 2,
                cursor: cursor(),
                modes: Modes {
                    bracketed_paste: true,
                    mouse: MouseMode::Drag,
                    mouse_sgr: true,
                    kitty_keyboard: 1,
                    ..Modes::default()
                },
                palette: Palette {
                    indexed: vec![(1, (200, 0, 0))],
                    background: Some((0, 0, 0)),
                    ..Palette::default()
                },
                title: Some("zsh".into()),
                lines: vec![styled_row(), Row::default()],
                links: vec![Link {
                    id: 1,
                    uri: "https://example.com".into(),
                }],
                history_len: 120,
                viewport_top: 1480,
            }),
        ),
        (
            "render_frame",
            TerminalRender::Frame(TerminalFrame {
                frame: 2,
                scrolled: 1,
                dirty: vec![(1, styled_row())],
                cursor: cursor(),
                modes: None,
                title: Some(None),
                links: vec![],
                history_len: 121,
                viewport_top: 1481,
            }),
        ),
    ]
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0")
}

/// JSON objects are unordered on the wire, and key order depends on whether some crate in the
/// build enables `serde_json/preserve_order` (GPUI does). Fixtures therefore store, and tests
/// compare, a canonical form with recursively sorted keys. Hex fixtures are compared verbatim.
fn canonical(name: &str, actual: &str) -> String {
    if !name.ends_with(".json") {
        return actual.to_owned();
    }
    fn sort(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: BTreeMap<String, serde_json::Value> =
                    map.into_iter().map(|(k, v)| (k, sort(v))).collect();
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(sort).collect())
            }
            other => other,
        }
    }
    let value: serde_json::Value = serde_json::from_str(actual).unwrap();
    serde_json::to_string(&sort(value)).unwrap()
}

/// Compares against the committed fixture, or rewrites it when `UPDATE_GOLDEN=1`.
fn check_golden(name: &str, actual: &str) {
    let path = fixture_dir().join(name);
    let actual = canonical(name, actual);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(fixture_dir()).unwrap();
        std::fs::write(&path, format!("{actual}\n")).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing fixture {}: {e}; run `just update-golden`",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected.trim_end(),
        "wire format changed for {name}"
    );
}

#[test]
fn requests_match_fixtures_and_roundtrip() {
    for (i, (name, request)) in requests().into_iter().enumerate() {
        let id = i as u64 + 1;
        let json = String::from_utf8(
            ControlMessage::request(id, &request)
                .unwrap()
                .to_json()
                .unwrap(),
        )
        .unwrap();
        check_golden(&format!("request_{name}.json"), &json);

        let ControlMessage::Request {
            id: got_id,
            method,
            params,
        } = ControlMessage::from_json(json.as_bytes()).unwrap()
        else {
            panic!("{name}: not classified as request");
        };
        assert_eq!(got_id, id);
        assert_eq!(Request::decode(&method, params).unwrap(), request, "{name}");
    }
}

#[test]
fn events_match_fixtures_and_roundtrip() {
    for (i, (name, event)) in events().into_iter().enumerate() {
        let seq = Seq(1000 + i as u64);
        let json = String::from_utf8(
            ControlMessage::event(epoch(), seq, &event)
                .unwrap()
                .to_json()
                .unwrap(),
        )
        .unwrap();
        check_golden(&format!("event_{name}.json"), &json);

        let ControlMessage::Event {
            event: tag,
            data,
            seq: got_seq,
            ..
        } = ControlMessage::from_json(json.as_bytes()).unwrap()
        else {
            panic!("{name}: not classified as event");
        };
        assert_eq!(got_seq, seq);
        assert_eq!(Event::decode(&tag, data).unwrap(), Some(event), "{name}");
    }
}

#[test]
fn responses_match_fixtures() {
    let cases: Vec<(&str, ControlMessage)> = vec![
        (
            "welcome",
            ControlMessage::ok(
                1,
                &Welcome {
                    protocol: PROTOCOL_VERSION,
                    daemon_version: "0.1.0".into(),
                    epoch: epoch(),
                    capabilities: vec!["terminal.render.v1".into()],
                    resume: ResumeOutcome::SnapshotRequired,
                },
            )
            .unwrap(),
        ),
        (
            "session_snapshot",
            ControlMessage::ok(
                2,
                &SessionSnapshot {
                    workspaces: vec![workspace_info()],
                    sessions: vec![session_info()],
                    seq: Seq(1042),
                },
            )
            .unwrap(),
        ),
        (
            "terminal_history",
            ControlMessage::ok(
                3,
                &TerminalHistoryRows {
                    from_line: 0,
                    rows: vec![styled_row()],
                },
            )
            .unwrap(),
        ),
        (
            "terminal_blocks",
            ControlMessage::ok(
                6,
                &TerminalBlocksResult {
                    blocks: vec![prompt_block(), finished_block()],
                    viewport_top: 1480,
                },
            )
            .unwrap(),
        ),
        (
            "terminal_block_text",
            ControlMessage::ok(
                7,
                &TerminalBlockTextResult {
                    text: "running 3 tests\ntest result: FAILED".into(),
                    truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "workspace_info",
            ControlMessage::ok(8, &workspace_info()).unwrap(),
        ),
        (
            "workspace_list",
            ControlMessage::ok(
                9,
                &WorkspaceList {
                    workspaces: vec![home_info(), workspace_info()],
                },
            )
            .unwrap(),
        ),
        (
            "workspace_opened",
            ControlMessage::ok(
                10,
                &WorkspaceOpened {
                    workspace: workspace_info(),
                    created: true,
                },
            )
            .unwrap(),
        ),
        (
            "recent_projects",
            ControlMessage::ok(
                11,
                &RecentProjects {
                    projects: vec![
                        RecentProject {
                            path: "/repo".into(),
                            name: "hutan".into(),
                            last_opened_ms: 1_791_894_170_000,
                            workspace_id: Some(ws()),
                        },
                        RecentProject {
                            path: "/old/project".into(),
                            name: "project".into(),
                            last_opened_ms: 1_791_800_000_000,
                            workspace_id: None,
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        ("ack", ControlMessage::ok(4, &Empty {}).unwrap()),
        (
            "history_hits",
            ControlMessage::ok(
                7,
                &HistoryHits {
                    hits: vec![
                        HistoryHit {
                            source: HistorySource::Claude,
                            session_key: "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60".into(),
                            title: Some("Scheduler stall investigation".into()),
                            cwd: Some("/work/app".into()),
                            role: HistoryRole::User,
                            ts_ms: Some(1_791_894_164_332),
                            snippet: "why does the «quokka» scheduler stall".into(),
                            score: 4.25,
                            matches: 3,
                            resume_argv: Some(vec![
                                "claude".into(),
                                "--resume".into(),
                                "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60".into(),
                            ]),
                            session_id: None,
                            path: Some(
                                "/Users/u/.claude/projects/-work-app/5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60.jsonl"
                                    .into(),
                            ),
                            terminal_id: None,
                            block_id: None,
                        },
                        HistoryHit {
                            source: HistorySource::Hutan,
                            session_key: sid().to_string(),
                            title: None,
                            cwd: None,
                            role: HistoryRole::Output,
                            ts_ms: None,
                            snippet: "test result: ok".into(),
                            score: 1.5,
                            matches: 1,
                            resume_argv: None,
                            session_id: Some(sid()),
                            path: None,
                            terminal_id: Some(TID),
                            block_id: Some(12),
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "history_status",
            ControlMessage::ok(
                8,
                &HistoryStatusInfo {
                    enabled: true,
                    sources: vec![
                        HistorySourceStatus {
                            name: "claude".into(),
                            files: 12,
                            rows: 1790,
                            last_indexed_ms: Some(1_791_894_164_332),
                            errors: 0,
                        },
                        HistorySourceStatus {
                            name: "hutan".into(),
                            files: 0,
                            rows: 4,
                            last_indexed_ms: None,
                            errors: 0,
                        },
                    ],
                    db_bytes: 11_534_336,
                },
            )
            .unwrap(),
        ),
        (
            "agent_applied",
            ControlMessage::ok(6, &AgentApplied { applied: false }).unwrap(),
        ),
        (
            "error",
            ControlMessage::err(5, ErrorBody::new(ErrorCode::NotFound, "session not found")),
        ),
        (
            "error_option_not_offered",
            ControlMessage::err(
                6,
                ErrorBody::new(
                    ErrorCode::OptionNotOffered,
                    "option `decline` was not offered; offered: accept, cancel",
                ),
            ),
        ),
        (
            "error_forbidden",
            ControlMessage::err(
                7,
                ErrorBody::new(
                    ErrorCode::Forbidden,
                    "attachment `file` is not allowed: `assistant.context.files` is false for the global assistant",
                ),
            ),
        ),
        (
            "assistant_list",
            ControlMessage::ok(
                14,
                &AssistantList {
                    assistants: vec![
                        AssistantEntry {
                            scope: AssistantScope::Global,
                            workspace_id: ws(),
                            default: true,
                            session: assistant_session_info(),
                        },
                        AssistantEntry {
                            scope: AssistantScope::Workspace,
                            workspace_id: ws(),
                            default: false,
                            session: assistant_session_info(),
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "assistant_session_result",
            ControlMessage::ok(
                15,
                &SessionResult {
                    session: assistant_session_info(),
                },
            )
            .unwrap(),
        ),
        (
            "chat_session_result",
            ControlMessage::ok(
                12,
                &SessionResult {
                    session: chat_session_info(),
                },
            )
            .unwrap(),
        ),
        (
            "chat_sent",
            ControlMessage::ok(13, &ChatSent { turn_id: turn() }).unwrap(),
        ),
        (
            "chat_history",
            ControlMessage::ok(
                14,
                &ChatHistory {
                    events: vec![
                        agent_event(
                            44,
                            AgentEventKind::MessageEnd {
                                role: MessageRole::User,
                                phase: None,
                                text: "run the tests".into(),
                            },
                        )
                        .replayed(),
                        agent_event(46, AgentEventKind::TurnCancelled).replayed(),
                        agent_event(
                            58,
                            AgentEventKind::MessagePartial {
                                message_id: item(),
                                phase: None,
                                text: "All three".into(),
                            },
                        )
                        .replayed(),
                    ],
                    has_more: true,
                    last_seq: 60,
                },
            )
            .unwrap(),
        ),
        (
            "chat_models",
            ControlMessage::ok(
                15,
                &ChatModels {
                    agent: "codex".into(),
                    models: vec![
                        ModelInfo {
                            id: "gpt-6.1-sol".into(),
                            display_name: "GPT-6.1-Sol".into(),
                            description: "Latest workhorse model".into(),
                            is_default: true,
                            hidden: false,
                            default_reasoning_effort: Some("low".into()),
                            reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
                        },
                        ModelInfo {
                            id: "gpt-5.6-luna".into(),
                            display_name: "GPT-5.6-Luna".into(),
                            description: String::new(),
                            is_default: false,
                            hidden: false,
                            default_reasoning_effort: None,
                            reasoning_efforts: vec![],
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "approval_list",
            ControlMessage::ok(
                16,
                &ApprovalList {
                    approvals: vec![approval_info()],
                },
            )
            .unwrap(),
        ),
        (
            "agent_tree",
            ControlMessage::ok(
                17,
                &AgentTree {
                    observations: vec![observation()],
                },
            )
            .unwrap(),
        ),
        (
            "chat_session_result_direct",
            ControlMessage::ok(
                18,
                &SessionResult {
                    session: direct_chat_session_info(),
                },
            )
            .unwrap(),
        ),
        (
            "provider_list",
            ControlMessage::ok(
                19,
                &ProviderList {
                    providers: vec![provider_info(), local_provider_info()],
                },
            )
            .unwrap(),
        ),
        (
            "provider_result",
            ControlMessage::ok(
                20,
                &ProviderResult {
                    provider: provider_info(),
                },
            )
            .unwrap(),
        ),
        (
            "provider_models",
            ControlMessage::ok(
                21,
                &ProviderModels {
                    provider_id: "openrouter".into(),
                    models: vec![ModelInfo {
                        id: "meta-llama/llama-3.3-70b-instruct".into(),
                        display_name: "meta-llama/llama-3.3-70b-instruct".into(),
                        description: "meta-llama".into(),
                        is_default: true,
                        hidden: false,
                        default_reasoning_effort: None,
                        reasoning_efforts: vec![],
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "provider_discovery",
            ControlMessage::ok(
                22,
                &ProviderDiscovery {
                    found: vec![
                        DiscoveredProvider {
                            kind: "ollama".into(),
                            name: "Ollama".into(),
                            base_url: "http://127.0.0.1:11434".into(),
                            models: vec!["llama3.3:70b".into(), "qwen3:8b".into()],
                            provider_id: Some("ollama".into()),
                        },
                        DiscoveredProvider {
                            kind: "openai-compatible".into(),
                            name: "LM Studio".into(),
                            base_url: "http://127.0.0.1:1234/v1".into(),
                            models: vec![],
                            provider_id: None,
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "notify_list",
            ControlMessage::ok(
                16,
                &NotifyList {
                    intents: vec![approval_intent(), open_intent()],
                },
            )
            .unwrap(),
        ),
        (
            "notify_acted_approval",
            ControlMessage::ok(
                17,
                &NotifyActed {
                    route: ActRoute::Approval,
                    desktop_clients: 0,
                },
            )
            .unwrap(),
        ),
        (
            "notify_acted_ui_open",
            ControlMessage::ok(
                18,
                &NotifyActed {
                    route: ActRoute::UiOpen,
                    desktop_clients: 1,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_created",
            ControlMessage::ok(
                23,
                &WorktreeCreated {
                    worktree: worktree_info(),
                },
            )
            .unwrap(),
        ),
        (
            "worktree_list",
            ControlMessage::ok(
                24,
                &WorktreeList {
                    worktrees: vec![
                        main_worktree_info(),
                        worktree_info(),
                        detached_worktree_info(),
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "worktree_removed",
            ControlMessage::ok(25, &WorktreeRemoved::default()).unwrap(),
        ),
        (
            "worktree_removed_branch_kept",
            ControlMessage::ok(
                26,
                &WorktreeRemoved {
                    branch_kept: Some(
                        "error: the branch 'feature/login' is not fully merged".into(),
                    ),
                },
            )
            .unwrap(),
        ),
        (
            "session_result_placement",
            ControlMessage::ok(
                27,
                &SessionResult {
                    session: SessionInfo {
                        placement: Some(Placement {
                            kind: PlacementKind::Worktree,
                            root: "/work/repo-wt/feature-login".into(),
                            branch: Some("feature/login".into()),
                            shared_with: vec![sid2()],
                        }),
                        ..session_info()
                    },
                },
            )
            .unwrap(),
        ),
    ];
    for (name, message) in cases {
        let json = String::from_utf8(message.to_json().unwrap()).unwrap();
        check_golden(&format!("response_{name}.json"), &json);
        assert_eq!(
            ControlMessage::from_json(json.as_bytes()).unwrap(),
            message,
            "{name}"
        );
    }
}

#[test]
fn render_frames_match_fixtures_and_roundtrip() {
    for (name, render) in renders() {
        let frame = Frame::terminal_render(TID, &render).unwrap();
        let hex: String = frame.payload.iter().map(|b| format!("{b:02x}")).collect();
        check_golden(&format!("{name}.hex"), &hex);
        assert_eq!(
            frame.decode_terminal_render().unwrap(),
            (TID, render),
            "{name}"
        );
    }
}

#[test]
fn unknown_method_and_bad_params_are_bad_requests() {
    let err = Request::decode("session.teleport", serde_json::json!({})).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    let err = Request::decode("terminal.ack", serde_json::json!({"terminal_id": 1})).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn unknown_events_are_ignored_not_errors() {
    assert_eq!(
        Event::decode("agent.teleported", serde_json::json!({"x": 1})).unwrap(),
        None
    );
}

#[test]
fn workspaces_of_an_older_daemon_decode_with_defaults() {
    // Protocol 0.6 and older sent only these three fields.
    let old = serde_json::json!({"id": ws(), "name": "legacy", "roots": ["/a"]});
    let decoded: WorkspaceInfo = serde_json::from_value(old).unwrap();
    assert_eq!(
        decoded,
        WorkspaceInfo {
            id: ws(),
            name: "legacy".into(),
            roots: vec!["/a".into()],
            kind: WorkspaceKind::Project,
            pinned: false,
            position: 0,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 0,
            total_sessions: 0,
        }
    );
}

#[test]
fn sessions_of_an_older_daemon_have_no_chat_info() {
    let old = serde_json::json!({
        "id": sid(), "workspace_id": ws(), "kind": "terminal", "title": "t",
        "state": {"state": "running"}, "command": [], "cwd": null, "terminal_id": 1,
        "created_at_ms": 1
    });
    let decoded: SessionInfo = serde_json::from_value(old).unwrap();
    assert_eq!(decoded.chat, None);
}

#[test]
fn sessions_of_an_older_daemon_have_no_placement() {
    let old = serde_json::json!({
        "id": sid(), "workspace_id": ws(), "kind": "terminal", "title": "t",
        "state": {"state": "running"}, "command": [], "cwd": null, "terminal_id": 1,
        "created_at_ms": 1
    });
    let decoded: SessionInfo = serde_json::from_value(old).unwrap();
    assert_eq!(decoded.placement, None);
}

#[test]
fn history_requests_default_their_limit() {
    let request: ChatHistoryRequest =
        serde_json::from_value(serde_json::json!({"session_id": sid()})).unwrap();
    assert_eq!(request.limit, HISTORY_DEFAULT_LIMIT);
    assert_eq!(request.before_seq, None);
}

#[test]
fn events_of_a_newer_agent_decode_as_opaque_kinds_only_when_known() {
    // A `chat.event` whose kind this build does not know is a decode error for that event
    // (the client ignores it), never a panic.
    let data = serde_json::json!({
        "session_id": sid(),
        "event": {"session_id": sid(), "seq": 1, "ts_ms": 1, "type": "from_the_future"}
    });
    assert!(Event::decode("chat.event", data).is_err());
}

#[test]
fn a_credential_request_never_prints_its_secret() {
    let request = Request::ProviderSetCredential(ProviderSetCredential {
        provider_id: "openrouter".into(),
        secret: "super-secret-value".into(),
    });
    let printed = format!("{request:?}");
    assert!(!printed.contains("super-secret-value"), "{printed}");
    assert!(printed.contains("openrouter"));
}

#[test]
fn no_response_or_event_fixture_has_a_secret_field() {
    // Only `provider.set_credential` carries one, and only as a request.
    for entry in std::fs::read_dir(fixture_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("request_") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("\"secret\""), "{name} has a secret field");
        assert!(!text.contains("not-a-real-key"), "{name}");
    }
}
