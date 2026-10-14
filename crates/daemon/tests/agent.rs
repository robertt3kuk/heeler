//! The agent reporting channel (roadmap M3.3, protocol 0.4) end to end: an in-process daemon
//! on a temporary `HEELER_HOME`, real PTY sessions, and a fake agent script that records the
//! command line it was resumed with.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use heeler_client::Client;
use heeler_core::{SessionId, SessionState};
use heeler_daemon::{DaemonConfig, Paths};
use heeler_protocol::ErrorCode;
use heeler_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, Empty, Event, Request,
    SessionInfo, SessionSnapshot, TerminalSpec,
};
use heeler_storage::{SessionRow, Storage};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

fn report(session: SessionId, source: &str, state: AgentState, seq: u64) -> AgentReportRequest {
    AgentReportRequest {
        session_id: session,
        source: source.into(),
        agent: "fake".into(),
        state,
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
    }
}

fn release(session: SessionId, source: &str, seq: u64) -> AgentReleaseRequest {
    AgentReleaseRequest {
        session_id: session,
        source: source.into(),
        seq,
    }
}

async fn send_report(client: &Client, req: AgentReportRequest) -> bool {
    client
        .call::<AgentApplied>(Request::AgentReport(req))
        .await
        .unwrap()
        .applied
}

async fn send_release(client: &Client, req: AgentReleaseRequest) -> bool {
    client
        .call::<AgentApplied>(Request::AgentRelease(req))
        .await
        .unwrap()
        .applied
}

async fn session_of(client: &Client, id: SessionId) -> SessionInfo {
    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    snapshot
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} missing"))
}

/// Polls the registry until `pred` holds for the session.
async fn wait_session(
    client: &Client,
    id: SessionId,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    let found = timeout(WAIT, async {
        loop {
            let session = session_of(client, id).await;
            if pred(&session) {
                return session;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    found.unwrap_or_else(|_| panic!("session {id}: timed out waiting for {what}"))
}

/// A session that stays alive until stopped.
fn idle_spec() -> TerminalSpec {
    sh("exec sleep 600")
}

/// Reads the database through a second handle until `check` holds for the session's row.
async fn eventually_stored(
    paths: &Paths,
    id: SessionId,
    what: &str,
    check: impl Fn(&SessionRow) -> bool,
) {
    let outcome = timeout(WAIT, async {
        loop {
            let storage = Storage::open(&paths.database).unwrap();
            let rows = storage.sessions().list().unwrap();
            storage.close();
            if rows.iter().any(|row| row.id == id && check(row)) {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(outcome.is_ok(), "{what}: never reached the database");
}

async fn read_when_present(path: &Path) -> String {
    timeout(WAIT, async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && text.ends_with('\n')
            {
                return text;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{} never appeared", path.display()))
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_set_the_session_agent_and_only_changes_are_events() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let session = create(&client, idle_spec()).await;
    assert!(session.agent.is_none());

    assert!(send_report(&client, report(session.id, "hooks", AgentState::Working, 1)).await);
    let updated = wait_event(&mut events, "working", |event| match event {
        Event::SessionUpdated { session: s } if s.id == session.id && s.agent.is_some() => {
            Some(s.clone())
        }
        _ => None,
    })
    .await;
    let agent = updated.agent.unwrap();
    assert_eq!(
        (agent.source.as_str(), agent.agent.as_str(), agent.state),
        ("hooks", "fake", AgentState::Working)
    );
    assert!(agent.updated_at_ms > 0);
    assert_eq!(agent.message, None);

    // The same report again changes nothing a client could see: no event.
    assert!(send_report(&client, report(session.id, "hooks", AgentState::Working, 2)).await);
    assert_no_event(&mut events, Duration::from_millis(300)).await;

    // A blocked report with a message and an id; the id then sticks to later reports.
    let mut blocked = report(session.id, "hooks", AgentState::Blocked, 3);
    blocked.message = Some("approve rm -rf build".into());
    blocked.agent_session_id = Some("abc-123".into());
    assert!(send_report(&client, blocked).await);
    let updated = wait_event(&mut events, "blocked", |event| match event {
        Event::SessionUpdated { session: s }
            if s.agent
                .as_ref()
                .is_some_and(|a| a.state == AgentState::Blocked) =>
        {
            Some(s.clone())
        }
        _ => None,
    })
    .await;
    let agent = updated.agent.unwrap();
    assert_eq!(agent.message.as_deref(), Some("approve rm -rf build"));
    assert_eq!(agent.agent_session_id.as_deref(), Some("abc-123"));

    assert!(send_report(&client, report(session.id, "hooks", AgentState::Idle, 4)).await);
    let snapshot = session_of(&client, session.id).await;
    let agent = snapshot.agent.unwrap();
    assert_eq!(agent.state, AgentState::Idle);
    assert_eq!(agent.message, None, "a new state drops the old message");
    assert_eq!(agent.agent_session_id.as_deref(), Some("abc-123"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sequence_numbers_must_increase_per_session_and_source() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let one = create(&client, idle_spec()).await;
    let two = create(&client, idle_spec()).await;

    assert!(send_report(&client, report(one.id, "a", AgentState::Working, 10)).await);
    assert!(!send_report(&client, report(one.id, "a", AgentState::Idle, 10)).await);
    assert!(!send_report(&client, report(one.id, "a", AgentState::Idle, 9)).await);
    assert_eq!(
        session_of(&client, one.id).await.agent.unwrap().state,
        AgentState::Working,
        "an ignored report changes nothing"
    );
    // Another source, and another session, have their own sequence.
    assert!(send_report(&client, report(one.id, "b", AgentState::Blocked, 1)).await);
    assert!(send_report(&client, report(two.id, "a", AgentState::Idle, 1)).await);
    // Releases are sequenced with the reports of their source.
    assert!(!send_release(&client, release(one.id, "a", 10)).await);
    assert_eq!(
        session_of(&client, one.id).await.agent.unwrap().source,
        "b",
        "the report of source b is the current one"
    );
    assert!(send_report(&client, report(one.id, "a", AgentState::Working, 11)).await);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_reports_are_rejected_without_touching_the_session() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, idle_spec()).await;

    let unknown = SessionId::new();
    let result = client
        .call::<AgentApplied>(Request::AgentReport(report(
            unknown,
            "a",
            AgentState::Idle,
            1,
        )))
        .await;
    assert_eq!(remote_code(result), ErrorCode::NotFound);
    let result = client
        .call::<AgentApplied>(Request::AgentRelease(release(unknown, "a", 1)))
        .await;
    assert_eq!(remote_code(result), ErrorCode::NotFound);

    let too_many: Vec<String> = std::iter::repeat_n("x".to_owned(), 65).collect();
    let too_long = vec!["x".to_owned(), "y".repeat(8 * 1024)];
    let cases: Vec<Vec<String>> = vec![
        Vec::new(),
        vec!["/usr/bin/codex".into(), "resume".into()],
        vec!["./codex".into()],
        vec!["codex".into(), "it's".into()],
        vec!["codex".into(), "bell\u{7}".into()],
        vec!["codex".into(), "two\nlines".into()],
        too_many,
        too_long,
    ];
    for (i, argv) in cases.into_iter().enumerate() {
        let mut req = report(session.id, "a", AgentState::Working, 100 + i as u64);
        req.resume_argv = Some(argv.clone());
        let result = client.call::<AgentApplied>(Request::AgentReport(req)).await;
        assert_eq!(remote_code(result), ErrorCode::BadRequest, "{argv:?}");
    }
    let mut nameless = report(session.id, "", AgentState::Working, 200);
    let result = client
        .call::<AgentApplied>(Request::AgentReport(nameless.clone()))
        .await;
    assert_eq!(remote_code(result), ErrorCode::BadRequest);
    nameless.source = "a".into();
    nameless.agent.clear();
    let result = client
        .call::<AgentApplied>(Request::AgentReport(nameless))
        .await;
    assert_eq!(remote_code(result), ErrorCode::BadRequest);

    assert!(
        session_of(&client, session.id).await.agent.is_none(),
        "rejected reports leave no trace"
    );
    // A rejected report does not consume its sequence number.
    assert!(send_report(&client, report(session.id, "a", AgentState::Working, 100)).await);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn release_clears_only_what_its_source_reported() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, idle_spec()).await;

    let mut first = report(session.id, "claude-hooks", AgentState::Idle, 1);
    first.resume_argv = Some(vec!["claude".into(), "--resume".into(), "abc".into()]);
    assert!(send_report(&client, first).await);
    eventually_stored(&t.paths, session.id, "the resume reference", |row| {
        row.resume_ref_json
            .as_deref()
            .is_some_and(|json| json.contains("--resume"))
    })
    .await;

    // Somebody else's release is not ours to honour.
    assert!(!send_release(&client, release(session.id, "other", 5)).await);
    assert!(session_of(&client, session.id).await.agent.is_some());

    assert!(send_release(&client, release(session.id, "claude-hooks", 2)).await);
    assert!(session_of(&client, session.id).await.agent.is_none());
    eventually_stored(&t.paths, session.id, "the cleared reference", |row| {
        row.resume_ref_json.is_none()
    })
    .await;
    // Nothing left to release.
    assert!(!send_release(&client, release(session.id, "claude-hooks", 3)).await);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_end_of_the_session_ends_the_report() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, idle_spec()).await;

    let mut req = report(session.id, "hooks", AgentState::Working, 1);
    req.resume_argv = Some(vec!["fake".into(), "--resume".into(), "x".into()]);
    assert!(send_report(&client, req).await);

    stop(&client, session.id).await.unwrap();
    let ended = session_of(&client, session.id).await;
    assert_eq!(ended.state, SessionState::Stopped);
    assert!(ended.agent.is_none());
    eventually_stored(&t.paths, session.id, "no resume reference", |row| {
        row.resume_ref_json.is_none()
    })
    .await;
    // A hook that fires after the end finds nothing to report to.
    assert!(!send_report(&client, report(session.id, "hooks", AgentState::Idle, 2)).await);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_returning_shell_prompt_clears_a_stale_report() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let flag_dir = TempDir::new().unwrap();
    let flag = flag_dir.path().join("prompt");
    let script = format!(
        "while [ ! -e '{}' ]; do sleep 0.05; done; printf '\\033]133;A\\007'; exec sleep 600",
        flag.display()
    );
    let session = create(&client, sh(&script)).await;

    let mut req = report(session.id, "hooks", AgentState::Working, 1);
    req.resume_argv = Some(vec!["fake".into(), "--resume".into(), "x".into()]);
    assert!(send_report(&client, req).await);
    wait_event(&mut events, "the report", |event| match event {
        Event::SessionUpdated { session: s } if s.id == session.id && s.agent.is_some() => Some(()),
        _ => None,
    })
    .await;

    // The "agent" is gone and the shell draws its prompt (OSC 133 A).
    std::fs::write(&flag, b"go").unwrap();
    wait_event(
        &mut events,
        "the report disappearing",
        |event| match event {
            Event::SessionUpdated { session: s } if s.id == session.id && s.agent.is_none() => {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    eventually_stored(&t.paths, session.id, "no resume reference", |row| {
        row.resume_ref_json.is_none()
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_get_the_heeler_environment_and_the_daemon_wins_over_the_spec() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::in_home(dir.path());
    let heeler_bin = PathBuf::from("/opt/heeler/bin/heeler");
    let daemon = heeler_daemon::start(DaemonConfig {
        heeler_bin: Some(heeler_bin.clone()),
        ..DaemonConfig::new(paths.clone())
    })
    .await
    .unwrap();
    let out_dir = TempDir::new().unwrap();
    let out = out_dir.path().join("env");
    let script = format!(
        "echo \"$HEELER_ENV|$HEELER_BIN|$HEELER_SESSION_ID|$HEELER_SOCKET\" > '{}'; exec sleep 600",
        out.display()
    );
    let mut spec = sh(&script);
    spec.env.insert("HEELER_ENV".into(), "0".into());
    spec.env.insert("HEELER_BIN".into(), "/evil/heeler".into());

    let client = Client::connect(heeler_client::ConnectOptions {
        socket: Some(paths.socket.clone()),
        autostart: None,
        client: heeler_protocol::messages::ClientInfo {
            kind: heeler_protocol::messages::ClientKind::Cli,
            version: "test".into(),
        },
    })
    .await
    .unwrap();
    let session = create(&client, spec).await;
    let text = read_when_present(&out).await;
    assert_eq!(
        text.trim(),
        format!(
            "1|{}|{}|{}",
            heeler_bin.display(),
            session.id,
            paths.socket.display()
        )
    );
    daemon.shutdown();
    timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
}

/// A fake agent on `PATH`: records its argv and the session id it runs in, then waits.
fn install_fake_agent(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-agent");
    std::fs::write(
        &script,
        "#!/bin/sh\n{ echo \"$HEELER_SESSION_ID\"; for a in \"$@\"; do echo \"$a\"; done; } > \"$FAKE_AGENT_OUT\"\nexec sleep 600\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_restart_resumes_the_agent_with_its_reported_command() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let bin = TempDir::new().unwrap();
    install_fake_agent(bin.path());
    let out_dir = TempDir::new().unwrap();
    let out = out_dir.path().join("argv");

    let mut spec = idle_spec();
    spec.env.insert(
        "PATH".into(),
        format!(
            "{}:{}",
            bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    spec.env
        .insert("FAKE_AGENT_OUT".into(), out.to_string_lossy().into_owned());
    let session = create(&client, spec.clone()).await;

    // The agent ran in the shell and reported how to resume it.
    let mut req = report(session.id, "fake-hooks", AgentState::Working, 5);
    req.agent_session_id = Some("conv 42".into());
    req.resume_argv = Some(vec![
        "fake-agent".into(),
        "--resume".into(),
        "conv 42".into(),
    ]);
    assert!(send_report(&client, req).await);
    eventually_stored(&t.paths, session.id, "the resume reference", |row| {
        row.resume_ref_json.is_some()
    })
    .await;
    assert!(!out.exists(), "the fake agent has not run yet");
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let resumed = wait_session(&client, session.id, "the respawn", |s| {
        s.state == SessionState::Running
    })
    .await;
    assert!(!resumed.restored);
    assert_eq!(
        resumed.command,
        ["sh", "-c", "exec sleep 600"],
        "the spec is untouched"
    );
    assert!(
        resumed.agent.is_none(),
        "the resumed agent has not reported yet"
    );
    let lines: Vec<String> = read_when_present(&out)
        .await
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        lines,
        [session.id.to_string().as_str(), "--resume", "conv 42"],
        "same session id, the reported argv"
    );

    // The new process starts a new reporting epoch: its first seq may be small again.
    assert!(
        send_report(
            &client,
            report(session.id, "fake-hooks", AgentState::Idle, 1)
        )
        .await
    );
    // A restart of an ended session uses the spec again, not the resume command.
    stop(&client, session.id).await.unwrap();
    let restarted: heeler_protocol::messages::SessionResult = client
        .call(Request::SessionRestart(
            heeler_protocol::messages::SessionRef {
                session_id: session.id,
            },
        ))
        .await
        .unwrap();
    assert!(restarted.session.agent.is_none());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_released_agent_is_not_resumed_after_a_restart() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let bin = TempDir::new().unwrap();
    install_fake_agent(bin.path());
    let out_dir = TempDir::new().unwrap();
    let out = out_dir.path().join("argv");
    let mut spec = idle_spec();
    spec.env.insert(
        "PATH".into(),
        format!(
            "{}:{}",
            bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    spec.env
        .insert("FAKE_AGENT_OUT".into(), out.to_string_lossy().into_owned());
    let session = create(&client, spec).await;

    let mut req = report(session.id, "fake-hooks", AgentState::Idle, 1);
    req.resume_argv = Some(vec!["fake-agent".into(), "--resume".into(), "x".into()]);
    assert!(send_report(&client, req).await);
    assert!(send_release(&client, release(session.id, "fake-hooks", 2)).await);
    eventually_stored(&t.paths, session.id, "the cleared reference", |row| {
        row.resume_ref_json.is_none()
    })
    .await;
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    wait_session(&client, session.id, "the respawn", |s| {
        s.state == SessionState::Running
    })
    .await;
    sleep(Duration::from_millis(300)).await;
    assert!(!out.exists(), "the plain spec command ran, not the agent");
    t.stop().await;
}
