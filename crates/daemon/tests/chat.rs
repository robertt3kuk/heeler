//! Chat sessions end to end: a real daemon on a temporary `HEELER_HOME`, real client
//! connections, and `heeler-codex-fixture replay` standing in for `codex app-server` with
//! transcripts recorded from codex 0.162.1.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create, remote_code, spec, stop, wait_event};
use heeler_agents::CodexConfig;
use heeler_client::Client;
use heeler_core::agent::PermissionOutcome;
use heeler_core::{AgentEvent, AgentEventKind, ApprovalId, ApprovalPolicy, SandboxMode, SessionId};
use heeler_protocol::ErrorCode;
use heeler_protocol::chat::{
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatCreate, ChatHistory,
    ChatHistoryRequest, ChatModels, ChatModelsRequest, ChatSend, ChatSent,
};
use heeler_protocol::messages::{
    Empty, Event, Request, SessionInfo, SessionList, SessionListFilter, SessionRef, SessionResult,
};
use heeler_core::{Seq, SessionState};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

const FIXTURE_BIN_NAME: &str = "heeler-codex-fixture";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

/// The fake app server, next to the test binaries; `just test` builds the workspace binaries
/// first.
fn fixture_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join(FIXTURE_BIN_NAME);
    assert!(
        bin.is_file(),
        "{FIXTURE_BIN_NAME} not built: run `cargo build -p heeler-agents --bins` (or `just test`)"
    );
    bin
}

/// A codex that replays `transcript` (a file, or a directory of transcripts used one per
/// spawn), recording what the client sends into `received`.
fn replay(transcript: &Path, received: &Path) -> CodexConfig {
    CodexConfig {
        program: fixture_bin(),
        args: vec![
            "replay".to_owned(),
            "--received".to_owned(),
            received.display().to_string(),
            transcript.display().to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    }
}

struct Chat {
    daemon: TestDaemon,
    /// Where the agent works; also holds the scripted transcripts and the received log.
    work: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

impl Chat {
    /// A daemon whose codex replays the committed fixture `name`.
    async fn with_fixture(name: &str) -> Self {
        let transcript = fixtures().join(format!("{name}.jsonl"));
        Self::with_transcripts(&transcript, |_| {}).await
    }

    /// A daemon whose codex replays `transcript`; `prepare` may write files into the work dir
    /// first.
    async fn with_transcripts(transcript: &Path, prepare: impl FnOnce(&Path)) -> Self {
        let work = tempfile::tempdir().unwrap();
        prepare(work.path());
        let config = replay(transcript, &work.path().join("received.jsonl"));
        let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    fn received_path(&self) -> PathBuf {
        self.work.path().join("received.jsonl")
    }

    /// Everything the fake server received, as `(method or "response", message)`.
    fn received(&self) -> Vec<(String, Value)> {
        received_in(&self.received_path())
    }

    fn cwd(&self) -> PathBuf {
        // The agent's directory: the (canonical) work dir itself.
        self.work.path().to_owned()
    }

    async fn create(&self) -> SessionInfo {
        self.create_with(None, None, None).await
    }

    async fn create_with(
        &self,
        model: Option<&str>,
        policy: Option<ApprovalPolicy>,
        sandbox: Option<SandboxMode>,
    ) -> SessionInfo {
        self.client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                workspace_id: None,
                cwd: self.cwd(),
                agent: "codex".to_owned(),
                model: model.map(str::to_owned),
                approval_policy: policy,
                sandbox,
                title: None,
            }))
            .await
            .unwrap()
            .session
    }

    async fn send(&self, id: SessionId, text: &str) -> ChatSent {
        self.client
            .call(Request::ChatSend(ChatSend {
                session_id: id,
                text: text.to_owned(),
            }))
            .await
            .unwrap()
    }

    async fn history(&self, id: SessionId, before: Option<u64>, limit: u32) -> ChatHistory {
        self.client
            .call(Request::ChatHistory(ChatHistoryRequest {
                session_id: id,
                before_seq: before,
                limit,
            }))
            .await
            .unwrap()
    }

    async fn approvals(&self, client: &Client) -> Vec<ApprovalInfo> {
        client
            .call::<ApprovalList>(Request::ApprovalList(ApprovalListRequest { session_id: None }))
            .await
            .unwrap()
            .approvals
    }

    async fn session(&self, id: SessionId) -> SessionInfo {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .expect("session listed")
    }

    /// Polls until the session is in `state`.
    async fn wait_state(&self, id: SessionId, state: SessionState) -> SessionInfo {
        timeout(WAIT, async {
            loop {
                let session = self.session(id).await;
                if session.state == state {
                    return session;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("session never reached {state:?}"))
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn received_in(path: &Path) -> Vec<(String, Value)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let wrapper: Value = serde_json::from_str(line).unwrap();
            let msg = wrapper["msg"].clone();
            (msg["method"].as_str().unwrap_or("response").to_owned(), msg)
        })
        .collect()
}

async fn next_chat_event(
    events: &mut broadcast::Receiver<(Seq, Event)>,
    what: &str,
    mut pick: impl FnMut(&AgentEvent) -> bool,
) -> AgentEvent {
    wait_event(events, what, |event| match event {
        Event::ChatEvent { event, .. } if pick(event) => Some((**event).clone()),
        _ => None,
    })
    .await
}

fn is_turn_end(event: &AgentEvent) -> bool {
    event.kind.ends_turn()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_turn_streams_journals_and_returns_the_session_to_idle() {
    let mut t = Chat::with_fixture("plain-turn").await;
    let created = t.create().await;
    assert_eq!(created.kind, heeler_core::SessionKind::Harness);
    assert_eq!(created.state, SessionState::Idle);
    assert_eq!(created.terminal_id, None);
    let chat = created.chat.clone().expect("chat info");
    assert_eq!(chat.agent, "codex");
    assert_eq!(chat.model, "gpt-6.1-sol", "the catalog's default, not a config default");
    assert_eq!(chat.approval_policy, ApprovalPolicy::OnRequest);
    assert_eq!(chat.sandbox, SandboxMode::WorkspaceWrite);
    assert!(!chat.turn_active);

    // Workspace of the session: Home (the work dir is under no project).
    let sent = t.send(created.id, "Reply with the single word pong and nothing else.").await;

    let first = next_chat_event(&mut t.events, "turn_started", |e| {
        matches!(e.kind, AgentEventKind::TurnStarted)
    })
    .await;
    assert_eq!(first.turn_id, Some(sent.turn_id));
    assert_eq!(first.session_id, created.id);
    let mut seqs = vec![first.seq];
    let mut text = String::new();
    loop {
        let event = next_chat_event(&mut t.events, "the turn to end", |_| true).await;
        assert!(event.seq > *seqs.last().unwrap(), "seq increases per session");
        seqs.push(event.seq);
        assert_eq!(event.delivery, heeler_core::Delivery::Live);
        if let AgentEventKind::MessageDelta { text: delta } = &event.kind {
            text.push_str(delta);
        }
        if is_turn_end(&event) {
            assert!(matches!(event.kind, AgentEventKind::TurnCompleted { .. }));
            break;
        }
    }
    assert_eq!(text, "pong");
    let idle = t.wait_state(created.id, SessionState::Idle).await;
    assert!(!idle.chat.unwrap().turn_active);

    // The journal has everything but the deltas, oldest first, marked as replay.
    let history = t.history(created.id, None, 200).await;
    let kinds: Vec<_> = history.events.iter().map(|e| e.kind.name()).collect();
    assert!(!kinds.contains(&"message_delta"), "{kinds:?}");
    for expected in [
        "resume_locator",
        "turn_started",
        "message_start",
        "message_end",
        "usage",
        "turn_completed",
    ] {
        assert!(kinds.contains(&expected), "{expected} in {kinds:?}");
    }
    assert!(history.events.iter().all(|e| e.delivery == heeler_core::Delivery::Replay));
    let journal_seqs: Vec<u64> = history.events.iter().map(|e| e.seq).collect();
    assert!(journal_seqs.windows(2).all(|w| w[0] < w[1]), "{journal_seqs:?}");
    assert!(!history.has_more);
    assert_eq!(history.last_seq, *seqs.last().unwrap());
    assert!(
        journal_seqs.len() < seqs.len() + 1,
        "deltas used seqs the journal does not have"
    );
    let answer = history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageEnd { role: heeler_core::agent::MessageRole::Assistant, text, .. } => Some(text.as_str()),
            _ => None,
        })
        .last();
    assert_eq!(answer, Some("pong"));

    // Paging backwards: the newest two, then the two before them.
    let newest = t.history(created.id, None, 2).await;
    assert_eq!(newest.events.len(), 2);
    assert!(newest.has_more);
    assert_eq!(
        newest.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        journal_seqs[journal_seqs.len() - 2..]
    );
    let older = t.history(created.id, Some(newest.events[0].seq), 2).await;
    assert_eq!(
        older.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        journal_seqs[journal_seqs.len() - 4..journal_seqs.len() - 2]
    );

    // What the provider was asked for.
    let received = t.received();
    let start = received.iter().find(|(m, _)| m == "thread/start").unwrap();
    assert_eq!(start.1["params"]["approvalPolicy"], "on-request");
    assert_eq!(start.1["params"]["sandbox"], "workspace-write");
    assert_eq!(start.1["params"]["approvalsReviewer"], "user");
    let cwd = std::fs::canonicalize(t.work.path()).unwrap();
    assert_eq!(start.1["params"]["cwd"], cwd.to_str().unwrap());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_is_created_with_the_requested_model_policy_and_sandbox() {
    let t = Chat::with_fixture("plain-turn").await;
    let created = t
        .create_with(
            Some("gpt-5.6-luna"),
            Some(ApprovalPolicy::Untrusted),
            Some(SandboxMode::ReadOnly),
        )
        .await;
    let chat = created.chat.unwrap();
    assert_eq!(
        (chat.model.as_str(), chat.approval_policy, chat.sandbox),
        ("gpt-5.6-luna", ApprovalPolicy::Untrusted, SandboxMode::ReadOnly)
    );
    let start = t.received().into_iter().find(|(m, _)| m == "thread/start").unwrap();
    assert_eq!(start.1["params"]["model"], "gpt-5.6-luna");
    assert_eq!(start.1["params"]["approvalPolicy"], "untrusted");
    assert_eq!(start.1["params"]["sandbox"], "read-only");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_model_is_refused_with_the_catalog() {
    let t = Chat::with_fixture("plain-turn").await;
    let error = t
        .client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: t.cwd(),
            agent: "codex".to_owned(),
            model: Some("gpt-nope".to_owned()),
            approval_policy: None,
            sandbox: None,
            title: None,
        }))
        .await
        .unwrap_err();
    let heeler_client::ClientError::Remote(error) = error else {
        panic!("{error:?}")
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(error.message.contains("gpt-nope") && error.message.contains("gpt-6.1-sol"), "{}", error.message);
    // The session exists, failed, so the attempt is visible.
    let sessions = t
        .client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions;
    assert!(matches!(sessions[0].state, SessionState::Failed { .. }), "{sessions:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_are_pending_in_the_daemon_and_answered_only_with_offered_options() {
    let mut t = Chat::with_fixture("approval-touch").await;
    let created = t
        .create_with(None, Some(ApprovalPolicy::Untrusted), Some(SandboxMode::ReadOnly))
        .await;
    t.send(created.id, "Run `touch hi.txt` in a shell, then reply with the word done.").await;

    let info = wait_event(&mut t.events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;
    assert_eq!(info.session_id, created.id);
    assert!(info.title.contains("touch hi.txt"), "{}", info.title);
    let offered: Vec<_> = info.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(offered, ["accept", "acceptWithExecpolicyAmendment", "cancel"]);

    // The session says it waits; the approval is listed.
    let waiting = t.wait_state(created.id, SessionState::WaitingApproval).await;
    assert_eq!(waiting.chat.unwrap().pending_approvals, 1);
    assert_eq!(t.approvals(&t.client).await, [info.clone()]);

    // A second client sees it too; the first one going away changes nothing.
    let other = t.daemon.connect().await;
    let (reconnected, events) = t.daemon.connect_subscribed().await;
    drop(std::mem::replace(&mut t.client, reconnected));
    t.events = events;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(t.approvals(&other).await, [info.clone()], "survives a client disconnect");
    assert_eq!(t.approvals(&t.client).await.len(), 1);

    // Only offered options can be chosen; nothing reaches the provider otherwise.
    for bad in ["decline", "acceptForSession", "", "ACCEPT"] {
        let result = t
            .client
            .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
                approval_id: info.id,
                option_id: bad.to_owned(),
            }))
            .await;
        assert_eq!(remote_code(result), ErrorCode::OptionNotOffered, "{bad:?}");
    }
    let unknown = t
        .client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: ApprovalId::new(),
            option_id: "accept".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    assert!(t.received().iter().all(|(m, _)| m != "response"));
    assert_eq!(t.approvals(&t.client).await.len(), 1, "refusals leave it pending");

    // Answered by the second client; everyone is told.
    let mut other_events = {
        let events = other.events();
        other
            .call::<heeler_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
            .await
            .unwrap();
        events
    };
    other
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    let outcome = wait_event(&mut t.events, "approval.resolved", |event| match event {
        Event::ApprovalResolved { approval_id, outcome, .. } if *approval_id == info.id => {
            Some(outcome.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(outcome, PermissionOutcome::Selected { option_id: "accept".into() });
    wait_event(&mut other_events, "approval.resolved on the other client", |event| {
        matches!(event, Event::ApprovalResolved { .. }).then_some(())
    })
    .await;

    let mut done = false;
    let mut reply = String::new();
    while !done {
        let event = next_chat_event(&mut t.events, "the turn to end", |_| true).await;
        if let AgentEventKind::MessageDelta { text } = &event.kind {
            reply.push_str(text);
        }
        done = is_turn_end(&event);
    }
    assert!(reply.ends_with("done"), "{reply}");
    t.wait_state(created.id, SessionState::Idle).await;
    assert!(t.approvals(&t.client).await.is_empty());

    // The provider received exactly the chosen decision, for its own request.
    let received = t.received();
    let response = received.iter().find(|(m, _)| m == "response").unwrap();
    assert_eq!(response.1["result"]["decision"], "accept");
    assert_eq!(received.iter().filter(|(m, _)| m == "response").count(), 1);

    // Answering again is too late.
    let again = t
        .client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(again), ErrorCode::NotFound);

    // The audit trail: the request and who decided what.
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let row: (String, String, String, Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT kind, title, session_id, outcome, option_id, decided_by FROM approvals WHERE id = ?1",
            [info.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    assert_eq!(row.0, "command");
    assert!(row.1.contains("touch hi.txt"));
    assert_eq!(row.2, created.id.to_string());
    assert_eq!(
        (row.3.as_deref(), row.4.as_deref(), row.5.as_deref()),
        (Some("selected"), Some("accept"), Some("cli"))
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_approval_expires_when_its_turn_ends() {
    let mut t = Chat::with_fixture("approval-interrupt").await;
    let created = t.create().await;
    t.send(created.id, "Run `touch hi.txt`").await;
    let info = wait_event(&mut t.events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;
    t.wait_state(created.id, SessionState::WaitingApproval).await;

    t.client
        .call::<Empty>(Request::ChatInterrupt(SessionRef { session_id: created.id }))
        .await
        .unwrap();
    let outcome = wait_event(&mut t.events, "approval.resolved", |event| match event {
        Event::ApprovalResolved { approval_id, outcome, .. } if *approval_id == info.id => {
            Some(outcome.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(outcome, PermissionOutcome::Expired);
    next_chat_event(&mut t.events, "turn_cancelled", |e| {
        matches!(e.kind, AgentEventKind::TurnCancelled)
    })
    .await;
    t.wait_state(created.id, SessionState::Idle).await;
    assert!(t.approvals(&t.client).await.is_empty());

    let late = t
        .client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    assert!(t.received().iter().all(|(m, _)| m != "response"), "nothing was answered");

    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let (outcome, decided_by): (String, Option<String>) = db
        .query_row(
            "SELECT outcome, decided_by FROM approvals WHERE id = ?1",
            [info.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((outcome.as_str(), decided_by), ("expired", None));
    t.stop().await;
}

/// A directory of transcripts for a daemon that will spawn the agent several times.
fn script(dir: &Path, names: &[&str]) -> PathBuf {
    let scripted = dir.join("script");
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, name) in names.iter().enumerate() {
        std::fs::copy(
            fixtures().join(format!("{name}.jsonl")),
            scripted.join(format!("{:02}-{name}.jsonl", index + 1)),
        )
        .unwrap();
    }
    scripted
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_restart_resumes_the_agent_and_keeps_the_history() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["plain-turn", "resume-turn"]);
    let received = work.path().join("received.jsonl");
    let config = replay(&scripted, &received);
    let daemon = TestDaemon::start_with("", {
        let config = config.clone();
        move |c| c.codex = Some(config)
    })
    .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let created = chat.create().await;
    chat.send(created.id, "ping").await;
    next_chat_event(&mut events, "the turn to end", is_turn_end).await;
    let before = chat.history(created.id, None, 500).await;
    let thread_id = before
        .events
        .iter()
        .find_map(|e| match &e.kind {
            AgentEventKind::ResumeLocator { thread_id, .. } => Some(thread_id.clone()),
            _ => None,
        })
        .expect("a resume locator in the journal");
    assert!(!before.events.is_empty());

    // The daemon restarts: the agent comes back by resuming the stored thread.
    let Chat { daemon, work, .. } = chat;
    let daemon = daemon
        .restart_with({
            let config = config.clone();
            move |c| c.codex = Some(config)
        })
        .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let restored = chat.wait_state(created.id, SessionState::Idle).await;
    assert_eq!(restored.id, created.id, "the same session");
    assert!(!restored.restored, "the journal has the whole conversation");
    assert_eq!(restored.chat.unwrap().model, "gpt-6.1-sol");

    let spawns: Vec<_> = chat.received().into_iter().filter(|(m, _)| m == "thread/resume").collect();
    assert_eq!(spawns.len(), 1, "{:?}", chat.received());
    assert_eq!(spawns[0].1["params"]["threadId"], thread_id.as_str());
    assert_eq!(spawns[0].1["params"]["excludeTurns"], true);
    assert_eq!(spawns[0].1["params"]["approvalPolicy"], "on-request");

    // History is Heeler's own: unchanged by the resume, with the resumed run appended.
    let after = chat.history(created.id, None, 500).await;
    assert_eq!(&after.events[..before.events.len()], &before.events[..]);
    let last_before = before.events.last().unwrap().seq;
    chat.send(created.id, "again").await;
    next_chat_event(&mut events, "the resumed turn to end", is_turn_end).await;
    let all = chat.history(created.id, None, 500).await;
    let resumed_seqs: Vec<u64> = all
        .events
        .iter()
        .map(|e| e.seq)
        .filter(|seq| *seq > last_before)
        .collect();
    assert!(resumed_seqs.len() >= 4, "{resumed_seqs:?}");
    assert!(all.events.windows(2).all(|w| w[0].seq < w[1].seq), "seq never goes back");
    let answers: Vec<_> = all
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageEnd { role: heeler_core::agent::MessageRole::Assistant, text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answers, ["pong", "again"]);
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_pending_at_a_daemon_restart_are_expired() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["approval-touch", "resume-turn"]);
    let config = replay(&scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", {
        let config = config.clone();
        move |c| c.codex = Some(config)
    })
    .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let created = chat.create().await;
    chat.send(created.id, "go").await;
    let info = wait_event(&mut events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;

    let Chat { daemon, work, .. } = chat;
    let daemon = daemon
        .restart_with({
            let config = config.clone();
            move |c| c.codex = Some(config)
        })
        .await;
    let (client, events) = daemon.connect_subscribed().await;
    let chat = Chat { daemon, work, client, events };
    chat.wait_state(created.id, SessionState::Idle).await;
    assert!(chat.approvals(&chat.client).await.is_empty());
    let late = chat
        .client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    let db = rusqlite::Connection::open(&chat.daemon.paths.database).unwrap();
    let outcome: String = db
        .query_row("SELECT outcome FROM approvals WHERE id = ?1", [info.id.to_string()], |r| r.get(0))
        .unwrap();
    assert_eq!(outcome, "expired");
    // The journal shows the interrupted turn ended (the agent went down mid-turn).
    let history = chat.history(created.id, None, 500).await;
    let kinds: Vec<_> = history.events.iter().map(|e| e.kind.name()).collect();
    assert!(kinds.contains(&"permission_requested"), "{kinds:?}");
    assert!(
        kinds.contains(&"permission_resolved")
            && (kinds.contains(&"turn_cancelled") || kinds.contains(&"turn_failed")),
        "{kinds:?}"
    );
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_chat_ends_its_agent_and_restarting_resumes_it() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["plain-turn", "resume-turn"]);
    let config = replay(&scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, events) = daemon.connect_subscribed().await;
    let chat = Chat { daemon, work, client, events };
    let created = chat.create().await;

    let stopped = stop(&chat.client, created.id).await.unwrap();
    assert_eq!(stopped.state, SessionState::Stopped);
    let again = stop(&chat.client, created.id).await;
    assert_eq!(remote_code(again), ErrorCode::InvalidState);
    let blocked = chat
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend { session_id: created.id, text: "hi".into() }))
        .await;
    assert_eq!(remote_code(blocked), ErrorCode::InvalidState);

    let restarted = chat
        .client
        .call::<SessionResult>(Request::SessionRestart(SessionRef { session_id: created.id }))
        .await
        .unwrap()
        .session;
    assert_eq!(restarted.id, created.id);
    chat.wait_state(created.id, SessionState::Idle).await;
    let resumed = chat.received().into_iter().filter(|(m, _)| m == "thread/resume").count();
    assert_eq!(resumed, 1);
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_is_validated() {
    let mut t = Chat::with_fixture("interrupt").await;
    let created = t.create().await;
    let empty = t
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend { session_id: created.id, text: "  \n".into() }))
        .await;
    assert_eq!(remote_code(empty), ErrorCode::BadRequest);
    let unknown = t
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend { session_id: SessionId::new(), text: "x".into() }))
        .await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    let terminal = create(&t.client, spec(&["cat"])).await;
    let not_chat = t
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend { session_id: terminal.id, text: "x".into() }))
        .await;
    assert_eq!(remote_code(not_chat), ErrorCode::BadRequest);
    let no_turn = t
        .client
        .call::<Empty>(Request::ChatInterrupt(SessionRef { session_id: created.id }))
        .await;
    assert_eq!(remote_code(no_turn), ErrorCode::InvalidState);

    // One turn at a time.
    t.send(created.id, "Write the numbers from 1 to 300").await;
    let second = t
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend { session_id: created.id, text: "more".into() }))
        .await;
    assert_eq!(remote_code(second), ErrorCode::InvalidState);
    t.client
        .call::<Empty>(Request::ChatInterrupt(SessionRef { session_id: created.id }))
        .await
        .unwrap();
    next_chat_event(&mut t.events, "turn_cancelled", |e| {
        matches!(e.kind, AgentEventKind::TurnCancelled)
    })
    .await;
    t.wait_state(created.id, SessionState::Idle).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_chat_validates_the_request() {
    let t = Chat::with_fixture("plain-turn").await;
    let create = |agent: &str, cwd: PathBuf| {
        let client = t.client.clone();
        let agent = agent.to_owned();
        async move {
            client
                .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                    workspace_id: None,
                    cwd,
                    agent,
                    model: None,
                    approval_policy: None,
                    sandbox: None,
                    title: None,
                }))
                .await
        }
    };
    assert_eq!(remote_code(create("gemini", t.cwd()).await), ErrorCode::BadRequest);
    assert_eq!(remote_code(create("codex", t.cwd().join("missing")).await), ErrorCode::BadRequest);
    assert_eq!(remote_code(create("codex", "relative/dir".into()).await), ErrorCode::BadRequest);
    let history = t
        .client
        .call::<ChatHistory>(Request::ChatHistory(ChatHistoryRequest {
            session_id: SessionId::new(),
            before_seq: None,
            limit: 10,
        }))
        .await;
    assert_eq!(remote_code(history), ErrorCode::NotFound);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsupported_codex_version_fails_the_session_with_a_clear_reason() {
    let work = tempfile::tempdir().unwrap();
    let transcript = work.path().join("old.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}"#,
            "\n",
            r#"{"dir":"in","msg":{"id":1,"result":{"userAgent":"heeler/0.144.2 (Mac OS; arm64)"}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let config = replay(&transcript, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, _events) = daemon.connect_subscribed().await;
    let chat = Chat { daemon, work, client, events: _events };
    let result = chat
        .client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: chat.cwd(),
            agent: "codex".into(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
        }))
        .await;
    let heeler_client::ClientError::Remote(error) = result.unwrap_err() else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(
        error.message.contains("0.144.2") && error.message.contains(">=0.160, <0.170"),
        "{}",
        error.message
    );
    let sessions = chat
        .client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions;
    let SessionState::Failed { reason } = &sessions[0].state else {
        panic!("{sessions:?}")
    };
    assert!(reason.contains("0.144.2"), "{reason}");
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_codex_says_where_to_configure_it() {
    let daemon = TestDaemon::start_with("", |c| {
        c.codex = Some(CodexConfig {
            program: "/nonexistent/heeler-test-codex".into(),
            ..CodexConfig::default()
        });
    })
    .await;
    let client = daemon.connect().await;
    let cwd = tempfile::tempdir().unwrap();
    let result = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: cwd.path().to_owned(),
            agent: "codex".into(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
        }))
        .await;
    let heeler_client::ClientError::Remote(error) = result.unwrap_err() else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(
        error.message.contains("/nonexistent/heeler-test-codex") && error.message.contains("daemon.toml"),
        "{}",
        error.message
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn models_come_from_the_agents_catalog() {
    let t = Chat::with_fixture("plain-turn").await;
    let models = t
        .client
        .call::<ChatModels>(Request::ChatModels(ChatModelsRequest { agent: "codex".into() }))
        .await
        .unwrap();
    assert_eq!(models.agent, "codex");
    let default: Vec<_> = models.models.iter().filter(|m| m.is_default).map(|m| m.id.as_str()).collect();
    assert_eq!(default, ["gpt-6.1-sol"]);
    assert!(models.models.iter().any(|m| m.id == "gpt-5.6-luna"));
    let unsupported = t
        .client
        .call::<ChatModels>(Request::ChatModels(ChatModelsRequest { agent: "gemini".into() }))
        .await;
    assert_eq!(remote_code(unsupported), ErrorCode::BadRequest);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_dies_mid_turn_fails_the_session_and_ends_the_turn() {
    let work = tempfile::tempdir().unwrap();
    let real = std::fs::read_to_string(fixtures().join("plain-turn.jsonl")).unwrap();
    let mut lines: Vec<String> = Vec::new();
    for line in real.lines() {
        lines.push(line.to_owned());
        if line.contains("\"item/agentMessage/delta\"") {
            break;
        }
    }
    lines.push(r#"{"dir":"exit","code":3}"#.to_owned());
    let transcript = work.path().join("dies.jsonl");
    std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
    let config = replay(&transcript, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, events) = daemon.connect_subscribed().await;
    let mut t = Chat { daemon, work, client, events };
    let created = t.create().await;
    t.send(created.id, "ping").await;
    let failed = next_chat_event(&mut t.events, "turn_failed", |e| {
        matches!(e.kind, AgentEventKind::TurnFailed { .. })
    })
    .await;
    let AgentEventKind::TurnFailed { message, .. } = failed.kind else {
        unreachable!()
    };
    assert!(message.contains("exited during the turn"), "{message}");
    let session = t
        .wait_state(
            created.id,
            SessionState::Failed {
                reason: "the agent exited with code 3".to_owned(),
            },
        )
        .await;
    assert!(!session.chat.unwrap().turn_active);
    t.stop().await;
}
