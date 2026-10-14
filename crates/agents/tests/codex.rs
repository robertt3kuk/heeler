//! The Codex adapter against the fake app server (`heeler-codex-fixture replay`) replaying
//! transcripts recorded from the real codex 0.162.1.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use heeler_agents::{
    AdapterMessage, AgentAdapter, AgentError, CodexAppServerAdapter, CodexConfig, CodexSession,
    StartSpec,
};
use heeler_core::agent::{
    FileChangeKind, MessageRole, PermissionKind, PermissionOptionKind, PermissionOutcome, ToolKind,
    ToolStatus,
};
use heeler_core::{AgentEventDraft, AgentEventKind, ApprovalId, ApprovalPolicy, SandboxMode};
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::timeout;

const FIXTURE_BIN: &str = env!("CARGO_BIN_EXE_heeler-codex-fixture");
const WAIT: Duration = Duration::from_secs(20);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex-0.162")
        .join(format!("{name}.jsonl"))
}

struct Harness {
    dir: TempDir,
    received: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let received = dir.path().join("received.jsonl");
        Self { dir, received }
    }

    fn config(&self, transcript: &Path) -> CodexConfig {
        CodexConfig {
            program: PathBuf::from(FIXTURE_BIN),
            args: vec![
                "replay".to_owned(),
                "--received".to_owned(),
                self.received.display().to_string(),
                transcript.display().to_string(),
                "app-server".to_owned(),
            ],
            env: Vec::new(),
        }
    }

    fn spec(&self, resume: Option<&str>) -> StartSpec {
        StartSpec {
            cwd: self.dir.path().to_owned(),
            model: None,
            approval_policy: ApprovalPolicy::Untrusted,
            sandbox: SandboxMode::ReadOnly,
            resume_thread_id: resume.map(str::to_owned),
        }
    }

    async fn start(&self, transcript: &str) -> CodexSession {
        self.start_with(&fixture(transcript), None).await
    }

    async fn start_with(&self, transcript: &Path, resume: Option<&str>) -> CodexSession {
        timeout(
            WAIT,
            CodexAppServerAdapter::start(&self.config(transcript), &self.spec(resume)),
        )
        .await
        .expect("start timed out")
        .expect("start failed")
    }

    /// What the fake server received, as `(method or "response", message)`.
    fn received(&self) -> Vec<(String, Value)> {
        std::fs::read_to_string(&self.received)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let wrapper: Value = serde_json::from_str(line).unwrap();
                let msg = wrapper["msg"].clone();
                let name = msg["method"].as_str().unwrap_or("response").to_owned();
                (name, msg)
            })
            .collect()
    }
}

/// Reads messages until `stop` says so (inclusive); returns them all.
async fn collect_until(
    events: &mut tokio::sync::mpsc::Receiver<AdapterMessage>,
    mut stop: impl FnMut(&AdapterMessage) -> bool,
) -> Vec<AdapterMessage> {
    let mut seen = Vec::new();
    loop {
        let message = timeout(WAIT, events.recv())
            .await
            .expect("timed out waiting for an event")
            .expect("event stream ended");
        let done = stop(&message);
        seen.push(message);
        if done {
            return seen;
        }
    }
}

fn kinds(messages: &[AdapterMessage]) -> Vec<&AgentEventKind> {
    messages
        .iter()
        .filter_map(|m| match m {
            AdapterMessage::Event(draft) => Some(&draft.kind),
            AdapterMessage::Exited(_) | AdapterMessage::Node(_) => None,
        })
        .collect()
}

fn names(messages: &[AdapterMessage]) -> Vec<&'static str> {
    kinds(messages)
        .into_iter()
        .map(AgentEventKind::name)
        .collect()
}

fn is_permission_request(message: &AdapterMessage) -> bool {
    matches!(message, AdapterMessage::Event(d) if matches!(d.kind, AgentEventKind::PermissionRequested { .. }))
}

fn ends_turn(message: &AdapterMessage) -> bool {
    matches!(message, AdapterMessage::Event(d) if d.kind.ends_turn())
}

fn requested(messages: &[AdapterMessage]) -> (ApprovalId, &AgentEventDraft) {
    messages
        .iter()
        .find_map(|m| match m {
            AdapterMessage::Event(draft) => match &draft.kind {
                AgentEventKind::PermissionRequested { approval_id, .. } => {
                    Some((*approval_id, draft.as_ref()))
                }
                _ => None,
            },
            AdapterMessage::Exited(_) | AdapterMessage::Node(_) => None,
        })
        .expect("a permission request")
}

#[tokio::test]
async fn a_command_turn_with_an_approval_maps_to_the_unified_events() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        started,
        mut events,
    } = h.start("approval-touch").await;
    assert_eq!(started.model, "gpt-6.1-sol", "the catalog's default model");
    assert!(!started.resumed);
    assert_eq!(adapter.agent(), "codex");

    let turn = adapter
        .send_message("Run `touch hi.txt` in a shell, then reply with the word done.".into())
        .await
        .unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, draft) = requested(&first);
    let AgentEventKind::PermissionRequested {
        kind,
        title,
        detail,
        options,
        ..
    } = &draft.kind
    else {
        unreachable!()
    };
    assert_eq!(*kind, PermissionKind::Command);
    assert!(title.contains("touch hi.txt"), "{title}");
    assert!(
        detail
            .as_deref()
            .unwrap_or("")
            .contains("/tmp/heeler-fixture")
    );
    // Exactly what the server offered, in its order.
    let offered: Vec<_> = options.iter().map(|o| (o.id.as_str(), o.kind)).collect();
    assert_eq!(
        offered,
        [
            ("accept", PermissionOptionKind::AllowOnce),
            (
                "acceptWithExecpolicyAmendment",
                PermissionOptionKind::AllowAlways
            ),
            ("cancel", PermissionOptionKind::Cancel),
        ]
    );
    assert_eq!(options[0].provider, Some(Value::String("accept".into())));
    assert!(
        options[1].label.contains("touch hi.txt"),
        "{}",
        options[1].label
    );
    assert_eq!(draft.turn_id, Some(turn.turn_id), "local turn id is stable");
    assert_eq!(
        draft.provider.turn_id.as_deref(),
        Some(turn.provider_turn_id.as_str())
    );
    assert!(draft.provider.request_id.is_some());

    // The first part of the turn: user message echo, commentary, tool start.
    let before = names(&first);
    assert_eq!(
        &before[..4],
        [
            "resume_locator",
            "turn_started",
            "message_start",
            "message_end"
        ]
    );
    assert!(
        before.contains(&"tool_start") && before.contains(&"message_delta"),
        "{before:?}"
    );

    adapter.respond(approval, "accept".into()).await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    let after = names(&rest);
    assert_eq!(after.first(), Some(&"permission_resolved"), "{after:?}");
    assert_eq!(after.last(), Some(&"turn_completed"), "{after:?}");
    for expected in ["tool_end", "message_end", "usage"] {
        assert!(after.contains(&expected), "{expected} in {after:?}");
    }
    let resolved = kinds(&rest)[0];
    assert_eq!(
        *resolved,
        AgentEventKind::PermissionResolved {
            approval_id: approval,
            outcome: PermissionOutcome::Selected {
                option_id: "accept".into()
            }
        }
    );
    let tool_end = kinds(&rest)
        .into_iter()
        .find(|k| matches!(k, AgentEventKind::ToolEnd { .. }))
        .unwrap();
    assert!(matches!(
        tool_end,
        AgentEventKind::ToolEnd {
            tool: ToolKind::Command,
            status: ToolStatus::Completed,
            ..
        }
    ));
    let final_text = kinds(&rest)
        .into_iter()
        .filter_map(|k| match k {
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .next_back();
    assert_eq!(final_text, Some("done"));

    // The provider got the decision it was offered, answering its own request id.
    let received = h.received();
    let response = received
        .iter()
        .find(|(name, _)| name == "response")
        .unwrap();
    assert_eq!(response.1["id"], 0);
    assert_eq!(response.1["result"]["decision"], "accept");
    let methods: Vec<_> = received.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "model/list",
            "thread/start",
            "turn/start",
            "response"
        ]
    );
    let start = &received[3].1["params"];
    assert_eq!(start["approvalPolicy"], "untrusted");
    assert_eq!(start["sandbox"], "read-only");
    assert_eq!(start["approvalsReviewer"], "user");
    assert_eq!(start["model"], "gpt-6.1-sol");
    adapter.shutdown().await;
}

#[tokio::test]
async fn only_offered_options_are_sent_to_the_provider() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("approval-touch").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, _) = requested(&first);

    // `decline` exists in Codex's schema but this request did not offer it.
    for option in ["decline", "acceptForSession", "nonsense", ""] {
        let error = adapter.respond(approval, option.into()).await.unwrap_err();
        assert!(
            matches!(&error, AgentError::OptionNotOffered { option: o } if o == option),
            "{option}: {error}"
        );
    }
    let error = adapter
        .respond(ApprovalId::new(), "accept".into())
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownApproval), "{error}");
    assert!(
        h.received().iter().all(|(name, _)| name != "response"),
        "nothing reached the provider"
    );

    adapter.respond(approval, "accept".into()).await.unwrap();
    collect_until(&mut events, ends_turn).await;
    adapter.shutdown().await;
}

#[tokio::test]
async fn an_answered_request_cannot_be_answered_twice() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("approval-touch").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, _) = requested(&first);
    adapter.respond(approval, "accept".into()).await.unwrap();
    let error = adapter
        .respond(approval, "accept".into())
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownApproval), "{error}");
    collect_until(&mut events, ends_turn).await;
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_declined_command_ends_as_a_declined_tool() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("approval-decline").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, _) = requested(&first);
    // The recording answered `decline`; the server offered only accept/cancel, so Heeler
    // answers `cancel` and the replay (which matches responses loosely) carries on.
    adapter.respond(approval, "cancel".into()).await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert!(rest.iter().any(|m| matches!(
        m,
        AdapterMessage::Event(d) if matches!(
            d.kind,
            AgentEventKind::ToolEnd { status: ToolStatus::Declined, .. }
        )
    )));
    let response = h
        .received()
        .into_iter()
        .find(|(name, _)| name == "response")
        .unwrap();
    assert_eq!(response.1["result"]["decision"], "cancel");
    adapter.shutdown().await;
}

#[tokio::test]
async fn file_changes_carry_per_file_diffs_and_default_options() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("file-change").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, draft) = requested(&first);
    let AgentEventKind::PermissionRequested { kind, options, .. } = &draft.kind else {
        unreachable!()
    };
    assert_eq!(*kind, PermissionKind::FileChange);
    // The request listed no `availableDecisions`: the schema's decisions are the offer.
    let ids: Vec<_> = options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["accept", "acceptForSession", "decline", "cancel"]);
    let diff = kinds(&first)
        .into_iter()
        .find_map(|k| match k {
            AgentEventKind::FileDiff { changes, .. } => changes.first(),
            _ => None,
        })
        .expect("a file diff before the approval");
    assert_eq!(diff.kind, FileChangeKind::Add);
    assert!(diff.path.ends_with("hello.txt"), "{}", diff.path);
    assert_eq!(diff.patch.as_deref(), Some("hi\n"));

    adapter.respond(approval, "accept".into()).await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    let after = names(&rest);
    assert!(
        after.contains(&"file_diff") && after.contains(&"tool_end"),
        "{after:?}"
    );
    // The aggregate turn diff arrives as a patch.
    assert!(
        kinds(&rest).into_iter().any(|k| matches!(
            k,
            AgentEventKind::FileDiff { patch: Some(p), changes } if p.contains("+hi") && changes.is_empty()
        )),
        "{after:?}"
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn interrupting_a_streaming_turn_cancels_it() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("interrupt").await;
    adapter
        .send_message("Write the numbers from 1 to 300".into())
        .await
        .unwrap();
    let mut deltas = 0;
    let streamed = collect_until(&mut events, |m| {
        deltas += usize::from(matches!(
            m,
            AdapterMessage::Event(d) if matches!(d.kind, AgentEventKind::MessageDelta { .. })
        ));
        deltas == 4
    })
    .await;
    assert!(names(&streamed).contains(&"message_start"));
    adapter.interrupt().await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert_eq!(names(&rest).last(), Some(&"turn_cancelled"));
    let methods: Vec<_> = h.received().into_iter().map(|(m, _)| m).collect();
    assert_eq!(methods.last().map(String::as_str), Some("turn/interrupt"));
    let interrupt = h.received().pop().unwrap().1;
    assert!(interrupt["params"]["turnId"].as_str().is_some());
    adapter.shutdown().await;
}

#[tokio::test]
async fn interrupting_without_a_turn_is_an_error() {
    let h = Harness::new();
    let session = h.start("interrupt").await;
    let error = session.adapter.interrupt().await.unwrap_err();
    assert!(matches!(error, AgentError::NoTurn), "{error}");
    session.adapter.shutdown().await;
}

#[tokio::test]
async fn a_pending_approval_expires_when_the_turn_is_interrupted() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("approval-interrupt").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, _) = requested(&first);
    adapter.interrupt().await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    let kinds = kinds(&rest);
    // The resolution comes before the turn's end.
    assert_eq!(
        kinds[kinds.len() - 2],
        &AgentEventKind::PermissionResolved {
            approval_id: approval,
            outcome: PermissionOutcome::Expired
        }
    );
    assert_eq!(kinds[kinds.len() - 1], &AgentEventKind::TurnCancelled);
    // Too late to answer.
    let error = adapter
        .respond(approval, "accept".into())
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownApproval), "{error}");
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_failed_turn_reports_the_providers_message() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("turn-failed").await;
    adapter.send_message("hi".into()).await.unwrap();
    let all = collect_until(&mut events, ends_turn).await;
    let failed = kinds(&all).pop().unwrap();
    let AgentEventKind::TurnFailed { message, .. } = failed else {
        panic!("{failed:?}")
    };
    assert!(
        message.contains("is not supported when using Codex"),
        "{message}"
    );
    // The provider's own error notification is kept as a notice.
    assert!(names(&all).contains(&"notice"), "{:?}", names(&all));
    adapter.shutdown().await;
}

#[tokio::test]
async fn resuming_reopens_the_stored_thread_and_continues() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        started,
        mut events,
    } = h
        .start_with(&fixture("resume-turn"), Some("thread-from-the-journal"))
        .await;
    assert!(started.resumed);
    assert_eq!(started.thread_id, "thread-from-the-journal");

    let turn = adapter.send_message("again".into()).await.unwrap();
    let all = collect_until(&mut events, ends_turn).await;
    // Events of the resumed thread (whose recorded id differs) are not dropped as foreign.
    let completed = kinds(&all).pop().unwrap();
    assert!(
        matches!(completed, AgentEventKind::TurnCompleted { .. }),
        "{completed:?}"
    );
    let locator = kinds(&all)[0];
    assert_eq!(
        *locator,
        AgentEventKind::ResumeLocator {
            agent: "codex".into(),
            thread_id: "thread-from-the-journal".into(),
            model: Some("gpt-6.1-sol".into())
        }
    );
    assert!(turn.provider_turn_id.len() > 8);

    let received = h.received();
    let methods: Vec<_> = received.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "thread/resume", "turn/start"]
    );
    let resume = &received[2].1["params"];
    assert_eq!(resume["threadId"], "thread-from-the-journal");
    assert_eq!(
        resume["excludeTurns"], true,
        "history comes from our journal"
    );
    assert_eq!(
        received[3].1["params"]["threadId"],
        "thread-from-the-journal"
    );
    adapter.shutdown().await;
}

fn write_transcript(h: &Harness, name: &str, lines: &[Value]) -> PathBuf {
    let path = h.dir.path().join(name);
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&path, text).unwrap();
    path
}

fn handshake_lines(user_agent: &str) -> Vec<Value> {
    use serde_json::json;
    vec![
        json!({"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}),
        json!({"dir":"in","msg":{"id":1,"result":{"userAgent":user_agent}}}),
        json!({"dir":"out","msg":{"method":"initialized"}}),
    ]
}

#[tokio::test]
async fn unsupported_versions_are_refused_with_a_clear_error() {
    for (agent, found) in [
        ("heeler/0.144.2 (Mac OS; arm64)", "0.144.2"),
        ("heeler/0.170.0 (Mac OS; arm64)", "0.170.0"),
    ] {
        let h = Harness::new();
        let transcript = write_transcript(&h, "old.jsonl", &handshake_lines(agent));
        let error = timeout(
            WAIT,
            CodexAppServerAdapter::start(&h.config(&transcript), &h.spec(None)),
        )
        .await
        .unwrap()
        .unwrap_err();
        let text = error.to_string();
        assert!(
            matches!(error, AgentError::UnsupportedVersion { .. }),
            "{text}"
        );
        assert!(
            text.contains(found) && text.contains(">=0.160, <0.170"),
            "{text}"
        );
        let methods: Vec<_> = h.received().into_iter().map(|(m, _)| m).collect();
        assert_eq!(methods, ["initialize"], "nothing past the version check");
    }
}

#[tokio::test]
async fn a_missing_program_is_a_spawn_error() {
    let h = Harness::new();
    let config = CodexConfig {
        program: PathBuf::from("/nonexistent/heeler-codex"),
        ..CodexConfig::default()
    };
    let error = CodexAppServerAdapter::start(&config, &h.spec(None))
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::Spawn { .. }), "{error}");
    assert!(error.to_string().contains("/nonexistent/heeler-codex"));
}

#[tokio::test]
async fn unknown_messages_become_opaque_events_and_never_crash_the_adapter() {
    use serde_json::json;
    let h = Harness::new();
    // The real handshake and thread start, then things this build has never heard of.
    let real = std::fs::read_to_string(fixture("approval-touch")).unwrap();
    let mut lines: Vec<Value> = Vec::new();
    for line in real.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        let is_turn_start = value["msg"]["method"] == "turn/start";
        if is_turn_start {
            break;
        }
        lines.push(value);
    }
    let thread = lines
        .iter()
        .find_map(|l| {
            l["msg"]["result"]["thread"]["id"]
                .as_str()
                .map(str::to_owned)
        })
        .unwrap();
    lines.extend([
        json!({"dir":"out","msg":{"method":"turn/start","id":4,"params":{}}}),
        json!({"dir":"in","msg":{"id":4,"result":{"turn":{"id":"turn-1"}}}}),
        json!({"dir":"in","msg":{"method":"future/thing","params":{"threadId":thread,"x":1}}}),
        json!({"dir":"in","msg":{"method":"turn/started","params":{"threadId":thread,"turn":"not an object"}}}),
        json!({"dir":"in","msg":{"method":"item/started","params":{"threadId":thread,"turnId":"turn-1","startedAtMs":1,"item":{"type":"futureItem","id":"i1","novel":true}}}}),
        json!({"dir":"in","msg":{"method":"item/started","params":{"threadId":"some-other-thread","turnId":"t9","item":{"type":"agentMessage","id":"x","text":""}}}}),
        json!({"dir":"in","msg":{"method":"item/tool/requestUserInput","id":77,"params":{"threadId":thread,"turnId":"turn-1","itemId":"q","questions":[]}}}),
        json!({"dir":"in","msg":{"method":"account/updated","params":{}}}),
        json!({"dir":"out","msg":{"id":77,"error":{"code":-32601,"message":"declined"}}}),
        json!({"dir":"exit","code":9}),
    ]);
    let transcript = write_transcript(&h, "odd.jsonl", &lines);
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start_with(&transcript, None).await;
    adapter.send_message("go".into()).await.unwrap();
    let all = collect_until(&mut events, |m| matches!(m, AdapterMessage::Exited(_))).await;

    let opaque: Vec<_> = all
        .iter()
        .filter_map(|m| match m {
            AdapterMessage::Event(d) => match &d.kind {
                AgentEventKind::Opaque { method } => Some((method.as_str(), d)),
                _ => None,
            },
            AdapterMessage::Exited(_) | AdapterMessage::Node(_) => None,
        })
        .collect();
    let methods: Vec<_> = opaque.iter().map(|(m, _)| *m).collect();
    assert_eq!(methods, ["future/thing", "turn/started", "item/started"]);
    assert_eq!(opaque[0].1.payload.as_ref().unwrap()["x"], 1);
    assert_eq!(opaque[2].1.payload.as_ref().unwrap()["novel"], true);
    assert_eq!(opaque[2].1.provider.item_id.as_deref(), Some("i1"));
    // A request Heeler cannot serve is declined (the agent must not wait for it) and noted.
    assert!(names(&all).contains(&"notice"));
    let refusal = h
        .received()
        .into_iter()
        .find(|(name, msg)| name == "response" && msg["id"] == 77)
        .expect("the unsupported request was answered");
    assert_eq!(refusal.1["error"]["code"], -32601);
    // Other threads' events and account chatter are dropped, not turned into noise.
    assert_eq!(opaque.len(), 3);
    // The process ended: the adapter says so, with the exit code.
    let Some(AdapterMessage::Exited(info)) = all.last() else {
        panic!("{all:?}")
    };
    assert_eq!(info.code, Some(9));
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_process_that_dies_expires_what_was_pending() {
    use serde_json::json;
    let h = Harness::new();
    let real = std::fs::read_to_string(fixture("approval-touch")).unwrap();
    let mut lines: Vec<Value> = Vec::new();
    for line in real.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        lines.push(value.clone());
        if value["msg"]["method"] == "item/commandExecution/requestApproval" {
            break;
        }
    }
    lines.push(json!({"dir":"exit","code":1}));
    let transcript = write_transcript(&h, "dies.jsonl", &lines);
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start_with(&transcript, None).await;
    adapter.send_message("go".into()).await.unwrap();
    let all = collect_until(&mut events, |m| matches!(m, AdapterMessage::Exited(_))).await;
    let names = names(&all);
    assert_eq!(names.last(), Some(&"permission_resolved"), "{names:?}");
    let Some(AdapterMessage::Exited(info)) = all.last() else {
        panic!("{all:?}")
    };
    assert_eq!(info.code, Some(1));
    let error = adapter
        .send_message("more".into())
        .await
        .expect_err("the process is gone");
    assert!(matches!(error, AgentError::Exited { .. }), "{error}");
}

#[test]
fn committed_fixtures_are_already_scrubbed() {
    use heeler_agents::fixture::scrub::scrub;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-0.162");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let scrubbed = scrub(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            scrubbed,
            text,
            "{} changes when scrubbed again",
            path.display()
        );
        count += 1;
    }
    assert!(count >= 7, "{count} fixtures");
}
