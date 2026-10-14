//! Wire-format guard: every message has a committed fixture under `tests/fixtures/v0/`.
//! A diff here means a protocol change: bump `PROTOCOL_VERSION`, then `just update-golden`.
#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use heeler_core::{Epoch, Seq, SessionId, SessionKind, SessionState, TerminalId, WorkspaceId};
use heeler_protocol::blocks::{
    Block, BlockPart, BlockState, TerminalBlockText, TerminalBlockTextResult, TerminalBlocks,
    TerminalBlocksResult,
};
use heeler_protocol::control::ControlMessage;
use heeler_protocol::messages::*;
use heeler_protocol::render::*;
use heeler_protocol::{ErrorBody, ErrorCode, Frame, PROTOCOL_VERSION};
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

fn ws2() -> WorkspaceId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3JB".parse().unwrap()
}

fn workspace_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: ws(),
        name: "heeler".into(),
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
        roots: vec!["/Users/me/Heeler".into()],
        kind: WorkspaceKind::Home,
        pinned: false,
        position: 0,
        created_by: CreatedBy::User,
        last_active_ms: 1_791_894_160_000,
        live_sessions: 0,
        total_sessions: 0,
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
                name: "heeler".into(),
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
                state: AgentState::Blocked,
                message: Some("Bash: rm -rf build".into()),
                seq: 1_791_894_164_332_000_000,
                agent_session_id: Some("5f3e1c1a-8d51-4a6e-9a0c-2b7a3c5d9e10".into()),
                resume_argv: Some(vec![
                    "claude".into(),
                    "--resume".into(),
                    "5f3e1c1a-8d51-4a6e-9a0c-2b7a3c5d9e10".into(),
                ]),
            }),
        ),
        (
            "agent_report_minimal",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "omp-extension".into(),
                agent: "omp".into(),
                state: AgentState::Working,
                message: None,
                seq: 7,
                agent_session_id: None,
                resume_argv: None,
            }),
        ),
        (
            "agent_release",
            Request::AgentRelease(AgentReleaseRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                seq: 1_791_894_164_999_000_000,
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
    ]
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
                            name: "heeler".into(),
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
            "agent_applied",
            ControlMessage::ok(6, &AgentApplied { applied: false }).unwrap(),
        ),
        (
            "error",
            ControlMessage::err(5, ErrorBody::new(ErrorCode::NotFound, "session not found")),
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
