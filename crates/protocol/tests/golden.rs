//! Wire-format guard: every message has a committed fixture under `tests/fixtures/v0/`.
//! A diff here means a protocol change: bump `PROTOCOL_VERSION`, then `just update-golden`.
#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use heeler_core::{Epoch, Seq, SessionId, SessionKind, SessionState, TerminalId, WorkspaceId};
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
        restored: false,
    }
}

fn workspace_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: ws(),
        name: "heeler".into(),
        roots: vec!["/repo".into()],
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
        ("ack", ControlMessage::ok(4, &Empty {}).unwrap()),
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
